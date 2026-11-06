use super::*;

impl Actor {
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
}
