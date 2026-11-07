use super::*;

impl Actor {
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
            let source = match &source_id {
                Some(source_id) => self.source(source_id).await,
                None => None,
            };
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
}
