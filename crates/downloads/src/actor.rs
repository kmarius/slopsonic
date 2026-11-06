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

    pub(super) fn settings_for(&self, source_id: &SourceId) -> SourceDownloadSettings {
        self.settings
            .get(source_id)
            .cloned()
            .unwrap_or_else(|| SourceDownloadSettings::for_source(source_id.clone()))
    }

    pub(super) async fn apply_settings(
        &mut self,
        settings: Vec<SourceDownloadSettings>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let settings = settings
            .into_iter()
            .map(|settings| (settings.source_id.clone(), settings))
            .collect::<HashMap<_, _>>();
        let changed_quality = self
            .settings
            .keys()
            .chain(settings.keys())
            .collect::<HashSet<_>>()
            .into_iter()
            .filter_map(|id| {
                let quality = settings
                    .get(id)
                    .map_or(DownloadQuality::Original, |setting| setting.quality);
                (self.settings_for(id).quality != quality).then(|| (id.clone(), quality))
            })
            .collect::<Vec<_>>();
        self.settings = settings;
        let source_ids = self.attached.keys().cloned().collect::<Vec<_>>();
        for source_id in &source_ids {
            let directory = self
                .settings
                .get(source_id)
                .and_then(|settings| settings.directory.clone());
            if self
                .attached
                .get(source_id)
                .is_some_and(|attached| attached.directory != directory)
            {
                self.abort_matching(active, false, |download| {
                    download.source_id.as_ref() == Some(source_id)
                })
                .await;
                self.discard_previous_directory(source_id, &directory).await;
                if let Some(attached) = self.attached.get_mut(source_id) {
                    attached.directory = directory;
                }
            }
        }
        for (source_id, quality) in changed_quality {
            self.abort_matching(active, true, |download| {
                download.source_id.as_ref() == Some(&source_id) && download.quality != quality
            })
            .await;
            if let Some(jobs) = self.jobs.get_mut(&Some(source_id.clone())) {
                for job in jobs {
                    job.quality = quality;
                    job.replace_unknown = true;
                    job.remaining.extend(job.completed.drain(..));
                    job.failed = false;
                    job.state = DownloadQueueState::Queued;
                }
            }
            let mut after = (String::new(), String::new());
            loop {
                let records = match self
                    .database
                    .download_files_page(Some(source_id.as_str()), &after, None)
                    .await
                {
                    Ok(records) if records.is_empty() => break,
                    Ok(records) => records,
                    Err(error) => {
                        warn!(%error, "could not read downloads for a quality change");
                        break;
                    }
                };
                let last = records.last().unwrap();
                after = (last.media_uri.clone(), last.encoding.clone());
                let mut replacements: HashMap<DownloadSubject, Vec<String>> = HashMap::new();
                for record in records {
                    let Ok(owners) = download_owners(&record) else {
                        continue;
                    };
                    for owner in owners {
                        let subject = match owner {
                            DownloadOwner::Subject(subject) => subject,
                            DownloadOwner::Retained => DownloadSubject::Prepared {
                                context_id: "retained-downloads".into(),
                                title: None,
                            },
                        };
                        replacements
                            .entry(subject)
                            .or_default()
                            .push(record.media_uri.clone());
                    }
                }
                for (subject, uris) in replacements {
                    self.enqueue(Some(source_id.clone()), subject, quality, uris, true)
                        .await;
                }
            }
        }
        for source_id in source_ids {
            self.reconcile_all_rules(&source_id);
        }
    }

    pub(super) fn reconcile_all_rules(&mut self, source_id: &SourceId) {
        let rules = self.settings_for(source_id).rules;
        self.schedule_rules(source_id, rules, true);
    }

    pub(super) fn schedule_rules(
        &mut self,
        source_id: &SourceId,
        rules: DownloadRules,
        authoritative: bool,
    ) {
        let Some(attached) = self.attached.get(source_id).cloned() else {
            return;
        };
        let mut intent = RuleIntent {
            database: self.database.clone(),
            source_key: attached.source_key,
            folder: attached.folder,
            rules,
        };
        if let Some(pending) = self.pending_rules.as_mut() {
            if authoritative || !pending.same_context(&intent) {
                *pending = intent;
            } else {
                for rule in intent.rules.active() {
                    pending.rules.set(rule, true);
                }
            }
        } else {
            if !authoritative
                && let Some(running) = self.running_rules.as_ref()
                && running.source_key == attached.source_key
                && running.same_context(&intent)
            {
                for rule in running.rules.active() {
                    intent.rules.set(rule, true);
                }
            }
            self.pending_rules = Some(intent);
        }
        self.start_rule_preparation();
    }

    pub(super) fn start_rule_preparation(&mut self) {
        if self.running_rules.is_some() {
            return;
        }
        let Some(intent) = self
            .pending_rules
            .take()
            .filter(|intent| !intent.rules.is_empty())
        else {
            return;
        };
        self.running_rules = Some(intent.clone());
        prepare_rules(intent, self.prepared_rules.clone());
    }

    pub(super) async fn apply_prepared_rules(
        &mut self,
        prepared: PreparedRules,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(intent) = self.running_rules.take() else {
            return;
        };
        let Some(source_id) = self.attached.iter().find_map(|(id, attached)| {
            (attached.source_key == intent.source_key).then(|| id.clone())
        }) else {
            self.start_rule_preparation();
            return;
        };
        let superseded = self.pending_rules.is_some();
        let same_context = self.attached[&source_id].folder == intent.folder;
        if !superseded && same_context {
            match prepared {
                Ok(prepared) => {
                    let quality = self.settings_for(&source_id).quality;
                    for (rule, media_uris) in prepared {
                        self.reconcile_rule(source_id.clone(), rule, quality, media_uris, active)
                            .await;
                    }
                }
                Err(error) => {
                    warn!(%error, %source_id, "could not prepare automatic downloads");
                }
            }
        }
        self.start_rule_preparation();
    }

    pub(super) async fn attach(
        &mut self,
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Weak<Source>>,
        folder: Option<FolderKey>,
        directory: Option<DownloadDirectory>,
    ) -> Result<(), String> {
        let unchanged = self.attached.get(&source_id).is_some_and(|attached| {
            attached.directory == directory
                && attached.source_key == source_key
                && same_weak_target(&attached.source, &source)
        });
        self.discard_previous_directory(&source_id, &directory)
            .await;
        self.attached.insert(
            source_id.clone(),
            AttachedSource {
                source_key,
                source: source.clone(),
                folder,
                directory: directory.clone(),
            },
        );
        let group = Some(source_id.clone());
        if unchanged && self.jobs.contains_key(&group) {
            self.retry_waiting();
            self.publish(Some(&source_id)).await;
            return Ok(());
        }
        let mut attachment_error = None;
        if let Err(error) = migrate_download_records(
            &self.root,
            &self.database,
            Some((source_key, &source_id)),
            directory.as_ref(),
        )
        .await
        {
            attachment_error = Some(error);
        }
        self.events
            .counts_dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(group.clone());
        let source_available = source.as_ref().and_then(Weak::upgrade).is_some();
        let mut jobs = if let Some(jobs) = self.jobs.get(&group) {
            jobs.clone()
        } else {
            match load_queue(&self.root, &source_id).await {
                Ok(jobs) => jobs,
                Err(error) => {
                    warn!(%error, %source_id, "could not load the download queue");
                    return Err(error);
                }
            }
        };
        let quality = self.settings_for(&source_id).quality;
        jobs.retain_mut(|job| {
            if job.quality != quality {
                job.quality = quality;
                job.replace_unknown = true;
                job.remaining.extend(job.completed.drain(..));
            }
            job.state = if source_available {
                DownloadQueueState::Queued
            } else {
                DownloadQueueState::WaitingForConnection
            };
            !job.remaining.is_empty()
        });
        let queued_tracks = jobs
            .iter()
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        if let Err(error) = cleanup_staging(
            &self.root,
            Some(&source_id),
            directory.as_ref(),
            &queued_tracks,
        )
        .await
        {
            attachment_error.get_or_insert_with(|| error.to_string());
        }
        self.jobs.insert(group.clone(), jobs);
        self.persist_and_publish(group.as_ref()).await;
        attachment_error.map_or(Ok(()), Err)
    }

    pub(super) async fn add_download_owner(
        &self,
        source_id: Option<&SourceId>,
        media_uri: &str,
        owner: &DownloadOwner,
        quality: DownloadQuality,
        replace_unknown: bool,
    ) -> Result<bool, String> {
        let encoding = library::media_encoding_key(quality.encoding());
        let previous = self
            .database
            .download_file(media_uri, &encoding)
            .await
            .map_err(|error| error.to_string())?;
        let mut saved = previous.clone();
        if saved.is_none() && !replace_unknown {
            saved = self
                .database
                .download_file(media_uri, "unknown")
                .await
                .map_err(|error| error.to_string())?;
        }
        let saved = match saved {
            Some(file) => {
                let paths = completed_download_paths(&self.root, &file)?;
                paths
                    .audio
                    .inspect()
                    .await?
                    .filter(|metadata| {
                        !metadata.size.is_some_and(|size| {
                            file.completed_size
                                .is_some_and(|expected| size != expected as u64)
                        })
                    })
                    .map(|_| (file, paths))
            }
            None => None,
        };
        let Some((mut file, paths)) = saved else {
            return Ok(false);
        };
        file.media_uri = media_uri.into();
        file.source_id = source_id.map(ToString::to_string);
        let mut owners = download_owners(&file)?;
        if !owners.insert(owner.clone())
            && previous.as_ref().is_some_and(|previous| {
                previous.path == file.path && previous.revision == file.revision
            })
        {
            self.retire_previous_files(&mut file).await?;
            return Ok(true);
        }
        file.owners = serde_json::to_string(&owners).map_err(|error| error.to_string())?;
        let source = source_id
            .and_then(|source| self.attached.get(source))
            .map(|source| source.source_key);
        let access =
            download_access(&self.database, media_uri, &paths, file.completed_size).await?;
        file.storage_root = access.root.clone();
        file.relative_path = access.relative_path.clone();
        file.completed_size = Some(access.size_bytes);
        self.database
            .commit_download(&file, source, &access)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(previous) =
            previous.filter(|old| old.path != file.path || old.revision != file.revision)
        {
            self.retire_receipt(&previous).await?;
        }
        Ok(true)
    }

    pub(super) async fn enqueue(
        &mut self,
        source_id: Option<SourceId>,
        subject: DownloadSubject,
        quality: DownloadQuality,
        media_uris: Vec<String>,
        replace_unknown: bool,
    ) -> Option<DownloadFeedback> {
        let attached = source_id
            .as_ref()
            .and_then(|source_id| self.attached.get(source_id));
        let source_available = source_id.is_none()
            || attached
                .and_then(|attached| attached.source.as_ref())
                .and_then(Weak::upgrade)
                .is_some();
        let can_start = !self.paused && source_available;

        let mut seen = HashSet::new();
        let media_uris = media_uris
            .into_iter()
            .filter(|media_uri| !media_uri.is_empty() && seen.insert(media_uri.clone()))
            .collect::<Vec<_>>();
        if media_uris.is_empty() {
            return None;
        }

        let owner = DownloadOwner::Subject(subject.clone());
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for media_uri in &media_uris {
            match self
                .add_download_owner(
                    source_id.as_ref(),
                    media_uri,
                    &owner,
                    quality,
                    replace_unknown,
                )
                .await
            {
                Ok(true) => completed.push(media_uri.clone()),
                Ok(false) => remaining.push(media_uri.clone()),
                Err(error) => {
                    warn!(%error, source_id=?source_id, %media_uri, "could not update download ownership");
                    remaining.push(media_uri.clone());
                }
            }
        }

        let mut scheduled_tracks = 0usize;
        if !remaining.is_empty() || !completed.is_empty() {
            let jobs = self.jobs.entry(source_id.clone()).or_default();
            if let Some(existing_index) = jobs
                .iter()
                .position(|job| job.subject == subject && job.quality == quality)
            {
                let existing = &mut jobs[existing_index];
                existing.failed = false;
                existing.replace_unknown |= replace_unknown;
                let completed_now = completed.iter().collect::<HashSet<_>>();
                existing
                    .remaining
                    .retain(|media_uri| !completed_now.contains(media_uri));
                let mut known = existing
                    .completed
                    .iter()
                    .chain(&existing.remaining)
                    .cloned()
                    .collect::<HashSet<_>>();
                existing.completed.extend(
                    completed
                        .into_iter()
                        .filter(|media_uri| known.insert(media_uri.clone())),
                );
                let additions = remaining
                    .into_iter()
                    .filter(|media_uri| known.insert(media_uri.clone()))
                    .collect::<Vec<_>>();
                scheduled_tracks = additions.len();
                existing.remaining.extend(additions);
                if existing.state != DownloadQueueState::Downloading {
                    existing.state = if source_available {
                        DownloadQueueState::Queued
                    } else {
                        DownloadQueueState::WaitingForConnection
                    };
                }
                if existing.remaining.is_empty() {
                    jobs.remove(existing_index);
                }
            } else if !remaining.is_empty() {
                scheduled_tracks = remaining.len();
                self.next_job = self.next_job.wrapping_add(1);
                jobs.push(DownloadJob {
                    id: job_id(source_id.as_ref(), &subject, self.next_job),
                    subject: subject.clone(),
                    quality,
                    replace_unknown,
                    completed,
                    failed: false,
                    remaining: remaining.into(),
                    state: if source_available {
                        DownloadQueueState::Queued
                    } else {
                        DownloadQueueState::WaitingForConnection
                    },
                });
            }
        }

        self.persist_and_publish(source_id.as_ref()).await;
        (scheduled_tracks > 0).then(|| DownloadFeedback {
            subject,
            preview_uris: media_uris.iter().take(4).cloned().collect(),
            item_count: scheduled_tracks,
            kind: if can_start {
                DownloadFeedbackKind::Started
            } else {
                DownloadFeedbackKind::Queued
            },
        })
    }

    pub(super) async fn reconcile_rule(
        &mut self,
        source_id: SourceId,
        rule: DownloadRule,
        quality: DownloadQuality,
        media_uris: Vec<String>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(attached) = self.attached.get(&source_id).cloned() else {
            warn!(%source_id, "ignored download reconciliation for an unattached source");
            return;
        };
        let source_available = attached.source.as_ref().and_then(Weak::upgrade).is_some();
        let can_start = !self.paused && source_available;

        let mut seen = HashSet::new();
        let media_uris = media_uris
            .into_iter()
            .filter(|media_uri| seen.insert(media_uri.clone()))
            .collect::<Vec<_>>();
        let desired = media_uris.iter().cloned().collect::<HashSet<_>>();
        let subject = DownloadSubject::Rule(rule);
        let group = Some(source_id.clone());
        let quality_changed = self.jobs.get(&group).is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job.subject == subject && job.quality != quality)
        });
        self.abort_matching(active, true, |download| {
            download.source_id.as_ref() == Some(&source_id)
                && download.subject == subject
                && (quality_changed || !desired.contains(&download.media_uri))
        })
        .await;
        let active_job_id = active
            .iter()
            .find(|download| {
                download.source_id.as_ref() == Some(&source_id) && download.subject == subject
            })
            .map(|download| download.job_id.clone());
        let owner = DownloadOwner::Subject(subject.clone());

        let owner_key = serde_json::to_string(&owner).expect("download ownership can be stored");
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(Some(source_id.as_str()), &after, Some(&owner_key))
                .await
            {
                Ok(records) if records.is_empty() => break,
                Ok(records) => records,
                Err(error) => {
                    warn!(%error, %source_id, "could not read rule downloads");
                    return;
                }
            };
            let last = records.last().unwrap();
            after = (last.media_uri.clone(), last.encoding.clone());
            for mut record in records {
                if desired.contains(&record.media_uri) {
                    continue;
                }
                let Ok(mut owners) = download_owners(&record) else {
                    continue;
                };
                owners.remove(&owner);
                owners.extend(self.queued_owners_for_track(
                    &source_id,
                    &record.media_uri,
                    Some(&subject),
                ));
                if let Err(error) = self.store_owners(&mut record, owners).await {
                    warn!(%error, %source_id, media_uri=%record.media_uri, "could not update rule ownership");
                }
            }
        }

        let replace_unknown = self.jobs.get(&Some(source_id.clone())).is_some_and(|jobs| {
            jobs.iter()
                .any(|job| job.subject == subject && job.replace_unknown)
        });
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for media_uri in &media_uris {
            match self
                .add_download_owner(
                    Some(&source_id),
                    media_uri,
                    &owner,
                    quality,
                    replace_unknown,
                )
                .await
            {
                Ok(true) => completed.push(media_uri.clone()),
                Ok(false) => remaining.push(media_uri.clone()),
                Err(error) => {
                    warn!(%error, %source_id, %media_uri, "could not update download ownership");
                    remaining.push(media_uri.clone());
                }
            }
        }

        let jobs = self.jobs.entry(group).or_default();
        let existing_index = jobs.iter().position(|job| job.subject == subject);
        let existing_id = existing_index.map(|index| jobs[index].id.clone());
        let old_remaining = jobs
            .iter()
            .filter(|job| job.subject == subject)
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        jobs.retain(|job| job.subject != subject);

        let scheduled_tracks = remaining
            .iter()
            .filter(|media_uri| quality_changed || !old_remaining.contains(*media_uri))
            .count();
        if !remaining.is_empty() {
            let id = existing_id.unwrap_or_else(|| {
                self.next_job = self.next_job.wrapping_add(1);
                job_id(Some(&source_id), &subject, self.next_job)
            });
            let state = if active_job_id.as_deref() == Some(id.as_str()) {
                DownloadQueueState::Downloading
            } else if source_available {
                DownloadQueueState::Queued
            } else {
                DownloadQueueState::WaitingForConnection
            };
            let job = DownloadJob {
                id,
                subject: subject.clone(),
                quality,
                replace_unknown,
                completed,
                failed: false,
                remaining: remaining.into(),
                state,
            };
            jobs.insert(existing_index.unwrap_or(jobs.len()).min(jobs.len()), job);
        }
        self.reconcile_staging(&Some(source_id.clone())).await;

        self.persist_and_publish(Some(&source_id)).await;
        if scheduled_tracks > 0 {
            let _ = self
                .events
                .send(DownloadEvent::Feedback(DownloadFeedback {
                    subject,
                    preview_uris: media_uris.iter().take(4).cloned().collect(),
                    item_count: scheduled_tracks,
                    kind: if can_start {
                        DownloadFeedbackKind::Started
                    } else {
                        DownloadFeedbackKind::Queued
                    },
                }))
                .await;
        }
    }

    pub(super) async fn fill_slots(&mut self, active: &mut Vec<ActiveDownload>) {
        if self.paused {
            return;
        }
        while active.len() < MAX_ACTIVE_DOWNLOADS {
            let Some(download) = self.start_next(active).await else {
                break;
            };
            active.push(download);
        }
    }

    pub(super) async fn start_next(&mut self, active: &[ActiveDownload]) -> Option<ActiveDownload> {
        loop {
            let (source_id, job_id, subject, quality, media_uri) = self.next_candidate(active)?;
            let attached = source_id
                .as_ref()
                .and_then(|source_id| self.attached.get(source_id))
                .cloned();
            let source = attached
                .as_ref()
                .and_then(|attached| attached.source.as_ref())
                .and_then(Weak::upgrade);
            if source_id.is_some() && source.is_none() {
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::WaitingForConnection;
                }
                self.publish(source_id.as_ref()).await;
                continue;
            }
            let replace_unknown = self
                .find_job_mut(&source_id, &job_id)
                .is_some_and(|job| job.replace_unknown);
            let custom_directory = attached
                .as_ref()
                .and_then(|attached| attached.directory.as_ref());
            let owner = DownloadOwner::Subject(subject.clone());
            match self
                .add_download_owner(
                    source_id.as_ref(),
                    &media_uri,
                    &owner,
                    quality,
                    replace_unknown,
                )
                .await
            {
                Ok(true) => {
                    self.remove_job_track(&source_id, &job_id, &media_uri, true);
                    self.persist_and_publish(source_id.as_ref()).await;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    warn!(%error, source_id=?source_id, %media_uri, "could not update download ownership");
                    if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                        job.state = DownloadQueueState::NeedsAttention;
                    }
                    self.persist_and_publish(source_id.as_ref()).await;
                    continue;
                }
            }
            let resolved = if let Some(source) = source.as_ref() {
                source
                    .resolve_download(&self.database, &media_uri, quality)
                    .await
            } else {
                Some(media_uri.as_str())
                    .filter(|uri| uri.starts_with("http://") || uri.starts_with("https://"))
                    .map(|uri| ResolvedDownload::Ready {
                        stream: ResolvedStream::new(uri),
                        codec: None,
                    })
                    .ok_or(SourceError::InvalidRequest(
                        "Direct media is not downloadable",
                    ))
            };
            let transcoded_extension = match &resolved {
                Ok(ResolvedDownload::Ready { codec, .. }) => codec.map(|codec| codec.extension()),
                Ok(ResolvedDownload::Encode { encoding, .. }) => Some(encoding.codec.extension()),
                Err(_) => None,
            };
            let metadata = self
                .database
                .download_metadata(&media_uri)
                .await
                .ok()
                .flatten();
            let paths = new_download_paths(
                &self.root,
                source_id.as_ref(),
                &media_uri,
                metadata.as_ref(),
                custom_directory,
                transcoded_extension,
            );
            if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                job.state = DownloadQueueState::Downloading;
            }
            self.publish(source_id.as_ref()).await;
            let task_paths = paths.clone();
            let transfers = Arc::clone(&self.transfers);
            let (cancellation, cancelled) = tokio::sync::oneshot::channel();
            let uri = media_uri.clone();
            let database = self.database.clone();
            let artwork = self.artwork.clone();
            let task_source_id = source_id.clone();
            let task = tokio::spawn(async move {
                let mut cancelled = cancelled;
                match resolved.map_err(download_source_failure)? {
                    ResolvedDownload::Encode { stream, encoding } => {
                        encode_download(stream, encoding, &task_paths, &mut cancelled)
                            .await
                            .map_err(download_source_failure)?;
                    }
                    ResolvedDownload::Ready { stream, codec } => {
                        let needs_encoding = match run_transfer(
                            task_source_id.as_ref(),
                            &uri,
                            quality,
                            &stream,
                            &task_paths,
                            &transfers,
                            &mut cancelled,
                        )
                        .await
                        {
                            Ok(()) => {
                                if let Some(codec) = codec {
                                    let path = task_paths.audio_part.clone();
                                    tokio::task::spawn_blocking(move || {
                                        sources::downloaded_codec(&path)
                                    })
                                    .await
                                    .map_err(|error| {
                                        DownloadFailure::NeedsAttention(error.to_string())
                                    })? != Some(codec)
                                } else {
                                    false
                                }
                            }
                            Err(SourceError::Server {
                                status: 415 | 500 | 501,
                                ..
                            }) if codec.is_some() => true,
                            Err(error) => return Err(download_source_failure(error)),
                        };
                        if needs_encoding {
                            let source = source.as_ref().ok_or_else(|| {
                                DownloadFailure::NeedsAttention(
                                    "The audio format did not match the download profile".into(),
                                )
                            })?;
                            let (ResolvedDownload::Ready {
                                stream: original, ..
                            }
                            | ResolvedDownload::Encode {
                                stream: original, ..
                            }) = source
                                .resolve_download(&database, &uri, DownloadQuality::Original)
                                .await
                                .map_err(download_source_failure)?;
                            let encoding = quality
                                .encoding()
                                .expect("encoded downloads have a profile");
                            encode_download(original, encoding, &task_paths, &mut cancelled)
                                .await
                                .map_err(download_source_failure)?;
                        }
                    }
                }
                // Companion failures do not discard successfully downloaded audio.
                tokio::select! {
                    _ = &mut cancelled => {},
                    _ = cache_track_artwork(&database, &artwork, &uri) => {},
                }
                Ok(())
            });
            return Some(ActiveDownload {
                source_id,
                job_id,
                media_uri,
                subject,
                paths,
                quality,
                cancellation: Some(cancellation),
                task,
            });
        }
    }

    pub(super) fn next_candidate(
        &self,
        active: &[ActiveDownload],
    ) -> Option<(
        Option<SourceId>,
        String,
        DownloadSubject,
        DownloadQuality,
        String,
    )> {
        for (source_id, jobs) in &self.jobs {
            for job in jobs {
                if !matches!(
                    job.state,
                    DownloadQueueState::Queued | DownloadQueueState::Downloading
                ) {
                    break;
                }
                let media_uri = job.remaining.iter().find(|media_uri| {
                    !active.iter().any(|download| {
                        download.source_id == *source_id && download.media_uri == **media_uri
                    })
                });
                if let Some(media_uri) = media_uri {
                    return Some((
                        source_id.clone(),
                        job.id.clone(),
                        job.subject.clone(),
                        job.quality,
                        media_uri.clone(),
                    ));
                }
            }
        }
        None
    }

    pub(super) async fn finish(
        &mut self,
        active: ActiveDownload,
        joined: Result<Result<(), DownloadFailure>, tokio::task::JoinError>,
        remaining_active: &mut Vec<ActiveDownload>,
    ) {
        if self
            .find_job_mut(&active.source_id, &active.job_id)
            .is_none()
        {
            return;
        }
        let ActiveDownload {
            source_id,
            job_id,
            media_uri,
            subject,
            paths,
            quality,
            ..
        } = active;
        let result = match joined {
            Ok(Ok(())) => self
                .commit_transfer(&source_id, &media_uri, &subject, &paths, quality)
                .await
                .map(|_| ()),
            Ok(Err(error)) => Err(error),
            Err(error) => Err(DownloadFailure::NeedsAttention(format!(
                "download task failed: {error}"
            ))),
        };
        let persist = match result {
            Ok(()) => {
                self.remove_job_track(&source_id, &job_id, &media_uri, true);
                true
            }
            Err(DownloadFailure::Item(error)) => {
                warn!(%error, source_id=?source_id, %media_uri, "could not download track");
                self.mark_subject_incomplete(&subject).await;
                self.remove_job_track(&source_id, &job_id, &media_uri, false);
                true
            }
            Err(DownloadFailure::Retry(error)) => {
                warn!(%error, source_id=?source_id, "download is waiting for the server");
                let completed = self
                    .abort_matching(remaining_active, true, |download| {
                        download.source_id == source_id && download.job_id == job_id
                    })
                    .await;
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::WaitingForConnection;
                }
                !completed.is_empty()
            }
            Err(DownloadFailure::NeedsAttention(error)) => {
                warn!(%error, source_id=?source_id, "download needs attention");
                self.abort_matching(remaining_active, true, |download| {
                    download.source_id == source_id && download.job_id == job_id
                })
                .await;
                if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                    job.state = DownloadQueueState::NeedsAttention;
                }
                true
            }
        };
        if persist {
            self.persist_and_publish(source_id.as_ref()).await;
        } else {
            self.publish(source_id.as_ref()).await;
        }
    }

    pub(super) async fn commit_transfer(
        &self,
        source_id: &Option<SourceId>,
        media_uri: &str,
        subject: &DownloadSubject,
        paths: &DownloadPaths,
        quality: DownloadQuality,
    ) -> Result<DownloadPaths, DownloadFailure> {
        let mut file = library::DownloadFile {
            media_uri: media_uri.into(),
            encoding: library::media_encoding_key(quality.encoding()),
            managed: true,
            ..Default::default()
        };
        let previous = self
            .database
            .download_file(media_uri, &file.encoding)
            .await
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        let finished = finalize_download(paths)
            .await
            .map_err(DownloadFailure::NeedsAttention)?;
        let mut owners = previous
            .as_ref()
            .map(download_owners)
            .transpose()
            .map_err(DownloadFailure::NeedsAttention)?
            .unwrap_or_default();
        owners.insert(DownloadOwner::Subject(subject.clone()));
        file.source_id = source_id.as_ref().map(ToString::to_string);
        file.path = finished
            .paths
            .audio
            .access_uri()
            .map_err(DownloadFailure::NeedsAttention)?;
        file.owners = serde_json::to_string(&owners)
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        file.completed_size = Some(i64::try_from(finished.completed_size).unwrap_or(i64::MAX));
        let mut retired: Vec<String> = previous
            .as_ref()
            .filter(|file| !file.retired_documents.is_empty())
            .map(|file| serde_json::from_str(&file.retired_documents))
            .transpose()
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?
            .unwrap_or_default();
        if let Some(previous) = previous
            .as_ref()
            .filter(|old| old.managed && old.path != file.path)
        {
            let used = self
                .database
                .download_file_shared(previous)
                .await
                .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
            if !used {
                retired.push(previous.path.clone());
            }
        }
        file.retired_documents = serde_json::to_string(&retired)
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        let source = source_id
            .as_ref()
            .and_then(|id| self.attached.get(id))
            .map(|source| source.source_key);
        let access = download_access(
            &self.database,
            media_uri,
            &finished.paths,
            file.completed_size,
        )
        .await
        .map_err(DownloadFailure::NeedsAttention)?;
        file.storage_root = access.root.clone();
        file.relative_path = access.relative_path.clone();
        self.database
            .commit_download(&file, source, &access)
            .await
            .map_err(|error| DownloadFailure::NeedsAttention(error.to_string()))?;
        self.events
            .counts_dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(source_id.clone());
        let _ = self
            .events
            .send(DownloadEvent::Changed {
                media_uri: media_uri.into(),
                downloaded: true,
            })
            .await;
        if let Err(error) = self.retire_previous_files(&mut file).await {
            let _ = self.events.send(DownloadEvent::Notice(error)).await;
        }
        Ok(finished.paths)
    }

    pub(super) async fn retire_previous_files(
        &self,
        file: &mut library::DownloadFile,
    ) -> Result<(), String> {
        let mut retired: Vec<String> = serde_json::from_str(if file.retired_documents.is_empty() {
            "[]"
        } else {
            &file.retired_documents
        })
        .map_err(|error| error.to_string())?;
        while let Some(path) = retired.first() {
            let old = library::DownloadFile {
                path: path.clone(),
                managed: true,
                ..file.clone()
            };
            self.remove_representation_bytes(&old).await?;
            retired.remove(0);
            file.retired_documents =
                serde_json::to_string(&retired).map_err(|error| error.to_string())?;
            self.database
                .save_download_file(file)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub(super) async fn abort_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) -> Vec<String> {
        self.settle_matching(active, preserve, true, matches).await
    }

    pub(super) async fn discard_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) {
        self.settle_matching(active, preserve, false, matches).await;
    }

    pub(super) async fn settle_matching(
        &mut self,
        active: &mut Vec<ActiveDownload>,
        preserve: bool,
        commit_completed: bool,
        matches: impl Fn(&ActiveDownload) -> bool,
    ) -> Vec<String> {
        let mut settling = Vec::new();
        let mut index = 0;
        while index < active.len() {
            if !matches(&active[index]) {
                index += 1;
                continue;
            }
            let mut download = active.swap_remove(index);
            if let Some(cancellation) = download.cancellation.take() {
                let _ = cancellation.send(());
            }
            settling.push(download);
        }

        let mut affected = HashSet::new();
        let mut completed_tracks = Vec::new();
        for download in settling {
            let joined = download.task.await;
            let completed = matches!(joined, Ok(Ok(())));
            if completed && commit_completed {
                match self
                    .commit_transfer(
                        &download.source_id,
                        &download.media_uri,
                        &download.subject,
                        &download.paths,
                        download.quality,
                    )
                    .await
                {
                    Ok(_) => {
                        completed_tracks.push(download.media_uri.clone());
                        self.remove_job_track(
                            &download.source_id,
                            &download.job_id,
                            &download.media_uri,
                            true,
                        );
                    }
                    Err(DownloadFailure::NeedsAttention(error)) => {
                        warn!(
                            %error,
                            source_id = ?download.source_id,
                            media_uri = %download.media_uri,
                            "could not finish a completed download"
                        );
                        if let Some(job) = self.find_job_mut(&download.source_id, &download.job_id)
                        {
                            job.state = DownloadQueueState::NeedsAttention;
                        }
                    }
                    Err(_) => unreachable!("download commit failures need attention"),
                }
                continue;
            }
            affected.insert((download.source_id.clone(), download.job_id.clone()));
            if !preserve && let Err(error) = discard_staging(&download.paths).await {
                warn!(
                    %error,
                    source_id = ?download.source_id,
                    media_uri = %download.media_uri,
                    "could not settle an interrupted download"
                );
            }
        }
        for (source_id, job_id) in affected {
            let still_active = active
                .iter()
                .any(|download| download.source_id == source_id && download.job_id == job_id);
            if let Some(job) = self.find_job_mut(&source_id, &job_id) {
                job.state = if still_active {
                    DownloadQueueState::Downloading
                } else {
                    DownloadQueueState::Queued
                };
            }
        }
        completed_tracks
    }

    pub(super) async fn reconcile_staging(&self, source_id: &Option<SourceId>) {
        let queued = self
            .jobs
            .get(source_id)
            .into_iter()
            .flatten()
            .flat_map(|job| job.remaining.iter().cloned())
            .collect::<HashSet<_>>();
        let directory = source_id
            .as_ref()
            .and_then(|source_id| self.attached.get(source_id))
            .and_then(|attached| attached.directory.as_ref());
        if let Err(error) =
            cleanup_staging(&self.root, source_id.as_ref(), directory, &queued).await
        {
            warn!(%error, source_id=?source_id, "could not reconcile download staging");
        }
    }

    pub(super) async fn discard_previous_directory(
        &self,
        source_id: &SourceId,
        directory: &Option<DownloadDirectory>,
    ) {
        let Some(attached) = self.attached.get(source_id) else {
            return;
        };
        if attached.directory == *directory {
            return;
        }
        if let Err(error) = cleanup_staging(
            &self.root,
            Some(source_id),
            attached.directory.as_ref(),
            &HashSet::new(),
        )
        .await
        {
            warn!(%error, %source_id, "could not remove download staging from the previous folder");
        }
    }

    pub(super) fn queued_owners_for_track(
        &self,
        source_id: &SourceId,
        identity: &str,
        excluded_subject: Option<&DownloadSubject>,
    ) -> HashSet<DownloadOwner> {
        self.jobs
            .get(&Some(source_id.clone()))
            .into_iter()
            .flatten()
            .filter(|job| excluded_subject != Some(&job.subject))
            .filter(|job| job.remaining.iter().any(|media_uri| media_uri == identity))
            .map(|job| DownloadOwner::Subject(job.subject.clone()))
            .collect()
    }

    pub(super) fn find_job_mut(
        &mut self,
        source_id: &Option<SourceId>,
        job_id: &str,
    ) -> Option<&mut DownloadJob> {
        self.jobs
            .get_mut(source_id)?
            .iter_mut()
            .find(|job| job.id == job_id)
    }

    pub(super) fn remove_job_track(
        &mut self,
        source_id: &Option<SourceId>,
        job_id: &str,
        media_uri: &str,
        completed: bool,
    ) {
        let Some(jobs) = self.jobs.get_mut(source_id) else {
            return;
        };
        let Some(job_index) = jobs.iter().position(|job| job.id == job_id) else {
            return;
        };
        let job = &mut jobs[job_index];
        if let Some(position) = job
            .remaining
            .iter()
            .position(|candidate| candidate == media_uri)
        {
            job.remaining.remove(position);
        }
        if completed {
            job.completed.push(media_uri.to_string());
        }
        if job.remaining.is_empty() {
            let job = jobs.remove(job_index);
            if completed && !job.failed {
                self.publish_subject_complete(&job.subject);
            }
        } else {
            job.state = DownloadQueueState::Downloading;
        }
    }

    pub(super) fn publish_subject_complete(&self, subject: &DownloadSubject) {
        if !self
            .jobs
            .values()
            .flatten()
            .any(|job| &job.subject == subject)
        {
            let _ = self.events.try_send(DownloadEvent::SubjectChanged {
                subject: subject.clone(),
                downloaded: true,
            });
        }
    }

    pub(super) async fn mark_subject_incomplete(&mut self, subject: &DownloadSubject) {
        for (source, jobs) in &mut self.jobs {
            let mut changed = false;
            for job in jobs
                .iter_mut()
                .filter(|job| &job.subject == subject && !job.failed)
            {
                job.failed = true;
                changed = true;
            }
            if changed && let Err(error) = persist_queue(&self.root, source.as_ref(), jobs).await {
                warn!(%error, "could not save download progress");
            }
        }
        let _ = self.events.try_send(DownloadEvent::SubjectChanged {
            subject: subject.clone(),
            downloaded: false,
        });
    }

    pub(super) fn retry_waiting(&mut self) {
        for (source_id, jobs) in &mut self.jobs {
            let available = source_id.as_ref().is_none_or(|source_id| {
                self.attached.get(source_id).is_some_and(|attached| {
                    attached
                        .source
                        .as_ref()
                        .is_some_and(|source| source.strong_count() > 0)
                })
            });
            if let Some(first) = jobs.first_mut()
                && available
                && first.state == DownloadQueueState::WaitingForConnection
            {
                first.state = DownloadQueueState::Queued;
            }
        }
    }

    pub(super) async fn force_remove(
        &mut self,
        source_id: Option<&SourceId>,
        media_uris: Vec<String>,
        notify: bool,
    ) {
        let group = source_id.cloned();
        let remove = media_uris.iter().collect::<HashSet<_>>();
        let subjects = self
            .jobs
            .get(&group)
            .into_iter()
            .flatten()
            .filter(|job| {
                job.remaining
                    .iter()
                    .chain(&job.completed)
                    .any(|uri| remove.contains(uri))
            })
            .map(|job| job.subject.clone())
            .collect::<HashSet<_>>();
        for subject in subjects {
            self.mark_subject_incomplete(&subject).await;
        }
        for job in self.jobs.entry(group.clone()).or_default().iter_mut() {
            job.remaining
                .retain(|media_uri| !remove.contains(media_uri));
            job.completed
                .retain(|media_uri| !remove.contains(media_uri));
        }
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| !job.remaining.is_empty());
        self.reconcile_staging(&group).await;

        let (removed, failed) = self.delete_downloads(source_id, media_uris).await;
        self.persist_and_publish(source_id).await;
        if notify {
            self.send_removal_notice(removed, failed).await;
        }
    }

    pub(super) async fn delete_downloads(
        &self,
        _source_id: Option<&SourceId>,
        media_uris: impl IntoIterator<Item = String>,
    ) -> (usize, usize) {
        let mut removed = 0;
        let mut failed = 0;
        for uri in media_uris {
            match self.database.download_files(&uri).await {
                Ok(files) => {
                    for file in files {
                        match self.retire_receipt(&file).await {
                            Ok(count) => removed += count,
                            Err(error) => {
                                failed += 1;
                                warn!(%error, %uri, "could not remove downloaded media");
                            }
                        }
                    }
                }
                Err(error) => {
                    failed += 1;
                    warn!(%error, %uri, "could not read downloaded media");
                }
            }
        }
        (removed, failed)
    }

    pub(super) async fn remove_representation_bytes(
        &self,
        file: &library::DownloadFile,
    ) -> Result<usize, String> {
        let used = self
            .database
            .download_file_shared(file)
            .await
            .map_err(|error| error.to_string())?;
        if !file.managed || used {
            return Ok(0);
        }
        let paths = completed_download_paths(&self.root, file)?;
        remove_download_files(&paths).await.map(usize::from)
    }

    pub(super) async fn retire_receipt(
        &self,
        file: &library::DownloadFile,
    ) -> Result<usize, String> {
        let mut file = file.clone();
        self.retire_previous_files(&mut file).await?;
        let removed = self.remove_representation_bytes(&file).await?;
        self.database
            .forget_download_file(&file)
            .await
            .map_err(|error| error.to_string())?;
        let downloaded = !self
            .database
            .download_files(&file.media_uri)
            .await
            .map_err(|error| error.to_string())?
            .is_empty();
        self.events
            .counts_dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(file.source_id.clone().map(SourceId::new));
        let _ = self
            .events
            .send(DownloadEvent::Changed {
                media_uri: file.media_uri.clone(),
                downloaded,
            })
            .await;
        for owner in download_owners(&file)? {
            if let DownloadOwner::Subject(subject) = owner {
                let _ = self.events.try_send(DownloadEvent::SubjectChanged {
                    subject,
                    downloaded: false,
                });
            }
        }
        Ok(removed)
    }

    pub(super) async fn send_removal_notice(&self, removed: usize, failed: usize) {
        let message = match (removed, failed) {
            (0, 0) => "This track is not downloaded".to_string(),
            (1, 0) => "Removed 1 download".to_string(),
            (count, 0) => format!("Removed {count} downloads"),
            (_, failed) => format!("Could not remove {failed} downloads"),
        };
        let _ = self.events.send(DownloadEvent::Notice(message)).await;
    }

    pub(super) async fn remove_rule(
        &mut self,
        source_id: &SourceId,
        rule: DownloadRule,
        delete_downloads: bool,
    ) {
        let subject = DownloadSubject::Rule(rule);
        let group = Some(source_id.clone());
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| job.subject != subject);
        self.reconcile_staging(&group).await;
        self.release_owner(source_id, &subject, None, !delete_downloads)
            .await;
        self.persist_and_publish(Some(source_id)).await;
    }

    pub(super) async fn cancel(
        &mut self,
        source_id: Option<&SourceId>,
        job_id: &str,
        active: &mut Vec<ActiveDownload>,
    ) {
        let group = source_id.cloned();
        let subject = self
            .jobs
            .get(&group)
            .and_then(|jobs| jobs.iter().find(|job| job.id == job_id))
            .map(|job| job.subject.clone());
        let Some(subject) = subject else { return };
        self.mark_subject_incomplete(&subject).await;
        self.abort_matching(active, true, |download| {
            download.source_id.as_ref() == source_id && download.job_id == job_id
        })
        .await;
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| job.id != job_id);
        self.reconcile_staging(&group).await;
        self.persist_and_publish(source_id).await;
    }

    pub(super) async fn clear_job(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        active: &mut Vec<ActiveDownload>,
    ) {
        let group = Some(source_id.clone());
        let Some(job) = self
            .jobs
            .get(&group)
            .and_then(|jobs| jobs.iter().find(|job| job.id == job_id))
            .cloned()
        else {
            return;
        };
        self.mark_subject_incomplete(&job.subject).await;
        let completed = job
            .completed
            .into_iter()
            .chain(
                self.abort_matching(active, true, |download| {
                    download.source_id.as_ref() == Some(source_id) && download.job_id == job_id
                })
                .await,
            )
            .collect::<HashSet<_>>();
        let subject = job.subject;
        self.jobs
            .entry(group.clone())
            .or_default()
            .retain(|job| job.id != job_id);
        self.reconcile_staging(&group).await;
        self.release_owner(source_id, &subject, Some(&completed), false)
            .await;
        self.persist_and_publish(Some(source_id)).await;
    }

    pub(super) async fn release_owner(
        &self,
        source_id: &SourceId,
        subject: &DownloadSubject,
        media_ids: Option<&HashSet<String>>,
        retain: bool,
    ) {
        if media_ids.is_some_and(HashSet::is_empty) {
            return;
        }
        let owner = DownloadOwner::Subject(subject.clone());
        let owner_key = serde_json::to_string(&owner).expect("download ownership can be stored");
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(Some(source_id.as_str()), &after, Some(&owner_key))
                .await
            {
                Ok(records) if records.is_empty() => break,
                Ok(records) => records,
                Err(error) => {
                    warn!(%error, %source_id, "could not read download ownership");
                    return;
                }
            };
            let last = records.last().unwrap();
            after = (last.media_uri.clone(), last.encoding.clone());
            for mut record in records {
                if media_ids.is_some_and(|ids| !ids.contains(&record.media_uri)) {
                    continue;
                }
                let Ok(mut owners) = download_owners(&record) else {
                    continue;
                };
                owners.remove(&owner);
                if retain {
                    owners.insert(DownloadOwner::Retained);
                }
                owners.extend(self.queued_owners_for_track(source_id, &record.media_uri, None));
                if let Err(error) = self.store_owners(&mut record, owners).await {
                    warn!(%error, %source_id, media_uri=%record.media_uri, "could not update download ownership");
                }
            }
        }
    }

    pub(super) async fn store_owners(
        &self,
        file: &mut library::DownloadFile,
        owners: HashSet<DownloadOwner>,
    ) -> Result<(), String> {
        if owners.is_empty() {
            self.retire_receipt(file).await.map(|_| ())
        } else {
            file.owners = serde_json::to_string(&owners).map_err(|error| error.to_string())?;
            self.database
                .save_download_file(file)
                .await
                .map_err(|error| error.to_string())
        }
    }

    pub(super) async fn move_job(
        &mut self,
        source_id: &SourceId,
        job_id: &str,
        movement: DownloadQueueMove,
    ) {
        let jobs = self.jobs.entry(Some(source_id.clone())).or_default();
        let (target, after) = match movement {
            DownloadQueueMove::Before { job_id } => (job_id, false),
            DownloadQueueMove::After { job_id } => (job_id, true),
            direction => {
                let Some(position) = jobs.iter().position(|job| job.id == job_id) else {
                    return;
                };
                let after = matches!(direction, DownloadQueueMove::Down);
                let target = if after {
                    position.checked_add(1)
                } else {
                    position.checked_sub(1)
                };
                let Some(target) = target.and_then(|position| jobs.get(position)) else {
                    return;
                };
                (target.id.clone(), after)
            }
        };
        let changed = reorder_jobs(jobs, job_id, &target, after);
        if changed {
            self.persist_and_publish(Some(source_id)).await;
        } else {
            self.publish(Some(source_id)).await;
        }
    }

    pub(super) async fn clear(&mut self, source_id: &SourceId, notify: bool) {
        let subjects = self
            .jobs
            .get(&Some(source_id.clone()))
            .into_iter()
            .flatten()
            .map(|job| job.subject.clone())
            .collect::<HashSet<_>>();
        for subject in subjects {
            self.mark_subject_incomplete(&subject).await;
        }
        let staging_directory = self
            .attached
            .get(source_id)
            .and_then(|attached| attached.directory.clone());
        self.jobs.remove(&Some(source_id.clone()));
        let directory = source_directory(&self.root, Some(source_id));
        let result = async {
            cleanup_staging(
                &self.root,
                Some(source_id),
                staging_directory.as_ref(),
                &HashSet::new(),
            )
            .await
            .map_err(|error| error.to_string())?;
            let mut after = (String::new(), String::new());
            loop {
                let records = self
                    .database
                    .download_files_page(Some(source_id.as_str()), &after, None)
                    .await
                    .map_err(|error| error.to_string())?;
                let Some(last) = records.last() else {
                    break;
                };
                after = (last.media_uri.clone(), last.encoding.clone());
                for record in records {
                    self.retire_receipt(&record).await?;
                }
            }
            remove_file_if_present(&directory.join(QUEUE_FILE)).await?;
            remove_file_if_present(&directory.join(QUEUE_PART_FILE)).await?;
            match tokio::fs::remove_dir(&directory).await {
                Ok(()) => Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    Ok(())
                }
                Err(error) => Err(format!("could not remove {}: {error}", directory.display())),
            }
        }
        .await;
        match result {
            Ok(()) => {
                self.publish(Some(source_id)).await;
                if notify {
                    let _ = self
                        .events
                        .send(DownloadEvent::Notice("Removed all downloads".to_string()))
                        .await;
                }
            }
            Err(error) => {
                warn!(%error, %source_id, "could not clear source downloads");
                if notify {
                    let _ = self
                        .events
                        .send(DownloadEvent::Notice(
                            "Could not remove all downloads".to_string(),
                        ))
                        .await;
                }
            }
        }
    }

    pub(super) async fn persist_and_publish(&self, source_id: Option<&SourceId>) {
        let group = source_id.cloned();
        if let Err(error) = persist_queue(
            &self.root,
            source_id,
            self.jobs.get(&group).map(Vec::as_slice).unwrap_or(&[]),
        )
        .await
        {
            warn!(%error, source_id=?source_id, "could not save the download queue");
        }
        self.publish(source_id).await;
    }

    pub(super) async fn publish_all(&self) {
        let source_ids = self.jobs.keys().cloned().collect::<Vec<_>>();
        for source_id in source_ids {
            self.publish(source_id.as_ref()).await;
        }
    }

    pub(super) async fn publish(&self, source_id: Option<&SourceId>) {
        let group = source_id.cloned();
        let previous = self
            .events
            .status
            .borrow()
            .queues
            .iter()
            .find(|queue| queue.source_id == group)
            .map(|queue| queue.downloaded_tracks);
        let changed = self
            .events
            .counts_dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&group);
        let downloaded_tracks = if changed || previous.is_none() {
            self.database
                .downloaded_count(source_id.map(SourceId::as_str))
                .await
                .unwrap_or_default() as u64
        } else {
            previous.unwrap()
        };
        let _ = self
            .events
            .send(DownloadEvent::Queue {
                summary: DownloadQueueSummary {
                    source_id: group.clone(),
                    total_jobs: self.jobs.get(&group).map_or(0, |jobs| jobs.len() as u64),
                    downloaded_tracks,
                },
                paused: self.paused,
            })
            .await;
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

impl Actor {
    pub(super) async fn queue_page(
        &self,
        scope: DownloadQueueScope,
        offset: u64,
        limit: u32,
    ) -> Result<DownloadPage, String> {
        let mut groups = self
            .jobs
            .iter()
            .filter(|(source, _)| match &scope {
                DownloadQueueScope::All => true,
                DownloadQueueScope::Source { source_id } => source.as_ref() == Some(source_id),
                DownloadQueueScope::Direct => source.is_none(),
            })
            .collect::<Vec<_>>();
        groups.sort_by(|(a, _), (b, _)| a.cmp(b));
        let total = groups.iter().map(|(_, jobs)| jobs.len() as u64).sum();
        let mut offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let limit = limit.min(128) as usize;
        let mut jobs = Vec::with_capacity(limit);
        for (source, queued) in groups {
            if offset >= queued.len() {
                offset -= queued.len();
                continue;
            }
            jobs.extend(
                queued
                    .iter()
                    .skip(offset)
                    .take(limit - jobs.len())
                    .map(|job| job.queue_item(source.clone())),
            );
            offset = 0;
            if jobs.len() == limit {
                break;
            }
        }
        let uris = jobs
            .iter()
            .filter_map(|job| job.preview_uris.first().cloned())
            .collect::<Vec<_>>();
        let previews = self
            .database
            .tracks_by_uris(&uris, &library::ReadCancellation::new())
            .await
            .map_err(|error| error.to_string())?;
        Ok(DownloadPage {
            total,
            jobs,
            previews,
        })
    }
}
