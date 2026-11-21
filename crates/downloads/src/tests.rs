use super::*;
use library::TrackKey;

async fn download_fixture(track_object_id: &str) -> (tempfile::TempDir, Database, TrackKey) {
    let directory = tempfile::tempdir().expect("temporary download fixture");
    let database = Database::open(&directory.path().join("library.sqlite3"))
        .await
        .expect("open Library");
    database.reconcile_server("Source").await.unwrap();
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
        completed: Vec::new(),
        failed: false,
        remaining: remaining.into(),
        state: DownloadQueueState::Queued,
    };
    persist_queue(root.path(), &[job(vec![media_uri.clone()])])
        .await
        .unwrap();
    let saved = load_queue(root.path()).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].remaining, std::slice::from_ref(&media_uri));
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
            let audio = paths.audio.clone();
            let task_audio = audio.clone();
            let active = ActiveDownload {
                job_id: job.id,
                media_uri: uri,
                subject: job.subject,
                paths,
                quality: DownloadQuality::Original,
                cancellation: None,
                task: tokio::spawn(async move { Ok(task_audio) }),
            };
            let result = if index == 0 && fail_first {
                Err(DownloadFailure::Item("unavailable track".into()))
            } else {
                Ok(audio)
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
        task: tokio::spawn({
            let audio = paths.audio.clone();
            async move { Ok(audio) }
        }),
    };

    actor
        .finish(active, Ok(Ok(paths.audio.clone())), &mut Vec::new())
        .await;

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

#[tokio::test]
async fn a_finished_download_replaces_the_other_encodings_of_its_track() {
    let (_library, database, track) = download_fixture("track").await;
    let root = tempfile::tempdir().expect("Downloads root");
    let actor = actor_for_test(root.path(), database.clone());
    let media_uri = test_media(track);
    let subject = test_job(media_uri.clone()).subject;
    let paths = download_paths(root.path(), &media_uri);
    std::fs::create_dir_all(paths.audio_part.parent().unwrap()).unwrap();
    std::fs::write(&paths.audio_part, b"original bytes").unwrap();
    actor
        .commit_transfer(&media_uri, &subject, &paths, DownloadQuality::Original)
        .await
        .unwrap_or_else(|_| panic!("commit original"));
    // The original lives in its own file, as when it was downloaded into another folder.
    let mut original = database
        .download_file(&media_uri, "original")
        .await
        .unwrap()
        .expect("original record");
    let original_path = paths.directory.join("original.audio");
    std::fs::rename(&paths.audio.path, &original_path).unwrap();
    original.path = reqwest::Url::from_file_path(&original_path)
        .unwrap()
        .to_string();
    database.save_download_file(&original).await.unwrap();

    std::fs::write(&paths.audio_part, b"opus bytes").unwrap();
    let opus = DownloadQuality::Transcoded(library::AudioEncoding {
        codec: library::AudioCodec::Opus,
        bitrate_kbps: Some(64),
        channels: library::AudioChannels::Preserve,
    });
    actor
        .commit_transfer(&media_uri, &subject, &paths, opus)
        .await
        .unwrap_or_else(|_| panic!("commit opus"));

    let files = database.download_files(&media_uri).await.unwrap();
    assert_eq!(
        files
            .iter()
            .map(|file| file.encoding.clone())
            .collect::<Vec<_>>(),
        [library::media_encoding_key(opus.encoding())]
    );
    assert!(!original_path.exists(), "the replaced original is deleted");
    assert!(paths.audio.path.exists());
}

#[tokio::test]
async fn a_folder_in_a_downloads_place_fails_the_place_check_and_is_not_removed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("track.flac");
    std::fs::create_dir(&path).unwrap();
    let audio = storage::DownloadAudio { root: None, path };
    assert!(audio.check_place().await.is_err());
    assert_eq!(audio.remove().await, Ok(false));
    assert!(audio.path.is_dir());
    let file = storage::DownloadAudio {
        root: None,
        path: root.path().join("other.flac"),
    };
    assert_eq!(file.check_place().await, Ok(()));
}
