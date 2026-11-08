use library::{
    DownloadAccessWrite, ListenDeliveryTarget, ListenWrite, QueueProvenance, QueueRepeatMode,
    ReadCancellation,
};

use super::support::{QueueItem, connection, fixture, persist_queue};

#[tokio::test]
async fn source_replaygain_applies_only_to_the_source_audio() {
    let fixture = fixture().await;
    let cancel = ReadCancellation::new();
    assert_eq!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[0], &cancel)
            .await
            .unwrap(),
        (None, None)
    );
    let source_key = fixture
        .database
        .audio_analysis_key(&fixture.track_uris[0])
        .await
        .unwrap()
        .unwrap();
    let input = library::TrackObservation {
        metadata: library::TrackMetadata {
            title: "Alpha".into(),
            album: "Album A".into(),
            artist: "Artist A".into(),
            sort_text: "alpha".into(),
            duration_millis: 180_000,
            disc_number: 1,
            track_number: 1,
            source_loudness: Some(library::NativeGain {
                replay_gain_db: Some(-4.5),
                replay_gain_peak: Some(0.9),
            }),
            album_loudness: Some(library::NativeGain {
                replay_gain_db: Some(-6.0),
                replay_gain_peak: Some(0.95),
            }),
            ..Default::default()
        },
        object_id: "track-0".into(),
        album_object_id: Some("album-a".into()),
        loudness_analysis_key: source_key,
        ..Default::default()
    };
    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    scan_catalog
        .apply_catalog(
            &library::CatalogBatch {
                tracks: vec![input],
                ..Default::default()
            },
            true,
        )
        .await
        .unwrap();
    let update = scan_catalog.into_changes();
    (&fixture.database)
        .publish_catalog_update("source", update)
        .await;
    let (track, album) = fixture
        .database
        .playback_loudness(&fixture.track_uris[0], &cancel)
        .await
        .unwrap();
    assert_eq!(
        track,
        Some(library::NativeGain {
            replay_gain_db: Some(-4.5),
            replay_gain_peak: Some(0.9),
        })
    );
    assert_eq!(album.unwrap().replay_gain_db, Some(-6.0));

    fixture
        .database
        .write_download_access(
            Some(fixture.source),
            &[&DownloadAccessWrite {
                media_uri: fixture.track_uris[0].clone(),
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
        .unwrap();
    assert_eq!(
        fixture
            .database
            .playback_loudness(&fixture.track_uris[0], &cancel)
            .await
            .unwrap()
            .0,
        None,
        "a different local audio identity must not inherit the source gain"
    );
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
    sqlx::query("INSERT INTO playlists(source_key,object_id,name,normalized_name,sort_text,position,artwork_binding) VALUES(?1,'artwork','Artwork','artwork','artwork',(SELECT COALESCE(max(position),-1)+1 FROM playlists),?2)")
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
async fn source_removal_preserves_pending_scrobbles() {
    let fixture = fixture().await;
    fixture
        .database
        .queue_scrobbles(
            &ListenWrite {
                play_id: "remove-play".to_string(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                duration_millis: 180_000,
                started_at: 100,
                listened_millis: 1000,
            },
            &[ListenDeliveryTarget {
                service: "lastfm".to_string(),
                account_id: "account".to_string(),
                next_attempt_at: Some(200),
            }],
        )
        .await
        .expect("queue scrobble");

    let mut scan_catalog = fixture.database.catalog("source", None).await.unwrap();
    {
        let write = library::CatalogBatch {
            tracks: vec![library::TrackObservation {
                metadata: library::TrackMetadata {
                    title: "Alpha".into(),
                    sort_text: "alpha".into(),
                    source_loudness: Some(library::NativeGain {
                        replay_gain_db: Some(-3.0),
                        replay_gain_peak: Some(0.8),
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
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM scrobble_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_as::<_,(i64,i64,i64)>("SELECT (SELECT count(*) FROM sources),(SELECT count(*) FROM tracks WHERE indexed),(SELECT count(*) FROM queue_entries)").fetch_one(&mut raw).await.unwrap(),(0,0,1));
}
