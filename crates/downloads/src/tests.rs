use super::*;
use library::TrackKey;

async fn download_fixture(
    source_id: &str,
    track_object_id: &str,
) -> (tempfile::TempDir, Database, SourceKey, TrackKey) {
    let directory = tempfile::tempdir().expect("temporary download fixture");
    let database = Database::open(&directory.path().join("library.sqlite3"))
        .await
        .expect("open Library");
    let source = database
        .reconcile_source(&library::SourceId::new(source_id), "server", "Source")
        .await
        .unwrap()
        .source;
    database
        .catalog(source_id, None)
        .await
        .unwrap()
        .apply_catalog(
            &library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    object_id: track_object_id.to_owned(),
                    metadata: library::TrackMetadata {
                        title: "Track".into(),
                        artist: "Artist".into(),
                        album: "Album".into(),
                        sort_text: "track".into(),
                        duration_millis: 180_000,
                        disc_number: 1,
                        track_number: 1,
                        source_format: Some("FLAC".into()),
                        ..Default::default()
                    },
                    loudness_analysis_key: [7; 32],
                    artwork_binding: None,
                    ..Default::default()
                }],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    let track = database
        .catalog_keys(
            library::CatalogKind::Track,
            Some(source),
            &[(track_object_id).to_string()],
            &library::ReadCancellation::new(),
        )
        .await
        .map(|keys| keys.into_iter().next().map(library::TrackKey::from_raw))
        .expect("read Track key")
        .expect("Track exists");
    (directory, database, source, track)
}

#[tokio::test]
async fn queued_media_survives_restart_serialization() {
    let source_id = SourceId::new("source");
    let (root, database, source, _) = download_fixture("source", "track").await;
    let media_uri = reqwest::Url::from_file_path(root.path().join("track.flac"))
        .unwrap()
        .to_string();
    database
        .catalog(source_id.as_str(), None)
        .await
        .unwrap()
        .apply_catalog(
            &library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    object_id: "track".into(),
                    media_uri: Some(media_uri.clone()),
                    metadata: library::TrackMetadata {
                        title: "Track".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    let direct = "https://media.example/direct.flac".to_string();
    let queue = QueueFile {
        version: QUEUE_VERSION,
        jobs: vec![DownloadJob {
            id: "job".to_string(),
            subject: DownloadSubject::for_media_uris(
                "track",
                Some("Track"),
                std::slice::from_ref(&media_uri),
            ),
            quality: DownloadQuality::Original,
            replace_unknown: false,
            completed: Vec::new(),
            failed: false,
            remaining: vec![media_uri.clone(), direct.clone()].into(),
            state: DownloadQueueState::Queued,
        }],
    };
    let restored: QueueFile =
        serde_json::from_slice(&serde_json::to_vec(&queue).expect("encode Queue"))
            .expect("decode Queue");
    assert_eq!(
        restored.jobs[0].remaining,
        [media_uri.clone(), direct.clone()]
    );
    persist_queue(root.path(), None, &restored.jobs)
        .await
        .unwrap();
    let mut already_saved = restored.jobs[0].clone();
    already_saved.remaining = vec![media_uri.clone()].into();
    persist_queue(root.path(), Some(&source_id), &[already_saved])
        .await
        .unwrap();
    let old = staging_paths(root.path(), None, &media_uri, None);
    let http = staging_paths(root.path(), None, &direct, None);
    std::fs::create_dir_all(old.audio_part.parent().unwrap()).unwrap();
    std::fs::write(&old.audio_part, b"old partial").unwrap();
    std::fs::write(&http.audio_part, b"http partial").unwrap();
    for _ in 0..2 {
        let direct_queue = load_direct_queue(root.path(), &database).await.unwrap();
        assert_eq!(direct_queue[0].remaining, std::slice::from_ref(&direct));
        let ordinary = load_queue(root.path(), &source_id).await.unwrap();
        assert_eq!(ordinary.len(), 1);
        assert_eq!(ordinary[0].remaining, std::slice::from_ref(&media_uri));
    }
    assert!(!old.audio_part.exists());
    assert!(http.audio_part.exists());
    let mut actor = actor_for_test(root.path(), database, &source_id, source);
    let mut settings = SourceDownloadSettings::for_source(source_id.clone());
    settings.quality = DownloadQuality::Transcoded(library::AudioEncoding {
        codec: library::AudioCodec::Opus,
        bitrate_kbps: Some(64),
        channels: library::AudioChannels::Preserve,
    });
    actor.settings.insert(source_id.clone(), settings.clone());
    actor
        .attach(source_id.clone(), source, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        actor.jobs[&Some(source_id.clone())][0].quality,
        settings.quality
    );
    let paths = download_paths(root.path(), None, &media_uri);
    let audio = paths.audio.native().unwrap();
    std::fs::write(audio, b"saved audio").unwrap();
    let record = ReleasedDownloadRecord {
        version: RECORD_VERSION,
        media_uri: media_uri.clone(),
        quality: Some(DownloadQuality::Original),
        owners: HashSet::from([DownloadOwner::Retained]),
        audio: RecordedAudio::Native {
            storage_root: None,
            custom_storage: false,
            relative_audio_path: Some(audio.file_name().unwrap().into()),
        },
        retired_documents: Vec::new(),
        completed_size: Some(11),
    };
    std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    migrate_download_records(root.path(), &actor.database, None, None)
        .await
        .unwrap();
    let receipt = actor
        .database
        .download_file(&media_uri, "original")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.source_id.as_deref(), Some(source_id.as_str()));
    assert_eq!(std::fs::read(audio).unwrap(), b"saved audio");
    assert_eq!(
        actor
            .database
            .downloaded_count(Some(source_id.as_str()))
            .await
            .unwrap(),
        1
    );
    actor
        .apply(
            Command::Remove {
                media_uris: vec![media_uri],
                notify: false,
            },
            &mut Vec::new(),
        )
        .await;
    assert!(actor.jobs[&Some(source_id)].is_empty());
}

#[tokio::test]
async fn direct_http_media_uses_the_application_download_queue_without_a_source() {
    let (_library, database, source_key, _) = download_fixture("unused", "track").await;
    let root = tempfile::Builder::new()
        .prefix("downloads café %")
        .tempdir()
        .expect("Downloads root");
    let source_id = SourceId::new("unused");
    let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
    let media_uri = "https://media.example/direct.flac".to_string();
    let (events, received) = async_channel::unbounded();
    actor.events.sender = events;
    actor
        .apply(
            Command::Download {
                subject: DownloadSubject::Prepared {
                    context_id: "direct".to_string(),
                    title: Some("Direct".to_string()),
                },
                media_uris: vec![media_uri.clone()],
            },
            &mut Vec::new(),
        )
        .await;

    assert_eq!(
        actor.jobs[&None][0].remaining,
        std::slice::from_ref(&media_uri)
    );
    let queue = std::fs::read(source_directory(root.path(), None).join(QUEUE_FILE))
        .expect("read direct Queue");
    let queue: QueueFile = serde_json::from_slice(&queue).expect("decode direct Queue");
    assert_eq!(queue.jobs[0].remaining, std::slice::from_ref(&media_uri));
    let DownloadEvent::Queue { summary, .. } = received.recv().await.unwrap() else {
        panic!("Queue event")
    };
    assert!(summary.source_id.is_none());
    let page = actor
        .queue_page(DownloadQueueScope::Direct, 0, 1)
        .await
        .unwrap();
    assert_eq!(page.jobs[0].preview_uris, std::slice::from_ref(&media_uri));
    let DownloadEvent::Feedback(feedback) = received.recv().await.unwrap() else {
        panic!("feedback event")
    };
    assert_eq!(feedback.preview_uris, std::slice::from_ref(&media_uri));

    let paths = download_paths(root.path(), None, &media_uri);
    std::fs::create_dir_all(&paths.directory).expect("create download directory");
    std::fs::write(&paths.audio_part, b"complete bytes").expect("write completed transfer");
    let job = actor.jobs[&None][0].clone();
    let subject = job.subject.clone();
    let active = ActiveDownload {
        source_id: None,
        job_id: job.id,
        media_uri: media_uri.clone(),
        subject: job.subject,
        paths: paths.clone(),
        quality: DownloadQuality::Original,
        cancellation: None,
        task: tokio::spawn(async { Ok(()) }),
    };
    actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;

    assert_eq!(
        received.recv().await.unwrap(),
        DownloadEvent::Changed {
            media_uri: media_uri.clone(),
            downloaded: true,
        }
    );
    let access = actor.database.playback_access(&media_uri).await.unwrap();
    assert_eq!(
        reqwest::Url::parse(access.as_deref().unwrap())
            .unwrap()
            .to_file_path()
            .unwrap(),
        paths.audio.native().unwrap()
    );
    assert_eq!(
        received.recv().await.unwrap(),
        DownloadEvent::SubjectChanged {
            subject: subject.clone(),
            downloaded: true,
        }
    );
    let DownloadEvent::Queue { summary, .. } = received.recv().await.unwrap() else {
        panic!("completed Queue event")
    };
    assert_eq!(summary.total_jobs, 0);
    assert_eq!(summary.downloaded_tracks, 1);
    for source_id in ["unused", "missing"] {
        let count = actor
            .database
            .downloaded_count(Some(source_id))
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    actor
        .apply(
            Command::Remove {
                media_uris: vec![media_uri.clone()],
                notify: false,
            },
            &mut Vec::new(),
        )
        .await;
    assert_eq!(
        received.recv().await.unwrap(),
        DownloadEvent::Changed {
            media_uri: media_uri.clone(),
            downloaded: false,
        }
    );
    let access = actor.database.playback_access(&media_uri).await.unwrap();
    assert!(access.is_none());
    assert!(!paths.audio.native().unwrap().exists());
    assert_eq!(
        received.recv().await.unwrap(),
        DownloadEvent::SubjectChanged {
            subject,
            downloaded: false,
        }
    );
    let DownloadEvent::Queue { summary, .. } = received.recv().await.unwrap() else {
        panic!("removed Queue event")
    };
    assert_eq!(summary.downloaded_tracks, 0);
}

#[tokio::test]
async fn collection_completion_follows_its_download_jobs_across_sources() {
    for fail_first in [false, true] {
        let (_directory, database, source_key, _) = download_fixture("source", "track").await;
        let root = tempfile::tempdir().unwrap();
        let source = SourceId::new("source");
        let mut actor = actor_for_test(root.path(), database, &source, source_key);
        let (events, received) = async_channel::unbounded();
        actor.events.sender = events;
        let subject = DownloadSubject::Prepared {
            context_id: "playlist:1".into(),
            title: None,
        };
        let remote = library::source_entity_uri(&source, "track", "track");
        let direct = "https://example.org/track.flac".to_string();
        actor
            .apply(
                Command::Download {
                    subject: subject.clone(),
                    media_uris: vec![remote.clone(), direct.clone()],
                },
                &mut Vec::new(),
            )
            .await;
        while received.try_recv().is_ok() {}

        for (index, (group, uri)) in [(Some(source.clone()), remote), (None, direct)]
            .into_iter()
            .enumerate()
        {
            let job = actor.jobs[&group][0].clone();
            let paths = download_paths(root.path(), group.as_ref(), &uri);
            std::fs::create_dir_all(&paths.directory).unwrap();
            std::fs::write(&paths.audio_part, b"completed transfer").unwrap();
            let active = ActiveDownload {
                source_id: group,
                job_id: job.id,
                media_uri: uri,
                subject: job.subject,
                paths,
                quality: DownloadQuality::Original,
                cancellation: None,
                task: tokio::spawn(async { Ok(()) }),
            };
            let result = if index == 0 && fail_first {
                Err(DownloadFailure::Item("unavailable track".into()))
            } else {
                Ok(())
            };
            actor.finish(active, Ok(result), &mut Vec::new()).await;
            let mut completed = 0;
            while let Ok(event) = received.try_recv() {
                if event
                    == (DownloadEvent::SubjectChanged {
                        subject: subject.clone(),
                        downloaded: true,
                    })
                {
                    completed += 1;
                }
            }
            assert_eq!(completed, usize::from(index == 1 && !fail_first));
            if index == 0 && fail_first {
                let saved: QueueFile = serde_json::from_slice(
                    &std::fs::read(source_directory(root.path(), None).join(QUEUE_FILE)).unwrap(),
                )
                .unwrap();
                assert!(
                    saved.jobs[0].failed,
                    "failure remains part of the pending download after restart"
                );
            }
        }
    }
}

fn actor_for_test(
    root: &Path,
    database: Database,
    source_id: &SourceId,
    source_key: SourceKey,
) -> Actor {
    let (events, _) = async_channel::unbounded();
    let (prepared_rules, _) = async_channel::unbounded();
    Actor {
        artwork: artwork::Artwork::new(root.join("artwork"), tokio::runtime::Handle::current())
            .unwrap(),
        root: Arc::new(root.to_path_buf()),
        database,
        events: DownloadEvents {
            sender: events,
            status: tokio::sync::watch::channel(DownloadStatus::default()).0,
            counts_dirty: Mutex::new(HashSet::new()),
        },
        transfers: Arc::new(TransferClients::default()),
        prepared_rules,
        attached: HashMap::from([(
            source_id.clone(),
            AttachedSource {
                source_key,
                source: None,
                folder: None,
                directory: None,
            },
        )]),
        settings: HashMap::new(),
        running_rules: None,
        pending_rules: None,
        jobs: HashMap::new(),
        paused: false,
        next_job: 0,
    }
}

fn test_media(source_id: &SourceId, _source_key: SourceKey, _track: TrackKey) -> String {
    library::source_entity_uri(source_id, "track", "track")
}

fn test_job(media_uri: String) -> DownloadJob {
    DownloadJob {
        id: "job".to_string(),
        subject: DownloadSubject::for_media_uris(
            "track",
            Some("Track"),
            std::slice::from_ref(&media_uri),
        ),
        quality: DownloadQuality::Original,
        replace_unknown: false,
        completed: Vec::new(),
        failed: false,
        remaining: vec![media_uri].into(),
        state: DownloadQueueState::Downloading,
    }
}

#[tokio::test]
async fn pausing_cancels_active_work_and_keeps_the_job_queued() {
    let (_library, database, source_key, track) = download_fixture("pause", "track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let source_id = SourceId::new("pause");
    let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
    let media_uri = test_media(&source_id, source_key, track);
    actor
        .jobs
        .insert(Some(source_id.clone()), vec![test_job(media_uri.clone())]);
    let paths = download_paths(root.path(), Some(&source_id), &media_uri);
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = cancelled.await;
        Err(DownloadFailure::Retry("cancelled".to_string()))
    });
    let mut active = vec![ActiveDownload {
        source_id: Some(source_id.clone()),
        job_id: "job".to_string(),
        media_uri: media_uri.clone(),
        subject: DownloadSubject::for_media_uris(
            "track",
            Some("Track"),
            std::slice::from_ref(&media_uri),
        ),
        paths,
        quality: DownloadQuality::Original,
        cancellation: Some(cancel),
        task,
    }];

    actor.apply(Command::SetPaused(true), &mut active).await;

    assert!(actor.paused);
    assert!(active.is_empty());
    assert_eq!(
        actor.jobs[&Some(source_id)][0].state,
        DownloadQueueState::Queued
    );
}

#[tokio::test]
async fn cancelling_removes_the_job_and_reconciles_its_staging_file() {
    let (_library, database, source_key, track) = download_fixture("cancel", "track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let source_id = SourceId::new("cancel");
    let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
    let media_uri = test_media(&source_id, source_key, track);
    actor
        .jobs
        .insert(Some(source_id.clone()), vec![test_job(media_uri.clone())]);
    let paths = download_paths(root.path(), Some(&source_id), &media_uri);
    std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
        .expect("create staging");
    std::fs::write(&paths.audio_part, b"partial").expect("write staging");
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = cancelled.await;
        Err(DownloadFailure::Retry("cancelled".to_string()))
    });
    let mut active = vec![ActiveDownload {
        source_id: Some(source_id.clone()),
        job_id: "job".to_string(),
        media_uri: media_uri.clone(),
        subject: DownloadSubject::for_media_uris(
            "track",
            Some("Track"),
            std::slice::from_ref(&media_uri),
        ),
        paths: paths.clone(),
        quality: DownloadQuality::Original,
        cancellation: Some(cancel),
        task,
    }];

    actor.cancel(Some(&source_id), "job", &mut active).await;

    assert!(actor.jobs[&Some(source_id)].is_empty());
    assert!(active.is_empty());
    assert!(!paths.audio_part.exists());
}

#[tokio::test]
async fn stale_completed_transfer_cannot_commit_after_its_job_was_removed() {
    let (_library, database, source_key, track) = download_fixture("stale", "track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let source_id = SourceId::new("stale");
    let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
    let media_uri = test_media(&source_id, source_key, track);
    let paths = download_paths(root.path(), Some(&source_id), &media_uri);
    std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
        .expect("create staging");
    std::fs::write(&paths.audio_part, b"complete bytes").expect("write completed staging");
    let active = ActiveDownload {
        source_id: Some(source_id.clone()),
        job_id: "removed".to_string(),
        media_uri: media_uri.clone(),
        subject: DownloadSubject::for_media_uris(
            "track",
            Some("Track"),
            std::slice::from_ref(&media_uri),
        ),
        paths: paths.clone(),
        quality: DownloadQuality::Original,
        cancellation: None,
        task: tokio::spawn(async { Ok(()) }),
    };

    actor.finish(active, Ok(Ok(())), &mut Vec::new()).await;

    assert!(!paths.audio.native().unwrap().exists());
    assert!(paths.audio_part.exists());
}

#[tokio::test]
async fn authoritative_rule_change_is_retained_while_prior_rules_prepare() {
    let (_library, database, source_key, _) = download_fixture("rules", "track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let source_id = SourceId::new("rules");
    let mut actor = actor_for_test(root.path(), database, &source_id, source_key);
    let mut first = DownloadRules::default();
    first.set(DownloadRule::Favorites, true);
    actor.schedule_rules(&source_id, first, true);
    assert!(actor.running_rules.is_some());
    let mut replacement = DownloadRules::default();
    replacement.set(DownloadRule::LatestFiveAlbums, true);
    actor.schedule_rules(&source_id, replacement, true);

    assert_eq!(
        actor.pending_rules.as_ref().map(|intent| intent.rules),
        Some(replacement)
    );
}
