use super::*;

impl Actor {
    pub(super) fn queued_owners_for_track(
        &self,
        identity: &str,
        excluded_subject: Option<&DownloadSubject>,
    ) -> HashSet<DownloadOwner> {
        self.jobs
            .iter()
            .filter(|job| excluded_subject != Some(&job.subject))
            .filter(|job| job.remaining.iter().any(|media_uri| media_uri == identity))
            .map(|job| DownloadOwner::Subject(job.subject.clone()))
            .collect()
    }

    pub(super) fn find_job_mut(&mut self, job_id: &str) -> Option<&mut DownloadJob> {
        self.jobs.iter_mut().find(|job| job.id == job_id)
    }

    /// Takes `media_uri` from its job; true when that ended the job.
    pub(super) fn remove_job_track(
        &mut self,
        job_id: &str,
        media_uri: &str,
        completed: bool,
    ) -> bool {
        let jobs = &mut self.jobs;
        let Some(job_index) = jobs.iter().position(|job| job.id == job_id) else {
            return false;
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
            return true;
        }
        job.state = DownloadQueueState::Downloading;
        false
    }

    pub(super) fn publish_subject_complete(&self, subject: &DownloadSubject) {
        if !self.jobs.iter().any(|job| &job.subject == subject) {
            let _ = self.events.try_send(DownloadEvent::SubjectChanged {
                subject: subject.clone(),
                downloaded: true,
            });
        }
    }

    pub(super) async fn mark_subject_incomplete(&mut self, subject: &DownloadSubject) {
        let mut changed = false;
        for job in self
            .jobs
            .iter_mut()
            .filter(|job| &job.subject == subject && !job.failed)
        {
            job.failed = true;
            changed = true;
        }
        if changed
            && self.attached.is_some()
            && let Err(error) = persist_queue(&self.root, &self.jobs).await
        {
            warn!(%error, "could not save download progress");
        }
        let _ = self.events.try_send(DownloadEvent::SubjectChanged {
            subject: subject.clone(),
            downloaded: false,
        });
    }

    pub(super) fn retry_waiting(&mut self) {
        if let Some(first) = self.jobs.first_mut()
            && first.state == DownloadQueueState::WaitingForConnection
        {
            first.state = DownloadQueueState::Queued;
        }
    }

    pub(super) async fn source(&self) -> Option<Arc<Source>> {
        let resolve = Arc::clone(&self.sources);
        tokio::task::spawn_blocking(move || resolve())
            .await
            .ok()
            .flatten()
    }

    pub(super) async fn cancel(&mut self, job_id: &str, active: &mut Vec<ActiveDownload>) {
        let subject = self
            .jobs
            .iter()
            .find(|job| job.id == job_id)
            .map(|job| job.subject.clone());
        let Some(subject) = subject else { return };
        self.mark_subject_incomplete(&subject).await;
        self.abort_matching(active, true, |download| download.job_id == job_id)
            .await;
        self.jobs.retain(|job| job.id != job_id);
        self.reconcile_staging().await;
        self.persist_and_publish().await;
    }

    pub(super) async fn clear_job(&mut self, job_id: &str, active: &mut Vec<ActiveDownload>) {
        let Some(job) = self.jobs.iter().find(|job| job.id == job_id).cloned() else {
            return;
        };
        self.mark_subject_incomplete(&job.subject).await;
        let completed = job
            .completed
            .into_iter()
            .chain(
                self.abort_matching(active, true, |download| download.job_id == job_id)
                    .await,
            )
            .collect::<HashSet<_>>();
        let subject = job.subject;
        self.jobs.retain(|job| job.id != job_id);
        self.reconcile_staging().await;
        self.release_owner(&subject, Some(&completed), false).await;
        self.persist_and_publish().await;
    }

    pub(super) async fn release_owner(
        &self,
        subject: &DownloadSubject,
        media_ids: Option<&HashSet<String>>,
        retain: bool,
    ) {
        if media_ids.is_some_and(HashSet::is_empty) {
            return;
        }
        if self.attached.is_none() {
            return;
        }
        let owner = DownloadOwner::Subject(subject.clone());
        let owner_key = serde_json::to_string(&owner).expect("download ownership can be stored");
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(&after, Some(&owner_key), false)
                .await
            {
                Ok(records) if records.is_empty() => break,
                Ok(records) => records,
                Err(error) => {
                    warn!(%error, "could not read download ownership");
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
                owners.extend(self.queued_owners_for_track(&record.media_uri, None));
                if let Err(error) = self.store_owners(&mut record, owners).await {
                    warn!(%error, media_uri=%record.media_uri, "could not update download ownership");
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

    pub(super) async fn move_job(&mut self, job_id: &str, movement: DownloadQueueMove) {
        let jobs = &mut self.jobs;
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
            self.persist_and_publish().await;
        } else {
            self.publish().await;
        }
    }

    pub(super) async fn clear(&mut self, notify: bool) {
        let subjects = self
            .jobs
            .iter()
            .map(|job| job.subject.clone())
            .collect::<HashSet<_>>();
        for subject in subjects {
            self.mark_subject_incomplete(&subject).await;
        }
        let Some(attached) = self.attached.clone() else {
            return;
        };
        let staging_directory = attached.directory.clone();
        self.jobs.clear();
        let directory = source_directory(&self.root);
        let result = async {
            cleanup_staging(&self.root, staging_directory.as_deref(), &HashSet::new())
                .await
                .map_err(|error| error.to_string())?;
            // The cancelled jobs stay gone however many files remain.
            remove_file_if_present(&directory.join(QUEUE_FILE)).await?;
            remove_file_if_present(&directory.join(QUEUE_PART_FILE)).await?;
            let mut after = (String::new(), String::new());
            loop {
                let records = self
                    .database
                    .download_files_page(&after, None, false)
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
        self.publish().await;
        match result {
            Ok(()) => {
                if notify {
                    let _ = self
                        .events
                        .send(DownloadEvent::Notice("Removed all downloads".to_string()))
                        .await;
                }
            }
            Err(error) => {
                warn!(%error, "could not clear source downloads");
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

    pub(super) async fn persist_and_publish(&self) {
        if self.attached.is_some()
            && let Err(error) = persist_queue(&self.root, &self.jobs).await
        {
            warn!(%error, "could not save the download queue");
        }
        self.publish().await;
    }

    pub(super) async fn publish(&self) {
        let previous = self
            .events
            .status
            .borrow()
            .queue
            .as_ref()
            .map(|queue| queue.downloaded_tracks);
        let changed = self.events.counts_dirty.swap(false, Ordering::AcqRel);
        let downloaded_tracks = match previous {
            Some(previous) if !changed => previous,
            _ => self.database.downloaded_count().await.unwrap_or_default() as u64,
        };
        let _ = self
            .events
            .send(DownloadEvent::Queue {
                summary: DownloadQueueSummary {
                    total_jobs: self.jobs.len() as u64,
                    downloaded_tracks,
                },
                paused: self.paused,
            })
            .await;
    }

    pub(super) async fn queue_page(&self, offset: u64, limit: u32) -> Result<DownloadPage, String> {
        let total = self.jobs.len() as u64;
        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        let jobs = self
            .jobs
            .iter()
            .skip(offset)
            .take(limit.min(128) as usize)
            .map(DownloadJob::queue_item)
            .collect::<Vec<_>>();
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
