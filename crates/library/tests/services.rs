use library::{
    DownloadAccessWrite, ListenDeliveryTarget, ListenWrite, LoudnessMeasurement, QueueProvenance,
    QueueRepeatMode, ReadCancellation,
};

use super::support::{QueueItem, connection, fixture, persist_queue};

#[tokio::test]
async fn loudness_forward_passes_revisit_invalidated_earlier_keys() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let mut measurement = LoudnessMeasurement {
        analysis_key: [0; 32],
        integrated_lufs: Some(-14.0),
        true_peak: Some(0.8),
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    let mut after = None;
    for expected in &fixture.tracks {
        let work = fixture
            .database
            .next_missing_track_loudness(fixture.source, after, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(work.track_key, *expected);
        measurement.analysis_key = work.expected_analysis_key;
        assert!(
            fixture
                .database
                .write_track_analyzed_loudness(fixture.source, work.track_key, &measurement,)
                .await
                .unwrap()
        );
        after = Some(work.track_key);
    }
    let mut connection = connection(&fixture.path).await;
    // A mapping or file change can invalidate a key behind the active cursor.
    sqlx::query("UPDATE tracks SET loudness_analysis_key=?2 WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .bind([99u8; 32].as_slice())
        .execute(&mut connection)
        .await
        .unwrap();
    assert!(
        fixture
            .database
            .next_missing_track_loudness(fixture.source, after, &cancel)
            .await
            .unwrap()
            .is_none()
    );
    let changed = fixture
        .database
        .next_missing_track_loudness(fixture.source, None, &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed.track_key, fixture.tracks[0]);
    assert_eq!(changed.expected_analysis_key, [99; 32]);
    measurement.analysis_key = changed.expected_analysis_key;
    assert!(
        fixture
            .database
            .write_track_analyzed_loudness(fixture.source, changed.track_key, &measurement,)
            .await
            .unwrap()
    );
    assert!(
        fixture
            .database
            .next_missing_track_loudness(fixture.source, None, &cancel)
            .await
            .unwrap()
            .is_none()
    );

    let mut after = None;
    let mut album_tracks = Vec::new();
    for expected in &fixture.albums {
        let work = fixture
            .database
            .next_missing_album_loudness(fixture.source, after, &cancel)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(work.album_key, *expected);
        album_tracks.extend(work.tracks.iter().map(|track| track.track_key));
        measurement.analysis_key = work.expected_analysis_key;
        assert!(
            fixture
                .database
                .write_album_analyzed_loudness(fixture.source, work.album_key, &measurement,)
                .await
                .unwrap()
        );
        after = Some(work.album_key);
    }
    assert_eq!(album_tracks, fixture.tracks);
    sqlx::query("UPDATE albums SET loudness_analysis_key=?2 WHERE album_key=?1")
        .bind(fixture.albums[0])
        .bind([98u8; 32].as_slice())
        .execute(&mut connection)
        .await
        .unwrap();
    assert!(
        fixture
            .database
            .next_missing_album_loudness(fixture.source, after, &cancel)
            .await
            .unwrap()
            .is_none()
    );
    let changed = fixture
        .database
        .next_missing_album_loudness(fixture.source, None, &cancel)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(changed.album_key, fixture.albums[0]);
    assert_eq!(changed.expected_analysis_key, [98; 32]);
    measurement.analysis_key = changed.expected_analysis_key;
    assert!(
        fixture
            .database
            .write_album_analyzed_loudness(fixture.source, changed.album_key, &measurement,)
            .await
            .unwrap()
    );
    assert!(
        fixture
            .database
            .next_missing_album_loudness(fixture.source, None, &cancel)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn loudness_selects_one_unit_and_source_facts_win() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let work = fixture
        .database
        .next_missing_track_loudness(fixture.source, None, &cancel)
        .await
        .expect("missing Track loudness")
        .unwrap();
    assert_eq!(work.track_key, fixture.tracks[0]);
    let below_gate = LoudnessMeasurement {
        analysis_key: work.expected_analysis_key,
        integrated_lufs: None,
        true_peak: Some(1.04),
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    assert!(
        fixture
            .database
            .write_track_analyzed_loudness(fixture.source, work.track_key, &below_gate)
            .await
            .expect("store completed analysis below the loudness gate")
    );
    assert_eq!(
        fixture
            .database
            .playback_loudness(&work.media_uri, &cancel)
            .await
            .unwrap()
            .0,
        Some(below_gate.clone())
    );
    assert!(
        fixture
            .database
            .r128_tag_write_page(fixture.source, None, 64, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    assert_ne!(
        fixture
            .database
            .next_missing_track_loudness(fixture.source, None, &cancel)
            .await
            .unwrap()
            .unwrap()
            .track_key,
        work.track_key
    );
    let analyzed = LoudnessMeasurement {
        analysis_key: work.expected_analysis_key,
        integrated_lufs: Some(-14.0),
        true_peak: Some(0.8),
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    assert!(
        fixture
            .database
            .write_track_analyzed_loudness(fixture.source, work.track_key, &analyzed)
            .await
            .expect("write analyzed loudness")
    );
    let tag_page = fixture
        .database
        .r128_tag_write_page(fixture.source, None, 64, &cancel)
        .await
        .expect("read Rufin-owned R128 tag page");
    assert_eq!(tag_page.len(), 1);
    assert_eq!(tag_page[0].track_key, work.track_key);
    assert_eq!(tag_page[0].track_lufs, Some(-14.0));
    assert!(
        !fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                work.track_key,
                &LoudnessMeasurement {
                    analysis_key: [2; 32],
                    integrated_lufs: Some(-8.0),
                    true_peak: Some(1.0),
                    replay_gain_db: None,
                    replay_gain_peak: None
                }
            )
            .await
            .expect("reject a stale work identity")
    );
    let source_fact = LoudnessMeasurement {
        analysis_key: work.expected_analysis_key,
        integrated_lufs: Some(-11.0),
        true_peak: None,
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    let mut input = library::TrackObservation {
        user_data: library::NativeUserState {
            favorite: Some(false),
            rating: (Some(Some(5))).map(|rating| rating.map(|value| value * 10)),
            ..Default::default()
        },
        metadata: library::TrackMetadata {
            title: "Alpha".into(),
            album: "Album A".into(),
            artist: "Artist A".into(),
            sort_text: "alpha".into(),
            duration_millis: 180_000,
            disc_number: 1,
            track_number: 1,
            year: Some(2020),
            date_added: Some("2024-01-02".into()),
            source_format: Some("FLAC".into()),
            comment: Some("Note".into()),
            bpm: Some(100),
            source_loudness: Some(library::NativeGain {
                integrated_lufs: Some(-11.0),
                ..Default::default()
            }),
            ..Default::default()
        },
        object_id: "track-0".into(),
        album_object_id: Some("album-a".into()),
        loudness_analysis_key: work.expected_analysis_key,
        ..Default::default()
    };

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    {
        let write = library::CatalogBatch {
            tracks: vec![input.clone()],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let update = scan_catalog.into_changes();
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    assert!(
        fixture
            .database
            .r128_tag_write_page(fixture.source, None, 64, &cancel)
            .await
            .expect("source R128 tag is not Rufin-owned")
            .is_empty()
    );
    assert_eq!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[0], &cancel)
            .await
            .expect("read Track loudness")
            .0,
        Some(source_fact.clone())
    );

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    input.loudness_analysis_key = [99; 32];
    input.metadata.source_loudness = None;
    input.metadata.artists = vec![library::ArtistCredit {
        object_id: "artist-a".into(),
        name: "Artist A".into(),
        ..Default::default()
    }];
    input.metadata.genres = vec![library::TagReference {
        object_id: "genre".into(),
        name: "Rock".into(),
        ..Default::default()
    }];
    input.metadata.moods = vec![library::TagReference {
        object_id: "mood".into(),
        name: "Energetic".into(),
        sort_text: Some("energetic".into()),
    }];
    {
        let write = library::CatalogBatch {
            tracks: vec![input.clone()],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = scan_catalog.into_changes();
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    let changed = fixture
        .database
        .next_missing_track_loudness(fixture.source, None, &cancel)
        .await
        .expect("changed Track loudness work")
        .unwrap();
    assert_eq!(changed.track_key, work.track_key);
    assert_ne!(changed.expected_analysis_key, work.expected_analysis_key);

    assert!(
        !fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                changed.track_key,
                &LoudnessMeasurement {
                    analysis_key: work.expected_analysis_key,
                    integrated_lufs: Some(-9.0),
                    true_peak: Some(0.7),
                    replay_gain_db: None,
                    replay_gain_peak: None
                }
            )
            .await
            .expect("reject stale analyzer completion")
    );
    let current_source = LoudnessMeasurement {
        analysis_key: changed.expected_analysis_key,
        integrated_lufs: Some(-10.0),
        true_peak: None,
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    input.metadata.source_loudness = Some(library::NativeGain {
        integrated_lufs: Some(-10.0),
        ..Default::default()
    });

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    {
        let write = library::CatalogBatch {
            tracks: vec![input.clone()],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let update = scan_catalog.into_changes();
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    assert!(
        !fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                changed.track_key,
                &LoudnessMeasurement {
                    analysis_key: changed.expected_analysis_key,
                    integrated_lufs: Some(-9.0),
                    true_peak: Some(0.7),
                    replay_gain_db: None,
                    replay_gain_peak: None
                }
            )
            .await
            .expect("do not replace newer source fact")
    );
    assert_eq!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[0], &cancel)
            .await
            .unwrap()
            .0,
        Some(current_source)
    );

    let album = fixture
        .database
        .next_missing_album_loudness(fixture.source, None, &cancel)
        .await
        .expect("missing Album loudness")
        .unwrap();
    assert_eq!(album.tracks.len(), 2);
    assert!(
        fixture
            .database
            .write_album_analyzed_loudness(
                fixture.source,
                album.album_key,
                &LoudnessMeasurement {
                    analysis_key: album.expected_analysis_key,
                    integrated_lufs: None,
                    true_peak: Some(0.0),
                    replay_gain_db: None,
                    replay_gain_peak: None,
                }
            )
            .await
            .expect("store an album below the loudness gate")
    );
    assert_ne!(
        fixture
            .database
            .next_missing_album_loudness(fixture.source, None, &cancel)
            .await
            .unwrap()
            .unwrap()
            .album_key,
        album.album_key
    );
    let album_measurement = LoudnessMeasurement {
        analysis_key: album.expected_analysis_key,
        integrated_lufs: Some(-13.0),
        true_peak: Some(0.85),
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    assert!(
        fixture
            .database
            .write_album_analyzed_loudness(fixture.source, album.album_key, &album_measurement)
            .await
            .expect("write Album loudness")
    );
    let old_album_key = album.expected_analysis_key;
    let quiet_track = album
        .tracks
        .iter()
        .find(|track| track.track_key == fixture.tracks[1])
        .unwrap();
    assert!(
        fixture
            .database
            .write_track_analyzed_loudness(
                fixture.source,
                quiet_track.track_key,
                &LoudnessMeasurement {
                    analysis_key: quiet_track.expected_analysis_key,
                    ..below_gate
                }
            )
            .await
            .unwrap()
    );
    let tag_page = fixture
        .database
        .r128_tag_write_page(fixture.source, None, 64, &cancel)
        .await
        .unwrap();
    let quiet_tags = tag_page
        .iter()
        .find(|row| row.track_key == quiet_track.track_key)
        .unwrap();
    assert_eq!(quiet_tags.track_lufs, None);
    assert_eq!(quiet_tags.album_lufs, Some(-13.0));
    fixture
        .database
        .write_download_access(
            Some(fixture.source),
            &[&DownloadAccessWrite {
                media_uri: fixture.track_uris[1].clone(),
                path: "/generic/local-track.flac".to_string(),
                root: "/generic".to_string(),
                relative_path: "local-track.flac".to_string(),
                size_bytes: 10,
                mtime_ns: 1,
                device_id: Some(2),
                inode: Some(3),
                parser_version: 1,
                access_uri: "file:///generic/local-track.flac".to_string(),
                loudness_analysis_key: Some([77; 32]),
            }],
        )
        .await
        .expect("install exact Local audio identity");
    let local_analysis = LoudnessMeasurement {
        analysis_key: [77; 32],
        integrated_lufs: Some(-18.0),
        true_peak: Some(0.6),
        replay_gain_db: None,
        replay_gain_peak: None,
    };
    assert!(
        fixture
            .database
            .write_track_analyzed_loudness(fixture.source, fixture.tracks[1], &local_analysis)
            .await
            .unwrap()
    );

    let mut scan_catalog = async {
        if (&fixture.database)
            .source_identity_key(&library::SourceId::new("source"))
            .await?
            .is_none()
        {
            (&fixture.database)
                .reconcile_source(&library::SourceId::new("source"), "subsonic", "Source")
                .await?;
        }
        fixture.database.catalog("source", None).await
    }
    .await
    .expect("begin membership-changing Inventory");
    let scan = scan_catalog.start_observation().await.unwrap();
    for (id, title, artist, sort, date) in [
        ("album-a", "Album A", "Artist A", "album a", "2024-01-02"),
        ("album-b", "Album B", "Artist B", "album b", "2023-01-02"),
    ] {
        {
            let write = library::CatalogBatch {
                albums: vec![library::AlbumObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        ..Default::default()
                    },
                    metadata: library::AlbumMetadata {
                        title: title.to_owned(),
                        artist: artist.to_owned(),
                        sort_text: sort.to_owned(),
                        year: Some(2024),
                        release_date: Some(date.to_owned()),
                        date_added: Some(date.to_owned()),
                        is_compilation: Some(id == "album-a"),
                        ..Default::default()
                    },
                    object_id: id.to_owned(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
    }
    for (index, album_id, artist, title) in [
        (0, "album-a", "Artist A", "Alpha"),
        (1, "album-a", "Artist A", "Beta"),
        (2, "album-a", "Artist B", "Gamma"),
        (3, "album-b", "Artist B", "Delta"),
    ] {
        {
            let write = library::CatalogBatch {
                tracks: vec![library::TrackObservation {
                    user_data: library::NativeUserState {
                        favorite: Some(false),
                        ..Default::default()
                    },
                    metadata: library::TrackMetadata {
                        title: title.to_owned(),
                        album: (if album_id == "album-a" {
                            "Album A"
                        } else {
                            "Album B"
                        })
                        .to_owned(),
                        artist: artist.to_owned(),
                        sort_text: title.to_lowercase(),
                        duration_millis: 180_000 + index * 1_000,
                        disc_number: 1,
                        track_number: index + 1,
                        year: Some(2020 + index),
                        date_added: Some("2024-01-02".into()),
                        source_format: Some("FLAC".into()),
                        comment: Some("Note".into()),
                        bpm: Some(100 + index),
                        source_loudness: match index {
                            1 => Some(library::NativeGain {
                                integrated_lufs: Some(-15.0),
                                ..Default::default()
                            }),
                            2 => Some(library::NativeGain {
                                integrated_lufs: Some(-16.0),
                                true_peak: Some(0.75),
                                replay_gain_db: Some(-4.5),
                                replay_gain_peak: Some(0.9),
                            }),
                            _ => None,
                        },
                        album_loudness: (index == 3).then_some(library::NativeGain {
                            integrated_lufs: Some(-13.0),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    object_id: format!("track-{index}"),
                    album_object_id: Some(album_id.to_owned()),
                    loudness_analysis_key: [index as u8 + 10; 32],
                    ..Default::default()
                }],
                ..Default::default()
            };

            scan_catalog.apply_catalog(&write, true).await.unwrap();
        };
    }
    let (updates_tx, mut updates) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |_, update| {
        let _ = updates_tx.send(update);
        std::future::ready(())
    });
    {
        let update = {
            scan_catalog
                .complete_source_observation(
                    scan,
                    &[
                        "track", "album", "artist", "genre", "mood", "folder", "playlist",
                    ],
                    None,
                )
                .await
                .unwrap();

            scan_catalog
                .complete_source_observation(scan, &[], None)
                .await
                .unwrap();

            scan_catalog.into_changes()
        };
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    let update = updates.try_recv().unwrap();
    assert!(update.metadata);
    assert!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[0], &cancel)
            .await
            .unwrap()
            .0
            .is_none(),
        "unstaged old source fact is removed"
    );
    assert_eq!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[1], &cancel)
            .await
            .unwrap()
            .0,
        Some(local_analysis),
        "a source fact cannot replace analysis of current local audio"
    );
    let next = fixture
        .database
        .next_missing_track_loudness(fixture.source, Some(fixture.tracks[0]), &cancel)
        .await
        .unwrap();
    assert!(next.is_none_or(|work| work.track_key != fixture.tracks[1]));
    let staged_track = fixture
        .database
        .playback_loudness(&fixture.track_uris[2], &cancel)
        .await
        .unwrap()
        .0
        .unwrap();
    assert_eq!(staged_track.integrated_lufs, Some(-16.0));
    assert_eq!(staged_track.true_peak, Some(0.75));
    assert_eq!(staged_track.replay_gain_db, Some(-4.5));
    assert_eq!(staged_track.replay_gain_peak, Some(0.9));
    assert_eq!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[3], &cancel)
            .await
            .unwrap()
            .1
            .unwrap()
            .integrated_lufs,
        Some(-13.0)
    );
    let changed_album = fixture
        .database
        .next_missing_album_loudness(fixture.source, None, &cancel)
        .await
        .expect("changed Album membership work")
        .unwrap();
    assert_eq!(changed_album.album_key, album.album_key);
    assert_ne!(changed_album.expected_analysis_key, old_album_key);
    assert!(
        fixture
            .database
            .write_album_analyzed_loudness(
                fixture.source,
                changed_album.album_key,
                &LoudnessMeasurement {
                    analysis_key: changed_album.expected_analysis_key,
                    integrated_lufs: Some(-12.5),
                    true_peak: Some(0.82),
                    replay_gain_db: None,
                    replay_gain_peak: None
                }
            )
            .await
            .expect("replace stale analyzed Album value")
    );

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    {
        let write = library::CatalogBatch {
            tracks: vec![library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Delta".into(),
                    sort_text: "delta".into(),
                    ..Default::default()
                },
                object_id: "track-3".into(),
                album_object_id: Some("album-b".into()),
                loudness_analysis_key: [43; 32],
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let update = scan_catalog.into_changes();
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    assert!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[3], &cancel)
            .await
            .unwrap()
            .1
            .is_none(),
        "unsupplied album gain must not acquire a new audio identity"
    );
}

#[tokio::test]
async fn loudness_keeps_canonical_provider_uri_without_direct_access() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    let track = fixture
        .database
        .next_missing_track_loudness(fixture.source, None, &cancel)
        .await
        .expect("select provider-resolved Track loudness")
        .expect("provider Track loudness work");
    assert_eq!(track.track_key, fixture.tracks[0]);
    assert_eq!(track.media_uri, fixture.track_uris[0]);

    let album = fixture
        .database
        .next_missing_album_loudness(fixture.source, None, &cancel)
        .await
        .expect("select provider-resolved Album loudness")
        .expect("provider Album loudness work");
    assert_eq!(album.tracks.len(), 2);
    assert_eq!(album.tracks[0].media_uri, fixture.track_uris[0]);
}

#[tokio::test]
async fn source_artwork_pages_distinct_bindings() {
    let fixture = fixture().await;
    let binding = |id: &str| {
        serde_json::to_vec(&[library::ArtworkCandidate::new(
            library::ArtworkImage::Native {
                source_id: library::SourceId::new("source"),
                item_id: id.into(),
                tag: None,
            },
            None,
        )])
        .unwrap()
    };
    let mut raw = connection(&fixture.path).await;
    sqlx::query("UPDATE artists SET native_artwork=NULL ")
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET native_artwork=?2 WHERE track_key=?1")
        .bind(fixture.tracks[0])
        .bind(binding("binding-a").as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate Track artwork");
    sqlx::query("UPDATE albums SET native_artwork=?2 WHERE album_key=?1")
        .bind(fixture.albums[0])
        .bind(binding("binding-a").as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate duplicate Album artwork");
    sqlx::query("UPDATE artists SET native_artwork=?2 WHERE artist_key=?1")
        .bind(fixture.artists[0])
        .bind(binding("binding-b").as_slice())
        .execute(&mut raw)
        .await
        .expect("hydrate Artist artwork");
    let cancel = ReadCancellation::new();
    let first = fixture
        .database
        .source_artwork_page(fixture.source, None, 1, &cancel)
        .await
        .expect("first artwork page");
    assert_eq!(first.len(), 1);
    let second = fixture
        .database
        .source_artwork_page(fixture.source, Some(&first[0]), 128, &cancel)
        .await
        .expect("resume artwork page");
    let mut expected_first = [binding("binding-a"), binding("binding-b")];
    expected_first.sort();
    assert_eq!(first, expected_first[..1]);
    assert_eq!(second, expected_first[1..]);
    sqlx::query("WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<259)\n        INSERT INTO tracks(source_key,object_id,media_uri,native_artwork,metadata,normalized_search,indexed) SELECT ?1,'artwork-'||n,'test:artwork-'||n,NULL,jsonb(json_object('title',COALESCE('',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('',''),'duration_millis',COALESCE(0,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('',''),'duration_millis',COALESCE(0,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('',''),'duration_millis',COALESCE(0,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('',''),'duration_millis',COALESCE(0,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('',''),'duration_millis',COALESCE(0,0))),'$.comment'),'')),1 FROM numbers")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    fixture
        .database
        .catalog("source", None)
        .await
        .unwrap()
        .set_source_artwork(
            "track",
            &(0..260)
                .map(|n| {
                    (
                        format!("artwork-{n}"),
                        Some(binding(&format!("binding-{n:04}"))),
                    )
                })
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE genres SET native_artwork=NULL WHERE genre_key=?1")
        .bind(fixture.genre)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("UPDATE folders SET native_artwork=?2 WHERE folder_key=?1")
        .bind(fixture.folder)
        .bind(binding("binding-folder").as_slice())
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO playlists(source_key,owner,object_id,name,normalized_name,sort_text,position,artwork_binding) VALUES(?1,'source','artwork','Artwork','artwork','artwork',(SELECT COALESCE(max(position),-1)+1 FROM playlists),?2)")
        .bind(fixture.source).bind(binding("binding-playlist").as_slice())
        .execute(&mut raw).await.unwrap();
    let mut expected = Vec::new();
    expected.extend((0..260).map(|n| binding(&format!("binding-{n:04}"))));
    expected.extend([
        binding("binding-a"),
        binding("binding-b"),
        binding("binding-folder"),
        binding("binding-playlist"),
    ]);
    expected.sort();
    let mut actual = Vec::<Vec<u8>>::new();
    loop {
        let page = fixture
            .database
            .source_artwork_page(
                fixture.source,
                actual.last().map(Vec::as_slice),
                128,
                &cancel,
            )
            .await
            .unwrap();
        assert!(page.len() <= 128);
        if page.is_empty() {
            break;
        }
        actual.extend(page);
        assert!(actual.len() <= expected.len());
    }
    assert_eq!(actual, expected);
    sqlx::query("INSERT INTO download_locators(media_uri,origin,path,root,relative_path,access_uri) SELECT media_uri,'download',media_uri,'','',media_uri FROM tracks WHERE object_id LIKE 'artwork-%' AND CAST(substr(object_id,9) AS INTEGER)%2=0")
        .execute(&mut raw).await.unwrap();
    let mut retention = fixture
        .database
        .artwork_cache_retention(&["test:artwork-1".into()])
        .await
        .unwrap();
    let mut retained = Vec::new();
    loop {
        let page = retention.next_bindings().await.unwrap();
        assert!(page.len() <= 128);
        if page.is_empty() {
            break;
        }
        retained.extend(page);
    }
    assert_eq!(retained.len(), 131);
    assert!(retained.contains(&binding("binding-0001")));
    assert!(!retained.contains(&binding("binding-a")));
    retention
        .retain(&["native/source/cover".into(), "external/cover".into()])
        .await
        .unwrap();
    assert_eq!(
        retention
            .unretained(&[
                "native/source/cover".into(),
                "external/other".into(),
                "external/cover".into()
            ])
            .await
            .unwrap(),
        [false, true, false]
    );
    retention.finish().await.unwrap();
}

#[tokio::test]
async fn source_removal_preserves_listens_and_pending_delivery_only() {
    let fixture = fixture().await;
    fixture
        .database
        .record_listen(
            &ListenWrite {
                external_id: Some("remove-play".to_string()),
                media_uri: fixture.track_uris[0].clone(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                duration_millis: 180_000,
                disc_number: None,
                track_number: None,
                year: None,
                release_date: None,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                started_at: 100,
                local_period: "1970-01".to_string(),
                listened_millis: 1000,
                skipped: false,
            },
            None,
            &[ListenDeliveryTarget {
                service: "lastfm".to_string(),
                account_id: "account".to_string(),
                next_attempt_at: Some(200),
            }],
        )
        .await
        .expect("record retained listen");

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    {
        let write = library::CatalogBatch {
            tracks: vec![library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Alpha".into(),
                    sort_text: "alpha".into(),
                    source_loudness: Some(library::NativeGain {
                        integrated_lufs: Some(-12.0),
                        true_peak: Some(0.8),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                object_id: "track-0".into(),
                loudness_analysis_key: [1; 32],
                ..Default::default()
            }],
            ..Default::default()
        };

        scan_catalog.apply_catalog(&write, true).await.unwrap();
    };
    {
        let update = scan_catalog.into_changes();
        (&fixture.database)
            .publish_catalog_update("source", update)
            .await;
    };
    let queue_media = fixture
        .database
        .track_references_for_uris(&fixture.track_uris[..1], &ReadCancellation::new())
        .await
        .expect("materialize Queue media")
        .remove(0);
    persist_queue(
        &fixture.database,
        &[QueueItem {
            item: queue_media,
            position: 0,
            provenance: QueueProvenance::Manual,
        }],
        Some(0),
        0,
        QueueRepeatMode::Off,
        false,
    )
    .await;
    assert!(
        fixture
            .database
            .remove_source(&library::SourceId::new("source"))
            .await
            .expect("remove source")
    );
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_as::<_, (Option<String>, String)>(
            "SELECT source_id,media_uri FROM listens WHERE external_id='remove-play'"
        )
        .fetch_one(&mut raw)
        .await
        .expect("retained listen identity"),
        (Some("source".to_string()), fixture.track_uris[0].clone())
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_as::<_,(i64,i64,i64,i64)>("SELECT (SELECT count(*) FROM sources),(SELECT count(*) FROM tracks WHERE indexed),(SELECT count(*) FROM queue_entries),(SELECT count(*) FROM loudness_measurements)").fetch_one(&mut raw).await.unwrap(),(0,0,1,0));
}
