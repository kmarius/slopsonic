use super::*;

pub(super) enum DownloadFailure {
    Item(String),
    Retry(String),
    NeedsAttention(String),
}

pub(super) struct ActiveDownload {
    pub(super) source_id: Option<SourceId>,
    pub(super) job_id: String,
    pub(super) media_uri: String,
    pub(super) subject: DownloadSubject,
    pub(super) paths: DownloadPaths,
    pub(super) quality: DownloadQuality,
    pub(super) cancellation: Option<tokio::sync::oneshot::Sender<()>>,
    pub(super) task: tokio::task::JoinHandle<Result<(), DownloadFailure>>,
}

pub(super) async fn run(
    mut actor: Actor,
    receiver: Receiver<Command>,
    rule_results: Receiver<PreparedRules>,
) {
    if let Err(error) = migrate_download_records(&actor.root, &actor.database, None, None).await {
        warn!(%error, "could not restore direct download access");
    }
    match load_direct_queue(&actor.root, &actor.database).await {
        Ok(jobs) if !jobs.is_empty() => {
            actor.jobs.insert(None, jobs);
        }
        Ok(_) => {}
        Err(error) => warn!(%error, "could not load the direct download queue"),
    }
    actor.publish(None).await;
    let mut retry = tokio::time::interval(RETRY_INTERVAL);
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut active = Vec::new();
    loop {
        if receiver.is_closed() {
            actor.abort_matching(&mut active, true, |_| true).await;
            break;
        }
        if let Ok(command) = receiver.try_recv() {
            actor.apply(command, &mut active).await;
            continue;
        }
        if let Ok(prepared) = rule_results.try_recv() {
            actor.apply_prepared_rules(prepared, &mut active).await;
            continue;
        }
        actor.fill_slots(&mut active).await;
        if !active.is_empty() {
            tokio::select! {
                command = receiver.recv() => {
                    let Ok(command) = command else {
                        actor.abort_matching(&mut active, true, |_| true).await;
                        break;
                    };
                    actor.apply(command, &mut active).await;
                }
                Ok(prepared) = rule_results.recv() => {
                    actor.apply_prepared_rules(prepared, &mut active).await;
                }
                (index, result) = wait_for_finished(&mut active) => {
                    let current = active.swap_remove(index);
                    actor.finish(current, result, &mut active).await;
                }
                _ = retry.tick() => actor.retry_waiting(),
            }
            continue;
        }
        tokio::select! {
            command = receiver.recv() => {
                let Ok(command) = command else {
                    break;
                };
                actor.apply(command, &mut active).await;
            }
            Ok(prepared) = rule_results.recv() => {
                actor.apply_prepared_rules(prepared, &mut active).await;
            }
            _ = retry.tick() => {
                actor.retry_waiting();
            }
        }
    }
}

pub(super) async fn wait_for_finished(
    active: &mut [ActiveDownload],
) -> (
    usize,
    Result<Result<(), DownloadFailure>, tokio::task::JoinError>,
) {
    std::future::poll_fn(|context| {
        for (index, download) in active.iter_mut().enumerate() {
            if let Poll::Ready(result) = Pin::new(&mut download.task).poll(context) {
                return Poll::Ready((index, result));
            }
        }
        Poll::Pending
    })
    .await
}

impl Actor {
    pub(super) async fn apply(&mut self, command: Command, active: &mut Vec<ActiveDownload>) {
        match command {
            Command::ClearArtworkCache(response) => {
                let active_uris: Vec<_> = active
                    .iter()
                    .map(|download| download.media_uri.clone())
                    .collect();
                let result = self
                    .artwork
                    .clear_cache(&self.database, &active_uris)
                    .await
                    .map_err(|error| error.to_string());
                let _ = response.send(result).await;
            }
            Command::Page {
                scope,
                offset,
                limit,
                response,
            } => {
                let _ = response
                    .send(self.queue_page(scope, offset, limit).await)
                    .await;
            }
            Command::Attach {
                source_id,
                source_key,
                source,
                folder,
                response,
            } => {
                let directory = self.settings_for(&source_id).directory;
                let directory_changed = self
                    .attached
                    .get(&source_id)
                    .is_some_and(|attached| attached.directory != directory);
                let source_changed = self
                    .attached
                    .get(&source_id)
                    .is_some_and(|attached| !same_weak_target(&attached.source, &source));
                self.discard_matching(active, !directory_changed, |download| {
                    download.source_id.as_ref() == Some(&source_id)
                        && (source_changed || directory_changed)
                })
                .await;
                let result = self
                    .attach(source_id.clone(), source_key, source, folder, directory)
                    .await;
                let ready = result.is_ok();
                self.pending_rules = None;
                let _ = response.send(result).await;
                if ready {
                    self.reconcile_all_rules(&source_id);
                }
            }
            Command::Download {
                subject,
                media_uris,
            } => {
                let mut skipped = false;
                let mut grouped = HashMap::<Option<SourceId>, Vec<String>>::new();
                for media_uri in media_uris {
                    grouped
                        .entry(media_source_id(&self.database, &media_uri).await)
                        .or_default()
                        .push(media_uri);
                }
                for (source_id, media_uris) in grouped {
                    if source_id
                        .as_ref()
                        .is_some_and(|source_id| !self.attached.contains_key(source_id))
                    {
                        warn!(source_id=?source_id, "ignored a download for an unattached source");
                        skipped = true;
                        continue;
                    }
                    let quality = source_id
                        .as_ref()
                        .map_or(DownloadQuality::Original, |source_id| {
                            self.settings_for(source_id).quality
                        });
                    if let Some(feedback) = self
                        .enqueue(source_id, subject.clone(), quality, media_uris, false)
                        .await
                    {
                        let _ = self.events.send(DownloadEvent::Feedback(feedback)).await;
                    }
                }
                if skipped {
                    self.mark_subject_incomplete(&subject).await;
                } else {
                    self.publish_subject_complete(&subject);
                }
            }
            Command::ConnectChanged => {
                self.retry_waiting();
            }
            Command::Remove { media_uris, notify } => {
                let mut grouped = HashMap::<Option<SourceId>, Vec<String>>::new();
                for media_uri in media_uris {
                    grouped
                        .entry(media_source_id(&self.database, &media_uri).await)
                        .or_default()
                        .push(media_uri);
                }
                for (source_id, media_uris) in grouped {
                    let remove = media_uris.iter().collect::<HashSet<_>>();
                    self.abort_matching(active, false, |download| {
                        download.source_id == source_id && remove.contains(&download.media_uri)
                    })
                    .await;
                    self.force_remove(source_id.as_ref(), media_uris, notify)
                        .await;
                }
            }
            Command::LibraryChanged { source_id } => {
                self.reconcile_all_rules(&source_id);
            }
            Command::SettingsChanged(settings) => {
                self.apply_settings(settings, active).await;
            }
            Command::RemoveRule {
                source_id,
                rule,
                delete_downloads,
            } => {
                self.abort_matching(active, true, |download| {
                    download.source_id.as_ref() == Some(&source_id)
                        && download.subject == DownloadSubject::Rule(rule)
                })
                .await;
                self.remove_rule(&source_id, rule, delete_downloads).await;
            }
            Command::Cancel { source_id, job_id } => {
                self.cancel(source_id.as_ref(), &job_id, active).await;
            }
            Command::ClearJob { source_id, job_id } => {
                self.clear_job(&source_id, &job_id, active).await;
            }
            Command::Suspend { ready, resume } => {
                self.abort_matching(active, true, |_| true).await;
                if ready.send(()).await.is_ok() {
                    let _ = resume.recv().await;
                }
            }
            Command::SetPaused(paused) => {
                if self.paused == paused {
                    return;
                }
                self.paused = paused;
                if paused {
                    self.abort_matching(active, true, |_| true).await;
                }
                self.publish_all().await;
            }
            Command::Move {
                source_id,
                job_id,
                movement,
            } => {
                self.move_job(&source_id, &job_id, movement).await;
            }
            Command::Clear { source_id, notify } => {
                if let Some(settings) = self.settings.get_mut(&source_id) {
                    settings.rules = DownloadRules::default();
                }
                self.reconcile_all_rules(&source_id);
                self.abort_matching(active, false, |download| {
                    download.source_id.as_ref() == Some(&source_id)
                })
                .await;
                self.clear(&source_id, notify).await;
            }
        }
    }
}

pub(super) async fn cache_track_artwork(
    database: &Database,
    artwork: &artwork::Artwork,
    media_uri: &str,
) {
    let result = async {
        let bindings = database
            .track_artwork_bindings(&[media_uri.to_owned()], &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        for binding in bindings {
            artwork
                .cache_source(&binding)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok::<_, String>(())
    }
    .await;
    if let Err(error) = result {
        warn!(%error, %media_uri, "could not download accompanying media");
    }
}

pub(super) fn download_source_failure(error: SourceError) -> DownloadFailure {
    match error {
        SourceError::NotFound => DownloadFailure::Item(error.to_string()),
        SourceError::Server { status, .. } if status < 500 && status != 429 => {
            DownloadFailure::Item(error.to_string())
        }
        SourceError::Tls(_)
        | SourceError::Network(_)
        | SourceError::Server { .. }
        | SourceError::Cancelled => DownloadFailure::Retry(error.to_string()),
        SourceError::Auth(_)
        | SourceError::Library(_)
        | SourceError::Json(_)
        | SourceError::InvalidRequest(_)
        | SourceError::InvalidConfig(_)
        | SourceError::Other(_) => DownloadFailure::NeedsAttention(error.to_string()),
    }
}

pub(super) fn job_id(
    source_id: Option<&SourceId>,
    subject: &DownloadSubject,
    sequence: u64,
) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let input = serde_json::to_vec(&(source_id, subject, now, sequence)).unwrap_or_default();
    hash_id_bytes(&input)
}

pub(super) fn reorder_jobs(
    jobs: &mut Vec<DownloadJob>,
    job_id: &str,
    target_job_id: &str,
    after: bool,
) -> bool {
    if job_id == target_job_id {
        return false;
    }
    let Some(source_index) = jobs.iter().position(|job| job.id == job_id) else {
        return false;
    };
    let job = jobs.remove(source_index);
    let Some(target_index) = jobs
        .iter()
        .position(|candidate| candidate.id == target_job_id)
    else {
        jobs.insert(source_index, job);
        return false;
    };
    jobs.insert(target_index + usize::from(after), job);
    true
}
