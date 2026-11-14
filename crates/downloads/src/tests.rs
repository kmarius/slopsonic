use super::*;
use library::TrackKey;

async fn download_fixture(track_object_id: &str) -> (tempfile::TempDir, Database, TrackKey) {
    let directory = tempfile::tempdir().expect("temporary download fixture");
    let database = Database::open(&directory.path().join("library.sqlite3"))
        .await
        .expect("open Library");
    database.reconcile_source("server", "Source").await.unwrap();
    database
        .catalog(None)
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
                    audio_key: [7; 32],
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
            &[(track_object_id).to_string()],
            &library::ReadCancellation::new(),
        )
        .await
        .map(|keys| keys.into_iter().next().map(library::TrackKey::from_raw))
        .expect("read Track key")
        .expect("Track exists");
    (directory, database, track)
}

#[tokio::test]
async fn queued_media_survives_restart_serialization() {
    let (root, database, _) = download_fixture("track").await;
    let media_uri = library::entity_uri("track", "track");
    database
        .catalog(None)
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
    let job = |remaining: Vec<String>| DownloadJob {
        id: "job".to_string(),
        subject: DownloadSubject::for_media_uris("track", Some("Track"), &remaining),
        quality: DownloadQuality::Original,
        replace_unknown: false,
        completed: Vec::new(),
        failed: false,
        remaining: remaining.into(),
        state: DownloadQueueState::Queued,
    };
    let encoded = |jobs: Vec<DownloadJob>| {
        serde_json::to_vec(&QueueFile {
            version: QUEUE_VERSION,
            jobs,
        })
        .unwrap()
    };
    let former = "slopsonic:source/track/slopsonic%2Dsource%2Dabc/track".to_string();
    let direct = "https://media.example/direct.flac".to_string();
    for (folder, jobs) in [
        ("0123abcd", vec![job(vec![former.clone()])]),
        ("direct", vec![job(vec![direct.clone()])]),
    ] {
        let folder = root.path().join(folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(QUEUE_FILE), encoded(jobs)).unwrap();
    }
    for _ in 0..2 {
        merge_legacy_queues(root.path(), &database).await.unwrap();
        let ordinary = load_queue(root.path()).await.unwrap();
        assert_eq!(ordinary.len(), 1);
        assert_eq!(ordinary[0].remaining, std::slice::from_ref(&media_uri));
    }
    assert!(!root.path().join("0123abcd").exists());
    assert!(!root.path().join("direct").exists());
    let mut actor = actor_for_test(root.path(), database);
    actor.attached = None;
    actor.settings.quality = DownloadQuality::Transcoded(library::AudioEncoding {
        codec: library::AudioCodec::Opus,
        bitrate_kbps: Some(64),
        channels: library::AudioChannels::Preserve,
    });
    actor.attach(None, None).await.unwrap();
    assert_eq!(actor.jobs[0].quality, actor.settings.quality);
}

#[tokio::test]
async fn collection_completion_follows_its_download_jobs() {
    for fail_first in [false, true] {
        let (_directory, database, _) = download_fixture("track").await;
        let root = tempfile::tempdir().unwrap();
        let mut actor = actor_for_test(root.path(), database);
        let (events, received) = async_channel::unbounded();
        actor.events.sender = events;
        let subject = DownloadSubject::Prepared {
            context_id: "playlist:1".into(),
            title: None,
        };
        let remote = library::entity_uri("track", "track");
        let second = library::entity_uri("track", "second");
        actor
            .apply(
                Command::Download {
                    subject: subject.clone(),
                    media_uris: vec![remote.clone(), second.clone()],
                },
                &mut Vec::new(),
            )
            .await;
        while received.try_recv().is_ok() {}

        for (index, uri) in [remote, second].into_iter().enumerate() {
            let job = actor.jobs[0].clone();
            let paths = download_paths(root.path(), &uri);
            std::fs::create_dir_all(&paths.directory).unwrap();
            std::fs::write(&paths.audio_part, b"completed transfer").unwrap();
            let active = ActiveDownload {
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
                    &std::fs::read(source_directory(root.path()).join(QUEUE_FILE)).unwrap(),
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

fn actor_for_test(root: &Path, database: Database) -> Actor {
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
            counts_dirty: AtomicBool::new(false),
        },
        transfers: Arc::new(TransferClients::default()),
        prepared_rules,
        sources: Arc::new(|| None),
        attached: Some(AttachedSource {
            folder: None,
            directory: None,
        }),
        settings: DownloadSettings::default(),
        running_rules: None,
        pending_rules: None,
        jobs: Vec::new(),
        paused: false,
        next_job: 0,
    }
}

fn test_media(_track: TrackKey) -> String {
    library::entity_uri("track", "track")
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
    let (_library, database, track) = download_fixture("track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let mut actor = actor_for_test(root.path(), database);
    let media_uri = test_media(track);
    actor.jobs = vec![test_job(media_uri.clone())];
    let paths = download_paths(root.path(), &media_uri);
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = cancelled.await;
        Err(DownloadFailure::Retry("cancelled".to_string()))
    });
    let mut active = vec![ActiveDownload {
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
    assert_eq!(actor.jobs[0].state, DownloadQueueState::Queued);
}

#[tokio::test]
async fn cancelling_removes_the_job_and_reconciles_its_staging_file() {
    let (_library, database, track) = download_fixture("track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let mut actor = actor_for_test(root.path(), database);
    let media_uri = test_media(track);
    actor.jobs = vec![test_job(media_uri.clone())];
    let paths = download_paths(root.path(), &media_uri);
    std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
        .expect("create staging");
    std::fs::write(&paths.audio_part, b"partial").expect("write staging");
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = cancelled.await;
        Err(DownloadFailure::Retry("cancelled".to_string()))
    });
    let mut active = vec![ActiveDownload {
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

    actor.cancel("job", &mut active).await;

    assert!(actor.jobs.is_empty());
    assert!(active.is_empty());
    assert!(!paths.audio_part.exists());
}

#[tokio::test]
async fn stale_completed_transfer_cannot_commit_after_its_job_was_removed() {
    let (_library, database, track) = download_fixture("track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let mut actor = actor_for_test(root.path(), database);
    let media_uri = test_media(track);
    let paths = download_paths(root.path(), &media_uri);
    std::fs::create_dir_all(paths.audio_part.parent().expect("staging parent"))
        .expect("create staging");
    std::fs::write(&paths.audio_part, b"complete bytes").expect("write completed staging");
    let active = ActiveDownload {
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

    assert!(!paths.audio.path.exists());
    assert!(paths.audio_part.exists());
}

#[tokio::test]
async fn authoritative_rule_change_is_retained_while_prior_rules_prepare() {
    let (_library, database, _) = download_fixture("track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let mut actor = actor_for_test(root.path(), database);
    let mut first = DownloadRules::default();
    first.set(DownloadRule::Favorites, true);
    actor.settings.rules = first;
    actor.reconcile_all_rules();
    assert!(actor.running_rules.is_some());
    let mut replacement = DownloadRules::default();
    replacement.set(DownloadRule::LatestFiveAlbums, true);
    actor.settings.rules = replacement;
    actor.reconcile_all_rules();

    assert_eq!(
        actor.pending_rules.as_ref().map(|intent| intent.rules),
        Some(replacement)
    );
}
