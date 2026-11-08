fn subsonic_configuration(source_id: SourceId, name: impl Into<String>) -> SourceConfiguration {
    SourceConfiguration {
        source_id,
        kind: sources::SubsonicFlavor::Subsonic,
        name: name.into(),
        provider_payload: serde_json::json!({
            "version": 1,
            "base_url": "http://127.0.0.1:1/",
            "username": "listener",
            "trust_invalid_cert": false,
        })
        .to_string(),
    }
}

use super::*;
use secrets::SecretStore;

#[tokio::test]
async fn adding_the_first_server_connects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let body = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","user":{"username":"listener"}}}"#;
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap(),
    );
    let runtime = tokio::runtime::Handle::current();
    let (events, _receiver) = async_channel::unbounded();
    let owner = SourceOwner::open_dormant(
        Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
        Arc::clone(&database),
        Downloads::new(
            artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                .unwrap(),
            directory.path().join("downloads"),
            database.as_ref().clone(),
            runtime.clone(),
            async_channel::unbounded().0,
            Vec::new(),
        ),
        SettingsFile::memory(),
        Arc::new(secrets::MemorySecretStore::new()),
        runtime,
        SourceOutputs { events },
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        owner
            .configure_source(SourceSetup {
                kind: sources::SubsonicFlavor::Navidrome,
                authentication: sources::SubsonicAuthentication::Password,
                credentials: sources::CredentialInput {
                    source_name: None,
                    server_url: format!("http://127.0.0.1:{port}"),
                    username: "listener".into(),
                    secret: "pw".into(),
                    trust_invalid_cert: false,
                },
            })
            .recv(),
    )
    .await;
    let outcome = result.map(|r| r.map(|r| r.map(|_| ())));
    assert!(matches!(outcome, Ok(Ok(Ok(())))), "{outcome:?}");
}

#[tokio::test]
async fn acquisition_progress_belongs_to_its_source() {
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap(),
    );
    let source_id = SourceId::new("test");
    database
        .reconcile_source(&source_id, "subsonic", "Test")
        .await
        .unwrap();
    let cached = database
        .cached_source(source_id.as_str(), &ReadCancellation::new())
        .await
        .unwrap()
        .unwrap();
    let runtime = tokio::runtime::Handle::current();
    let (events, receiver) = async_channel::unbounded();
    let owner = SourceOwner::open_dormant(
        Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
        Arc::clone(&database),
        Downloads::new(
            artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                .unwrap(),
            directory.path().join("downloads"),
            database.as_ref().clone(),
            runtime.clone(),
            async_channel::unbounded().0,
            Vec::new(),
        ),
        SettingsFile::memory(),
        Arc::new(secrets::MemorySecretStore::new()),
        runtime,
        SourceOutputs { events },
    );

    let music = directory.path().join("music");
    std::fs::create_dir(&music).unwrap();
    let configuration = subsonic_configuration(source_id.clone(), "Test");
    let configured = ConfiguredSource {
        configuration: configuration.clone(),
        credential: None,
        music_folder_id: None,
    };
    owner
        .shared
        .settings
        .update(|stored| {
            stored.sources.configured = Some(configured.clone());
            Ok(())
        })
        .unwrap();
    owner.cache_connected(
        &configured,
        Arc::new(Source::open(configuration, Some("salt:token".into())).unwrap()),
    );
    let other_id = SourceId::new("other");
    database
        .reconcile_source(&other_id, "subsonic", "Other")
        .await
        .unwrap();
    let mut other = configured.clone();
    other.configuration = subsonic_configuration(other_id.clone(), "Other");
    owner.cache_connected(
        &other,
        Arc::new(Source::open(other.configuration.clone(), Some("salt:token".into())).unwrap()),
    );
    let first = owner.shared.begin_refresh(&source_id, false, true).unwrap();
    let second = owner.shared.begin_refresh(&other_id, false, true).unwrap();
    owner.shared.present_refresh(&source_id);
    owner.shared.begin_selection();
    assert!(owner.shared.refresh_is_current(&source_id, &first));
    assert!(owner.shared.refresh_is_current(&other_id, &second));
    owner.shared.refresh_progress(
        &other_id,
        &second,
        SourceReadProgress {
            stage: SourceReadStage::Tracks,
            completed: 128,
            total: None,
        },
    );
    assert!(
        matches!(&*owner.shared.operation.borrow(), SourceOperation::Refreshing {source_id: current,..} if current==&source_id)
    );
    owner
        .finish_source_refresh(&other_id, &second, Ok(()))
        .await;
    assert!(
        matches!(&*owner.shared.operation.borrow(), SourceOperation::Refreshing {source_id: current,..} if current==&source_id)
    );
    assert!(owner.shared.refresh_is_current(&source_id, &first));
    owner
        .finish_source_refresh(&source_id, &first, Ok(()))
        .await;
    for error in [
        SourceError::Network("connection refused".into()),
        SourceError::Cancelled,
        SourceError::Auth("Login expired".into()),
        SourceError::Network("connection timed out".into()),
    ] {
        while receiver.try_recv().is_ok() {}
        owner.publish_operation(SourceOperation::Idle).await;
        let token = owner
            .shared
            .begin_refresh(&source_id, false, false)
            .unwrap();
        owner.shared.present_refresh(&source_id);
        owner.shared.refresh_progress(
            &source_id,
            &token,
            SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: 128,
                total: None,
            },
        );
        assert!(matches!(
            *owner.shared.operation.borrow(),
            SourceOperation::Idle
        ));
        owner
            .finish_source_refresh(&source_id, &token, Err(error))
            .await;
        assert!(matches!(
            *owner.shared.operation.borrow(),
            SourceOperation::Idle
        ));
        while let Ok(event) = receiver.try_recv() {
            assert!(!matches!(
                event,
                SourceEvent::Operation(SourceOperation::Failed { .. })
            ));
        }
    }
    owner
        .finish_refresh(
            &source_id,
            Err(SourceError::Network("connection refused".into())),
        )
        .await;
    assert!(matches!(&*owner.shared.operation.borrow(),
        SourceOperation::Failed { message, .. } if message == "Server is unreachable"));
    let token = owner.shared.begin_refresh(&source_id, false, true).unwrap();
    owner.shared.present_refresh(&source_id);
    owner
        .finish_source_refresh(
            &source_id,
            &token,
            Err(SourceError::Auth("Login expired".into())),
        )
        .await;
    assert!(matches!(
        *owner.shared.operation.borrow(),
        SourceOperation::Failed { .. }
    ));

    let selected_library = SelectedLibrary {
        source_id: source_id.clone(),
        source_key: cached.source,
        music_folder_key: None,
        music_folder_object_id: None,
        music_folders: Arc::from([]),
    };
    let client = owner.client(&source_id).unwrap();
    assert!(owner.start_observer(selected_library.clone(), client.clone(), false));
    let observer = owner.shared.clients.lock().unwrap()[&source_id]
        .observer
        .as_ref()
        .unwrap()
        .0
        .clone();
    *owner.shared.selected.lock().unwrap() = Some(selected_library.clone());
    owner.release_selected().await;
    assert!(!observer.cancellation().load(Ordering::Acquire));
    assert!(!owner.start_observer(selected_library.clone(), client, false));
    for (metadata, activity, artwork, selected, visible) in [
        (false, false, false, true, false),
        (false, false, true, true, false),
        (true, false, false, true, false),
        (true, false, false, false, false),
        (false, true, false, true, false),
        (false, true, false, false, false),
    ] {
        owner.publish_operation(SourceOperation::Idle).await;
        *owner.shared.selected.lock().unwrap() = selected.then(|| selected_library.clone());
        while receiver.try_recv().is_ok() {}
        let configuration = owner.configuration_changes();
        let catalog = owner.catalog_changes();
        owner
            .catalog_updated(
                source_id.clone(),
                cached.source,
                CatalogUpdate {
                    metadata,
                    activity,
                    playlists: false,
                    artwork,
                    home: false,
                    track_rekeys: Arc::from([]),
                },
            )
            .await;
        assert!(
            !configuration.has_changed().unwrap(),
            "catalog data does not change source configuration"
        );
        assert_eq!(
            catalog.has_changed().unwrap(),
            metadata || artwork || activity
        );
        let mut refreshing = false;
        let mut published = false;
        while let Ok(event) = receiver.try_recv() {
            refreshing |= matches!(
                event,
                SourceEvent::Operation(SourceOperation::Refreshing { .. })
            );
            published |= matches!(event, SourceEvent::CatalogPublished(_));
        }
        assert_eq!(refreshing, visible);
        assert_eq!(published, metadata || artwork || activity);
    }
    let mut changes = owner.catalog_changes();
    let before = changes.borrow().clone();
    for (metadata, activity) in [(true, false), (false, true)] {
        owner
            .catalog_updated(
                source_id.clone(),
                cached.source,
                CatalogUpdate {
                    metadata,
                    activity,
                    ..Default::default()
                },
            )
            .await;
    }
    let revision = changes.borrow_and_update().clone();
    assert_eq!(revision.catalog, before.catalog + 1);
    assert_eq!(revision.activity, before.activity + 1);
    assert_eq!(revision.change, Some(CatalogChange::Broad));
    while receiver.try_recv().is_ok() {}
    for (artwork, home, expected) in [
        (false, true, CatalogChange::Broad),
        (true, false, CatalogChange::Broad),
        (false, false, CatalogChange::Playlists(None)),
    ] {
        owner
            .catalog_updated(
                source_id.clone(),
                cached.source,
                CatalogUpdate {
                    metadata: false,
                    activity: false,
                    playlists: true,
                    artwork,
                    home,
                    track_rekeys: Arc::from([]),
                },
            )
            .await;
        let SourceEvent::CatalogPublished(publication) = receiver.recv().await.unwrap() else {
            panic!("scan changes publish catalog content");
        };
        assert_eq!(publication.change, expected);
    }
    let mut catalog = owner.catalog_changes();
    let before = catalog.borrow().clone();
    let listen: library::ListenWrite = serde_json::from_value(serde_json::json!({
        "external_id":"direct", "media_uri":"https://audio.invalid/direct.flac",
        "title":"Direct", "artist":"", "album":"", "duration_millis":1000,
        "started_at":1_700_000_000,"local_period":"2023-11","listened_millis":1000,"skipped":false
    }))
    .unwrap();
    owner
        .shared
        .database
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    let SourceEvent::CatalogPublished(publication) = receiver.recv().await.unwrap() else {
        panic!("accepted direct listen publishes activity");
    };
    assert_eq!(publication.source_key, None);
    assert_eq!(publication.change, CatalogChange::Activity);
    let revision = catalog.borrow_and_update().clone();
    assert_eq!(revision.catalog, before.catalog);
    assert_eq!(revision.activity, before.activity + 1);
    owner
        .shared
        .database
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    assert!(!catalog.has_changed().unwrap());
    assert!(receiver.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_selection_is_published_while_credentials_are_held() {
    struct HeldSecrets {
        inner: secrets::MemorySecretStore,
        started: Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl SecretStore for HeldSecrets {
        fn load_secret(&self, key: &secrets::SecretKey) -> secrets::SecretResult<Option<String>> {
            let _ = self.started.try_send(());
            let _ = self.release.lock().unwrap().recv();
            self.inner.load_secret(key)
        }
        fn save_secret(&self, key: &secrets::SecretKey, secret: &str) -> secrets::SecretResult<()> {
            self.inner.save_secret(key, secret)
        }
        fn delete_secret(&self, key: &secrets::SecretKey) -> secrets::SecretResult<()> {
            self.inner.delete_secret(key)
        }
    }
    {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let held_inner = secrets::MemorySecretStore::new();
        let held_credential = crate::settings::seal_credential(&held_inner, "salt:token").unwrap();
        let source_id = SourceId::new("held-credential-source");
        database
            .reconcile_source(&source_id, "subsonic", "Music")
            .await
            .unwrap();
        let source_key = database
            .source_identity_key(&source_id)
            .await
            .unwrap()
            .unwrap();
        let update = {
            let mut catalog = database.catalog(source_id.as_str(), None).await.unwrap();
            catalog
                .apply_catalog(
                    &library::CatalogBatch {
                        folders: vec![library::FolderObservation {
                            object_id: "new-folder".into(),
                            metadata: library::FolderMetadata {
                                name: Some("New Folder".into()),
                                ..Default::default()
                            },
                            ..Default::default()
                        }],
                        albums: vec![library::AlbumObservation {
                            object_id: "album".into(),
                            metadata: library::AlbumMetadata {
                                title: "Cached Album".into(),
                                artist: "Artist".into(),
                                sort_text: "cached album".into(),
                                ..Default::default()
                            },
                            user_data: library::NativeUserState {
                                favorite: Some(false),
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
            catalog.into_changes()
        };
        database
            .publish_catalog_update(source_id.as_str(), update)
            .await;
        let settings = SettingsFile::memory();
        settings.update(|stored| {
        stored.sources.configured = Some(ConfiguredSource {
            configuration: SourceConfiguration {
                source_id: source_id.clone(),
                kind: sources::SubsonicFlavor::Subsonic,
                name: "Music".into(),
                provider_payload: serde_json::json!({ "version": 1, "base_url": "http://127.0.0.1:9", "username": "listener", "trust_invalid_cert": false }).to_string(),
            },
            credential: Some(held_credential),
            music_folder_id: None,
        });
        Ok(())
    }).unwrap();
        let runtime = tokio::runtime::Handle::current();
        let (started, started_receiver) = async_channel::bounded(1);
        let (release, release_receiver) = std::sync::mpsc::channel();
        let secrets: Arc<dyn SecretStore> = Arc::new(HeldSecrets {
            inner: held_inner,
            started,
            release: Mutex::new(release_receiver),
        });
        let (events, receiver) = async_channel::unbounded();
        let owner = SourceOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
                artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                    .unwrap(),
                directory.path().join("downloads"),
                database.as_ref().clone(),
                runtime.clone(),
                async_channel::unbounded().0,
                Vec::new(),
            ),
            settings,
            secrets,
            runtime,
            SourceOutputs { events },
        );
        owner.shared.load_source_counts().await.unwrap();
        assert_eq!(
            owner.shared.source_status(None).unwrap().cache.album_count,
            1
        );
        let completion = owner.select_source(source_id.clone());
        let selected = tokio::time::timeout(Duration::from_secs(2), completion)
            .await
            .expect("selection result must not wait for credentials")
            .unwrap();
        assert_eq!(selected, source_id);
        drop(selected);
        let published = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SourceEvent::Selected {
                    configured,
                    selected,
                } = receiver.recv().await.unwrap()
                {
                    assert_eq!(configured.unwrap().cache.album_count, 1);
                    break selected;
                }
            }
        })
        .await
        .expect("cached selection must not wait for credentials");
        assert_eq!(published.source_id, source_id);
        assert_eq!(published.source_key, source_key);
        assert_eq!(published.music_folders[0].name, "New Folder");
        tokio::time::timeout(Duration::from_secs(2), started_receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(owner.cached_client(&source_id).is_none());
        let connection = owner
            .shared
            .connection
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let favorite =
            FavoriteTarget::Album(library::source_entity_uri(&source_id, "album", "album"));
        for value in [true, false] {
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    owner.set_favorite_with_result(favorite.clone(), value),
                )
                .await
                .expect("favorite completion must not wait for live access")
                .unwrap(),
                value
            );
            assert_eq!(database.favorite(&favorite).await.unwrap(), value);
        }
        while receiver.try_recv().is_ok() {}
        owner
            .shared
            .settings
            .update(|stored| {
                stored.ui.application.download_artwork_automatically = true;
                Ok(())
            })
            .unwrap();
        let update = {
            let mut catalog = database.catalog(source_id.as_str(), None).await.unwrap();
            catalog
                .apply_catalog(
                    &library::CatalogBatch {
                        folders: vec![library::FolderObservation {
                            object_id: "new-folder".into(),
                            metadata: library::FolderMetadata {
                                name: Some("New Folder".into()),
                                sort_text: Some("new folder".into()),
                            },
                            ..Default::default()
                        }],
                        tracks: vec![library::TrackObservation {
                            object_id: "new-track".into(),
                            album_object_id: Some("album".into()),
                            metadata: library::TrackMetadata {
                                title: "New Track".into(),
                                album: "Cached Album".into(),
                                artist: "Artist".into(),
                                sort_text: "new track".into(),
                                duration_millis: 1_000,
                                disc_number: 1,
                                track_number: 1,
                                ..Default::default()
                            },
                            user_data: library::NativeUserState {
                                favorite: Some(false),
                                ..Default::default()
                            },
                            folders: Some(vec!["new-folder".into()]),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    true,
                )
                .await
                .unwrap();
            catalog.into_changes()
        };
        database
            .publish_catalog_update(source_id.as_str(), update)
            .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let SourceEvent::CatalogPublished(CatalogPublication {
                    change: CatalogChange::Broad,
                    ..
                }) = receiver.recv().await.unwrap()
                {
                    assert_eq!(
                        owner.selected_library().unwrap().music_folders[0].object_id,
                        "new-folder"
                    );
                    break;
                }
            }
        })
        .await
        .expect("the UI receives the updated folder snapshot");
        assert!(
            owner.cached_client(&source_id).is_none(),
            "summary refresh does not obtain credentials"
        );
        assert_eq!(
            owner.selected_library().unwrap().music_folders[0].object_id,
            "new-folder"
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while owner.cached_client(&source_id).is_none() || !connection.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("live access resumes after credential initialization");
        assert_eq!(owner.selected_library().unwrap().source_id, source_id);
        assert_eq!(
            owner.selected_library().unwrap().music_folders[0].object_id,
            "new-folder"
        );
        assert_eq!(
            database
                .cached_source(source_id.as_str(), &ReadCancellation::new())
                .await
                .unwrap()
                .unwrap()
                .source,
            source_key
        );
        assert_eq!(
            (
                owner.shared.source_status(None).unwrap().cache.album_count,
                owner.shared.source_status(None).unwrap().cache.track_count,
            ),
            (1, 1)
        );
        owner.deliver_pending_favorites().await;
        assert!(!database.favorite(&favorite).await.unwrap());
        assert!(
            database
                .due_remote_favorites(unix_seconds(), 64, &ReadCancellation::new())
                .await
                .unwrap()
                .is_empty(),
            "offline delivery is deferred without losing the local edit"
        );
        assert_eq!(
            database
                .due_remote_favorites(unix_seconds() + 60, 64, &ReadCancellation::new())
                .await
                .unwrap(),
            vec![(favorite.media_uri().to_owned(), false)]
        );
    }
}

#[tokio::test]
async fn source_switch_and_removal_complete_without_an_event_consumer() {
    for keep_receiver in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        let settings_path = directory.path().join("settings.toml");
        let settings = SettingsFile::open(settings_path.clone()).unwrap();
        let first = SourceId::new("first");
        for id in [&first] {
            settings
                .update(|stored| {
                    stored.sources.configured = Some(ConfiguredSource {
                        configuration: subsonic_configuration(id.clone(), id.as_str()),
                        credential: None,
                        music_folder_id: None,
                    });
                    Ok(())
                })
                .unwrap();
            database
                .reconcile_source(&id, "subsonic", id.as_str())
                .await
                .unwrap();
        }
        let runtime = tokio::runtime::Handle::current();
        let (events, receiver) = async_channel::unbounded();
        let receiver = keep_receiver.then_some(receiver);
        let owner = SourceOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
                artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                    .unwrap(),
                directory.path().join("downloads"),
                database.as_ref().clone(),
                runtime.clone(),
                async_channel::unbounded().0,
                Vec::new(),
            ),
            settings.clone(),
            Arc::new(secrets::MemorySecretStore::new()),
            runtime,
            SourceOutputs { events },
        );
        assert!(owner.source_status().is_some());
        assert!(owner.selected_library().is_none());
        let missing = owner.select_source(SourceId::new("missing"));
        let error = tokio::time::timeout(Duration::from_secs(5), missing)
            .await
            .unwrap()
            .err()
            .expect("unknown source returns an error to its caller");
        assert!(!error.to_string().is_empty());
        assert!(owner.selected_library().is_none());

        // A pending playlist edit must not hold up source selection.
        let lane = owner.shared.lane.lock().await;
        let mut selection = Box::pin(owner.select_source(first.clone()));
        assert!(futures_util::poll!(selection.as_mut()).is_pending());
        let selected = tokio::time::timeout(Duration::from_secs(5), selection)
            .await
            .expect("selection completes without consuming events")
            .unwrap();
        assert_eq!(selected, first);
        assert_eq!(owner.selected_library().unwrap().source_id, first);
        drop(lane);
        tokio::time::timeout(Duration::from_secs(5), owner.forget_source(first.clone()))
            .await
            .expect("removal completes without consuming events")
            .unwrap();
        assert!(owner.source_status().is_none());
        assert!(owner.shared.selected().is_none());
        assert!(
            database
                .source_identity_key(&first)
                .await
                .unwrap()
                .is_none()
        );
        if let Some(receiver) = receiver {
            let mut assignments = Vec::new();
            while let Ok(event) = receiver.try_recv() {
                match event {
                    SourceEvent::Selected { selected, .. } => {
                        assignments.push(Some(selected.source_id))
                    }
                    SourceEvent::ReleaseSelected => assignments.push(None),
                    _ => {}
                }
            }
            assert_eq!(assignments, vec![Some(first.clone()), None]);
        }
        owner.shared.clients.lock().unwrap().clear();
        owner.shared.cancel_selection();
        owner.forget_source(first.clone()).await.unwrap();
        assert!(owner.source_status().is_none(), "removal can be repeated");
    }
}
