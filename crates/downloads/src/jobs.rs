use super::*;

impl Actor {
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
