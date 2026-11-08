use super::*;

impl Actor {
    pub(super) async fn attach(
        &mut self,
        source_id: SourceId,
        source_key: SourceKey,
        folder: Option<FolderKey>,
        directory: Option<DownloadDirectory>,
    ) -> Result<(), String> {
        let unchanged = self.attached.get(&source_id).is_some_and(|attached| {
            attached.directory == directory && attached.source_key == source_key
        });
        self.discard_previous_directory(&source_id, &directory)
            .await;
        self.attached.insert(
            source_id.clone(),
            AttachedSource {
                source_key,
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
        self.events
            .counts_dirty
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(group.clone());
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
            job.state = DownloadQueueState::Queued;
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
                    existing.state = DownloadQueueState::Queued;
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
                    state: DownloadQueueState::Queued,
                });
            }
        }

        self.persist_and_publish(source_id.as_ref()).await;
        (scheduled_tracks > 0).then(|| DownloadFeedback {
            subject,
            preview_uris: media_uris.iter().take(4).cloned().collect(),
            item_count: scheduled_tracks,
            kind: if self.paused {
                DownloadFeedbackKind::Queued
            } else {
                DownloadFeedbackKind::Started
            },
        })
    }
}
