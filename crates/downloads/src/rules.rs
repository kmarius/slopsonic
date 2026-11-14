use super::*;

impl Actor {
    pub(super) async fn apply_settings(
        &mut self,
        settings: DownloadSettings,
        active: &mut Vec<ActiveDownload>,
    ) {
        let quality = settings.quality;
        let quality_changed = self.settings.quality != quality;
        let directory = settings.directory.clone();
        self.settings = settings;
        if self
            .attached
            .as_ref()
            .is_some_and(|attached| attached.directory != directory)
        {
            self.abort_matching(active, false, |_| true).await;
            self.discard_previous_directory(&directory).await;
            if let Some(attached) = self.attached.as_mut() {
                attached.directory = directory;
            }
        }
        if quality_changed {
            self.abort_matching(active, true, |download| download.quality != quality)
                .await;
            for job in &mut self.jobs {
                job.quality = quality;
                job.replace_unknown = true;
                job.remaining.extend(job.completed.drain(..));
                job.failed = false;
                job.state = DownloadQueueState::Queued;
            }
            self.requeue_downloads(quality, false).await;
        }
        self.reconcile_all_rules();
    }

    /// Downloads again every file of the source, or only those whose audio changed on the server.
    pub(super) async fn requeue_downloads(&mut self, quality: DownloadQuality, stale: bool) {
        let Some(source_id) = self.source_id().cloned() else {
            return;
        };
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(Some(source_id.as_str()), &after, None, stale)
                .await
            {
                Ok(records) if records.is_empty() => break,
                Ok(records) => records,
                Err(error) => {
                    warn!(%error, "could not read downloads to replace");
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
                self.enqueue(subject, quality, uris, true).await;
            }
        }
    }

    pub(super) fn reconcile_all_rules(&mut self) {
        let Some(attached) = &self.attached else {
            return;
        };
        let rules = self.settings.rules;
        self.pending_rules = Some(RuleIntent {
            database: self.database.clone(),
            source_key: attached.source_key,
            folder: attached.folder,
            rules,
        });
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
        let Some(attached) = self
            .attached
            .as_ref()
            .filter(|attached| attached.source_key == intent.source_key)
        else {
            self.start_rule_preparation();
            return;
        };
        let superseded = self.pending_rules.is_some();
        let same_context = attached.folder == intent.folder;
        if !superseded && same_context {
            match prepared {
                Ok(prepared) => {
                    let quality = self.settings.quality;
                    for (rule, media_uris) in prepared {
                        self.reconcile_rule(rule, quality, media_uris, active).await;
                    }
                }
                Err(error) => {
                    warn!(%error, "could not prepare automatic downloads");
                }
            }
        }
        self.start_rule_preparation();
    }

    pub(super) async fn reconcile_rule(
        &mut self,
        rule: DownloadRule,
        quality: DownloadQuality,
        media_uris: Vec<String>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let Some(source_id) = self.source_id().cloned() else {
            warn!("ignored download reconciliation without an attached server");
            return;
        };

        let mut seen = HashSet::new();
        let media_uris = media_uris
            .into_iter()
            .filter(|media_uri| seen.insert(media_uri.clone()))
            .collect::<Vec<_>>();
        let desired = media_uris.iter().cloned().collect::<HashSet<_>>();
        let subject = DownloadSubject::Rule(rule);
        let quality_changed = self
            .jobs
            .iter()
            .any(|job| job.subject == subject && job.quality != quality);
        self.abort_matching(active, true, |download| {
            download.subject == subject
                && (quality_changed || !desired.contains(&download.media_uri))
        })
        .await;
        let active_job_id = active
            .iter()
            .find(|download| download.subject == subject)
            .map(|download| download.job_id.clone());
        let owner = DownloadOwner::Subject(subject.clone());

        let owner_key = serde_json::to_string(&owner).expect("download ownership can be stored");
        let mut after = (String::new(), String::new());
        loop {
            let records = match self
                .database
                .download_files_page(Some(source_id.as_str()), &after, Some(&owner_key), false)
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
                owners.extend(self.queued_owners_for_track(&record.media_uri, Some(&subject)));
                if let Err(error) = self.store_owners(&mut record, owners).await {
                    warn!(%error, %source_id, media_uri=%record.media_uri, "could not update rule ownership");
                }
            }
        }

        let replace_unknown = self
            .jobs
            .iter()
            .any(|job| job.subject == subject && job.replace_unknown);
        let mut completed = Vec::new();
        let mut remaining = Vec::new();
        for media_uri in &media_uris {
            match self
                .add_download_owner(media_uri, &owner, quality, replace_unknown)
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

        let jobs = &mut self.jobs;
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
                job_id(&subject, self.next_job)
            });
            let state = if active_job_id.as_deref() == Some(id.as_str()) {
                DownloadQueueState::Downloading
            } else {
                DownloadQueueState::Queued
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
        self.reconcile_staging().await;

        self.persist_and_publish().await;
        if scheduled_tracks > 0 {
            let _ = self
                .events
                .send(DownloadEvent::Feedback(DownloadFeedback {
                    subject,
                    preview_uris: media_uris.iter().take(4).cloned().collect(),
                    item_count: scheduled_tracks,
                    kind: if self.paused {
                        DownloadFeedbackKind::Queued
                    } else {
                        DownloadFeedbackKind::Started
                    },
                }))
                .await;
        }
    }
}
