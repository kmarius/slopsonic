fn subsonic_configuration(name: impl Into<String>) -> SourceConfiguration {
    SourceConfiguration {
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

/// A Navidrome stand-in that answers every request for `username`; returns its port.
async fn fake_server(username: &'static str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let body = format!(
                    r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","type":"navidrome","user":{{"username":"{username}"}}}}}}"#
                );
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
    port
}

fn server_setup(url: String, name: Option<&str>) -> SourceSetup {
    SourceSetup {
        authentication: sources::SubsonicAuthentication::Password,
        credentials: sources::CredentialInput {
            source_name: name.map(str::to_owned),
            server_url: url,
            username: "listener".into(),
            secret: "pw".into(),
            trust_invalid_cert: false,
        },
    }
}

#[tokio::test]
async fn adding_the_first_server_connects() {
    let port = fake_server("listener").await;
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap(),
    );
    let runtime = tokio::runtime::Handle::current();
    let (events, _receiver) = async_channel::unbounded();
    let owner = ServerOwner::open_dormant(
        Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
        Arc::clone(&database),
        Downloads::new(
            artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                .unwrap(),
            directory.path().join("downloads"),
            database.as_ref().clone(),
            runtime.clone(),
            async_channel::unbounded().0,
            Default::default(),
        ),
        SettingsFile::memory(),
        runtime,
        ServerOutputs { events },
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        owner
            .configure_server(server_setup(format!("http://127.0.0.1:{port}"), None))
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
    database.reconcile_server("Test").await.unwrap();
    let runtime = tokio::runtime::Handle::current();
    let (events, receiver) = async_channel::unbounded();
    let owner = ServerOwner::open_dormant(
        Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
        Arc::clone(&database),
        Downloads::new(
            artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                .unwrap(),
            directory.path().join("downloads"),
            database.as_ref().clone(),
            runtime.clone(),
            async_channel::unbounded().0,
            Default::default(),
        ),
        SettingsFile::memory(),
        runtime,
        ServerOutputs { events },
    );

    let music = directory.path().join("music");
    std::fs::create_dir(&music).unwrap();
    let configuration = subsonic_configuration("Test");
    let configured = ConfiguredServer {
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
    let first = owner.shared.begin_refresh(false, true).unwrap();
    assert!(owner.shared.begin_refresh(false, true).is_none());
    owner.shared.present_refresh();
    owner.shared.begin_selection();
    assert!(owner.shared.refresh_is_current(&first));
    owner.shared.refresh_progress(
        &first,
        SourceReadProgress {
            stage: SourceReadStage::Tracks,
            completed: 128,
            total: None,
        },
    );
    assert!(matches!(
        &*owner.shared.operation.borrow(),
        ServerOperation::Refreshing { progress } if progress.completed == 128
    ));
    owner.finish_server_refresh(&first, Ok(())).await;
    for error in [
        SourceError::Network("connection refused".into()),
        SourceError::Cancelled,
        SourceError::Auth("Login expired".into()),
        SourceError::Network("connection timed out".into()),
    ] {
        while receiver.try_recv().is_ok() {}
        owner.publish_operation(ServerOperation::Idle).await;
        let token = owner.shared.begin_refresh(false, false).unwrap();
        owner.shared.present_refresh();
        owner.shared.refresh_progress(
            &token,
            SourceReadProgress {
                stage: SourceReadStage::Tracks,
                completed: 128,
                total: None,
            },
        );
        assert!(matches!(
            *owner.shared.operation.borrow(),
            ServerOperation::Idle
        ));
        owner.finish_server_refresh(&token, Err(error)).await;
        assert!(matches!(
            *owner.shared.operation.borrow(),
            ServerOperation::Idle
        ));
        while let Ok(event) = receiver.try_recv() {
            assert!(!matches!(
                event,
                ServerEvent::Operation(ServerOperation::Failed { .. })
            ));
        }
    }
    owner
        .finish_refresh(Err(SourceError::Network("connection refused".into())))
        .await;
    assert!(matches!(&*owner.shared.operation.borrow(),
        ServerOperation::Failed { message, .. } if message == "Server is unreachable"));
    let token = owner.shared.begin_refresh(false, true).unwrap();
    owner.shared.present_refresh();
    owner
        .finish_server_refresh(&token, Err(SourceError::Auth("Login expired".into())))
        .await;
    assert!(matches!(
        *owner.shared.operation.borrow(),
        ServerOperation::Failed { .. }
    ));

    let selected_library = SelectedLibrary {
        music_folder_key: None,
        music_folder_object_id: None,
        music_folders: Arc::from([]),
    };
    let client = owner.client().unwrap();
    assert!(owner.start_observer(client.clone()));
    let observer = owner
        .shared
        .client
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .observer
        .as_ref()
        .unwrap()
        .0
        .clone();
    *owner.shared.selected.lock().unwrap() = Some(selected_library.clone());
    owner.release_selected().await;
    assert!(!observer.cancellation().load(Ordering::Acquire));
    assert!(!owner.start_observer(client));
    for (metadata, activity, artwork, selected, visible) in [
        (false, false, false, true, false),
        (false, false, true, true, false),
        (true, false, false, true, false),
        (true, false, false, false, false),
        (false, true, false, true, false),
        (false, true, false, false, false),
    ] {
        owner.publish_operation(ServerOperation::Idle).await;
        *owner.shared.selected.lock().unwrap() = selected.then(|| selected_library.clone());
        while receiver.try_recv().is_ok() {}
        let configuration = owner.configuration_changes();
        let catalog = owner.catalog_changes();
        owner
            .catalog_updated(CatalogUpdate {
                metadata,
                activity,
                playlists: false,
                artwork,
                home: false,
                track_rekeys: Arc::from([]),
            })
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
                ServerEvent::Operation(ServerOperation::Refreshing { .. })
            );
            published |= matches!(event, ServerEvent::CatalogPublished(_));
        }
        assert_eq!(refreshing, visible);
        assert_eq!(published, metadata || artwork || activity);
    }
    let mut changes = owner.catalog_changes();
    let before = changes.borrow().clone();
    for (metadata, activity) in [(true, false), (false, true)] {
        owner
            .catalog_updated(CatalogUpdate {
                metadata,
                activity,
                ..Default::default()
            })
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
            .catalog_updated(CatalogUpdate {
                metadata: false,
                activity: false,
                playlists: true,
                artwork,
                home,
                track_rekeys: Arc::from([]),
            })
            .await;
        let ServerEvent::CatalogPublished(publication) = receiver.recv().await.unwrap() else {
            panic!("scan changes publish catalog content");
        };
        assert_eq!(publication.change, expected);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_selection_and_favorites_work_while_the_server_is_unreachable() {
    {
        let directory = tempfile::tempdir().unwrap();
        let database = Arc::new(
            Database::open(directory.path().join("library.sqlite"))
                .await
                .unwrap(),
        );
        database.reconcile_server("Music").await.unwrap();
        let update = {
            let mut catalog = database.catalog(None).await.unwrap();
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
        database.publish_catalog_update(update).await;
        let settings = SettingsFile::memory();
        settings.update(|stored| {
        stored.sources.configured = Some(ConfiguredServer {
            configuration: SourceConfiguration {
                kind: sources::SubsonicFlavor::Subsonic,
                name: "Music".into(),
                provider_payload: serde_json::json!({ "version": 1, "base_url": "http://127.0.0.1:9", "username": "listener", "trust_invalid_cert": false }).to_string(),
            },
            credential: Some("salt:token".into()),
            music_folder_id: None,
        });
        Ok(())
    }).unwrap();
        let runtime = tokio::runtime::Handle::current();
        let (events, receiver) = async_channel::unbounded();
        let owner = ServerOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
                artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                    .unwrap(),
                directory.path().join("downloads"),
                database.as_ref().clone(),
                runtime.clone(),
                async_channel::unbounded().0,
                Default::default(),
            ),
            settings,
            runtime,
            ServerOutputs { events },
        );
        owner.shared.load_source_counts().await.unwrap();
        assert_eq!(
            owner.shared.server_status(None).unwrap().cache.album_count,
            1
        );
        let completion = owner.select_server();
        tokio::time::timeout(Duration::from_secs(2), completion)
            .await
            .expect("selection result must not wait for the server")
            .unwrap();
        let published = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let ServerEvent::Selected {
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
        .expect("cached selection must not wait for the server");
        assert_eq!(published.music_folders[0].name, "New Folder");
        let connection = owner
            .shared
            .connection
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let favorite = FavoriteTarget::Album(library::entity_uri("album", "album"));
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
            let mut catalog = database.catalog(None).await.unwrap();
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
        database.publish_catalog_update(update).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let ServerEvent::CatalogPublished(CatalogPublication {
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
        assert_eq!(
            owner.selected_library().unwrap().music_folders[0].object_id,
            "new-folder"
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while owner.cached_client().is_none() || !connection.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("live access is attempted");
        assert_eq!(
            owner.selected_library().unwrap().music_folders[0].object_id,
            "new-folder"
        );
        assert!(
            database
                .cached_server(&ReadCancellation::new())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            (
                owner.shared.server_status(None).unwrap().cache.album_count,
                owner.shared.server_status(None).unwrap().cache.track_count,
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
        settings
            .update(|stored| {
                stored.sources.configured = Some(ConfiguredServer {
                    configuration: subsonic_configuration("first"),
                    credential: None,
                    music_folder_id: None,
                });
                Ok(())
            })
            .unwrap();
        database.reconcile_server("first").await.unwrap();
        let runtime = tokio::runtime::Handle::current();
        let (events, receiver) = async_channel::unbounded();
        let receiver = keep_receiver.then_some(receiver);
        let owner = ServerOwner::open_dormant(
            Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
            Arc::clone(&database),
            Downloads::new(
                artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                    .unwrap(),
                directory.path().join("downloads"),
                database.as_ref().clone(),
                runtime.clone(),
                async_channel::unbounded().0,
                Default::default(),
            ),
            settings.clone(),
            runtime,
            ServerOutputs { events },
        );
        assert!(owner.server_status().is_some());
        assert!(owner.selected_library().is_none());
        // A pending playlist edit must not hold up source selection.
        let lane = owner.shared.lane.lock().await;
        let mut selection = Box::pin(owner.select_server());
        assert!(futures_util::poll!(selection.as_mut()).is_pending());
        tokio::time::timeout(Duration::from_secs(5), selection)
            .await
            .expect("selection completes without consuming events")
            .unwrap();
        drop(lane);
        // A reset cancelling a selection waits for an install already past its check.
        let intent = owner.shared.begin_selection();
        let shared = Arc::clone(&owner.shared);
        let canceller = owner
            .shared
            .install_if_current(&intent, move || {
                let canceller = std::thread::spawn(move || shared.cancel_selection());
                std::thread::sleep(Duration::from_millis(50));
                assert!(!canceller.is_finished());
                canceller
            })
            .expect("the current selection installs");
        canceller.join().unwrap();
        assert!(owner.shared.install_if_current(&intent, || ()).is_none());
        tokio::time::timeout(Duration::from_secs(5), owner.forget_server())
            .await
            .expect("removal completes without consuming events")
            .unwrap();
        assert!(owner.server_status().is_none());
        assert!(owner.shared.selected().is_none());
        assert!(
            database
                .cached_server(&ReadCancellation::new())
                .await
                .unwrap()
                .is_none()
        );
        if let Some(receiver) = receiver {
            let mut assignments = Vec::new();
            while let Ok(event) = receiver.try_recv() {
                match event {
                    ServerEvent::Selected { selected, .. } => {
                        assignments.push(Some(selected.music_folder_key))
                    }
                    ServerEvent::ReleaseSelected => assignments.push(None),
                    _ => {}
                }
            }
            assert_eq!(assignments.len(), 2);
            assert!(assignments[0].is_some() && assignments[1].is_none());
        }
        // A selection overtaken by the forget restates the release, again without a consumer.
        tokio::time::timeout(Duration::from_secs(5), owner.restate_selection())
            .await
            .expect("restating completes without consuming events");
        assert!(owner.shared.selected().is_none());
        owner.shared.client.lock().unwrap().take();
        owner.shared.cancel_selection();
        owner.forget_server().await.unwrap();
        assert!(owner.server_status().is_none(), "removal can be repeated");
    }
}

#[tokio::test]
async fn server_reset_drops_its_local_state_and_keeps_the_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap(),
    );
    database.reconcile_server("Server").await.unwrap();
    let album = FavoriteTarget::Album(library::entity_uri("album", "a"));
    assert!(
        database
            .queue_remote_favorite(&album, true, unix_seconds())
            .await
            .unwrap()
    );
    let settings = SettingsFile::memory();
    settings
        .update(|stored| {
            stored.sources.configured = Some(ConfiguredServer {
                configuration: subsonic_configuration("Server"),
                credential: None,
                music_folder_id: None,
            });
            let ui = &mut stored.ui;
            ui.sidebar.pins.push(crate::settings::SidebarPin::Album {
                album_id: "a".into(),
            });
            ui.sidebar.playlist_pins_imported = true;
            ui.recent_search_results
                .push(crate::settings::app::RecentSearchResult {
                    kind: crate::settings::app::RecentSearchKind::Album,
                    media_uri: album.media_uri().to_owned(),
                    title: "A".into(),
                    subtitle: String::new(),
                    artwork_binding: None,
                });
            ui.random_play.genre = Some(crate::settings::RandomPlayGenreSelection {
                music_folder_id: None,
                genre_id: "rock".into(),
            });
            ui.downloads.rules.entire_library = true;
            Ok(())
        })
        .unwrap();
    let runtime = tokio::runtime::Handle::current();
    let owner = ServerOwner::open_dormant(
        Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
        Arc::clone(&database),
        Downloads::new(
            artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                .unwrap(),
            directory.path().join("downloads"),
            database.as_ref().clone(),
            runtime.clone(),
            async_channel::unbounded().0,
            Default::default(),
        ),
        settings.clone(),
        runtime,
        ServerOutputs {
            events: async_channel::unbounded().0,
        },
    );

    let downloads = directory.path().join("downloads");
    let downloaded = downloads.join("music/track.flac");
    std::fs::create_dir_all(downloaded.parent().unwrap()).unwrap();
    std::fs::write(&downloaded, b"audio").unwrap();
    let track = library::entity_uri("track", "t");
    database
        .save_download_file(&library::DownloadFile {
            media_uri: track.clone(),
            encoding: "original".into(),
            path: url::Url::from_file_path(&downloaded).unwrap().to_string(),
            storage_root: downloads.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .await
        .unwrap();

    owner.reset_server_state().await.unwrap();

    // The downloads lane clears in the background.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while (downloaded.exists() || !database.download_files(&track).await.unwrap().is_empty())
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!downloaded.exists(), "the downloaded file is removed");
    assert!(database.download_files(&track).await.unwrap().is_empty());
    assert!(
        database
            .cached_server(&ReadCancellation::new())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        database
            .due_remote_favorites(unix_seconds() + 60, 64, &ReadCancellation::new())
            .await
            .unwrap()
            .is_empty()
    );
    let stored = settings.load();
    assert!(stored.sources.configured.is_some());
    assert!(stored.ui.sidebar.pins.is_empty());
    assert!(!stored.ui.sidebar.playlist_pins_imported);
    assert!(stored.ui.recent_search_results.is_empty());
    assert!(stored.ui.random_play.genre.is_none());
    assert!(stored.ui.downloads.rules.is_empty());
}

async fn wait<T>(receiver: async_channel::Receiver<Result<T, String>>) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(20), receiver.recv())
        .await
        .expect("the source operation finishes")
        .expect("the source operation replies")
}

#[tokio::test]
async fn editing_to_another_server_resets_and_a_same_server_edit_keeps_state() {
    let (first, second) = (fake_server("listener").await, fake_server("listener").await);
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        Database::open(directory.path().join("library.sqlite"))
            .await
            .unwrap(),
    );
    let settings = SettingsFile::memory();
    let runtime = tokio::runtime::Handle::current();
    let owner = ServerOwner::open_dormant(
        Artwork::new(directory.path().join("artwork"), runtime.clone()).unwrap(),
        Arc::clone(&database),
        Downloads::new(
            artwork::Artwork::new(directory.path().join("downloads/artwork"), runtime.clone())
                .unwrap(),
            directory.path().join("downloads"),
            database.as_ref().clone(),
            runtime.clone(),
            async_channel::unbounded().0,
            Default::default(),
        ),
        settings.clone(),
        runtime,
        ServerOutputs {
            events: async_channel::unbounded().0,
        },
    );
    wait(owner.configure_server(server_setup(format!("http://127.0.0.1:{first}"), None)))
        .await
        .map(|_| ())
        .unwrap();
    let album = FavoriteTarget::Album(library::entity_uri("album", "a"));
    let pending = || async {
        !database
            .due_remote_favorites(unix_seconds() + 60, 64, &ReadCancellation::new())
            .await
            .unwrap()
            .is_empty()
    };
    let pinned = || !settings.load().ui.sidebar.pins.is_empty();
    database
        .queue_remote_favorite(&album, true, unix_seconds())
        .await
        .unwrap();
    settings
        .update(|stored| {
            stored
                .ui
                .sidebar
                .pins
                .push(crate::settings::SidebarPin::Album {
                    album_id: "a".into(),
                });
            Ok(())
        })
        .unwrap();

    // The same server under another name and spelling of its address keeps its state.
    wait(owner.update_server(server_setup(
        format!("http://127.0.0.1:{first}/"),
        Some("Renamed"),
    )))
    .await
    .unwrap();
    assert!(pending().await);
    assert!(pinned());

    wait(owner.update_server(server_setup(format!("http://127.0.0.1:{second}"), None)))
        .await
        .unwrap();
    assert!(
        !pending().await,
        "the old server's undelivered favorites go"
    );
    assert!(!pinned(), "the old server's pins go");
    let address = settings
        .load()
        .sources
        .configured
        .as_ref()
        .and_then(|source| source.configuration.server_address());
    assert_eq!(
        address
            .as_deref()
            .map(|address| address.trim_end_matches('/')),
        Some(format!("http://127.0.0.1:{second}").as_str())
    );
}
