use library::{ListenDeliveryTarget, ListenWrite, ReadCancellation, SourceId};

use super::support::{connection, fixture};

#[tokio::test]
async fn listens_accept_empty_optional_recording_ids() {
    let fixture = fixture().await;
    let (published, mut observed) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |source, update| {
        let _ = published.send((source, update));
        std::future::ready(())
    });
    let listen = ListenWrite {
        external_id: Some("optional-recording-ids".into()),
        media_uri: "https://audio.invalid/direct.flac".into(),
        title: "Playable song".into(),
        artist: "Artist".into(),
        album: "Album".into(),
        duration_millis: 1000,
        disc_number: None,
        track_number: None,
        year: None,
        release_date: None,
        source_format: None,
        musicbrainz_recording_id: Some(String::new()),
        musicbrainz_release_track_id: Some(String::new()),
        started_at: 1_700_000_000,
        local_period: "2023-11".into(),
        listened_millis: 1000,
        skipped: false,
    };
    let key = fixture
        .database
        .record_listen(&listen, None, &[])
        .await
        .unwrap();
    let (source, update) = observed.try_recv().unwrap();
    assert!(source.is_none());
    assert!(update.activity && !update.metadata);
    assert_eq!(
        fixture
            .database
            .record_listen(&listen, None, &[])
            .await
            .unwrap(),
        key
    );
    assert!(observed.try_recv().is_err());
}

#[tokio::test]
async fn activity_keeps_one_listen_and_independent_delivery_targets() {
    let fixture = fixture().await;
    let (updates, mut observed) = tokio::sync::mpsc::unbounded_channel();
    fixture.database.observe_catalog(move |_, update| {
        updates.send(update).unwrap();
        std::future::ready(())
    });
    let listen = ListenWrite {
        external_id: Some("play-1".to_string()),
        media_uri: fixture.track_uris[0].clone(),
        title: "Stored Title".to_string(),
        artist: "Stored Artist".to_string(),
        album: "Stored Album".to_string(),
        duration_millis: 180_000,
        disc_number: None,
        track_number: None,
        year: None,
        release_date: None,
        source_format: None,
        musicbrainz_recording_id: None,
        musicbrainz_release_track_id: None,
        started_at: 1_700_000_000,
        local_period: "2023-11".to_string(),
        listened_millis: 90_000,
        skipped: true,
    };
    let targets = (0..12)
        .map(|index| ListenDeliveryTarget {
            service: match index % 3 {
                0 => "lastfm",
                1 => "listenbrainz",
                _ => "librefm",
            }
            .to_string(),
            account_id: format!("account-{index}"),
            next_attempt_at: Some(10),
        })
        .collect::<Vec<_>>();
    let key = fixture
        .database
        .record_listen(&listen, None, &targets)
        .await
        .expect("record listen");
    let update = observed.try_recv().expect("new listen changes activity");
    assert!(update.activity);
    assert!(!update.metadata);
    assert_eq!(
        fixture
            .database
            .record_listen(&listen, None, &targets)
            .await
            .expect("record listen idempotently"),
        key
    );
    assert!(observed.try_recv().is_err());
    let cancel = ReadCancellation::new();
    let history = (&library::MemberRead {
        descending: true,
        ..library::MemberRead::new(library::Collection::History {
            source: (Some(&SourceId::new("source"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .expect("Activity History");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].metadata.title, "Stored Title");
    assert_eq!(history[0].metadata.duration_millis, 180_000);
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT local_play_count FROM tracks WHERE media_uri=?1")
            .bind(&listen.media_uri)
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1,
        "duplicate accepted listens must not increment the indexed local count"
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT skipped FROM listens WHERE listen_key=?1")
            .bind(key)
            .fetch_one(&mut raw)
            .await
            .expect("read accepted skip")
    );
    sqlx::query("INSERT INTO native_media_state(media_uri,play_count,last_played) SELECT media_uri,2,1600000000 FROM tracks WHERE source_key=?1 AND object_id='track-1' ON CONFLICT(media_uri) DO UPDATE SET play_count=excluded.play_count,last_played=excluded.last_played")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    drop(raw);
    let catalog = fixture
        .database
        .tracks_by_uris(&([(&listen.media_uri).to_string()]), &cancel)
        .await
        .map(|rows| {
            let mut rows = rows;
            rows.pop()
        })
        .unwrap()
        .unwrap();
    let history = (&library::MemberRead {
        descending: true,
        ..library::MemberRead::new(library::Collection::History { source: None })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    let row = &history[0];
    assert_eq!(row.links.album_media_uri, catalog.links.album_media_uri);
    assert_eq!(row.links.artists, catalog.links.artists);
    assert_eq!(row.links.album_artists, catalog.links.album_artists);
    assert_eq!(row.metadata.date_added, catalog.metadata.date_added);
    assert_eq!(row.metadata.bpm, catalog.metadata.bpm);
    assert_eq!(row.metadata.genres[0].name, "Rock");
    assert_eq!(row.state.play_count, 1);
    let (playlist, _) = super::support::server_playlist(
        &fixture.database,
        "History track",
        std::slice::from_ref(&listen.media_uri),
    )
    .await;
    let entries = fixture
        .database
        .playlist_edit_entries(
            playlist,
            &library::MemberRead {
                collection: library::Collection::Playlist(playlist),
                sort: Some(library::CatalogSort::Position),
                ..library::MemberRead::new(library::Collection::Playlist(playlist))
            }
            .selection(),
            &cancel,
        )
        .await
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| entry.playlist_entry_key)
                .collect::<Vec<_>>()
        })
        .unwrap();
    let entries = fixture
        .database
        .playlist_entries(&((&entries).to_vec()), &cancel)
        .await
        .unwrap();
    let entry = &entries[0];
    assert_eq!(
        entry.track.links.album_media_uri,
        catalog.links.album_media_uri
    );
    assert_eq!(entry.track.links.artists, catalog.links.artists);
    assert_eq!(entry.track.links.album_artists, catalog.links.album_artists);
    assert_eq!(entry.track.metadata.date_added, catalog.metadata.date_added);
    assert_eq!(entry.track.metadata.bpm, catalog.metadata.bpm);
    assert_eq!(entry.track.metadata.genres[0].name, "Rock");
    assert_eq!(entry.track.state.play_count, catalog.state.play_count);
    assert_eq!(entry.track.state.last_played, catalog.state.last_played);
    let due = fixture
        .database
        .due_listen_deliveries(10, 100, &cancel)
        .await
        .expect("due listen deliveries");
    assert_eq!(due.len(), 12);
    assert_eq!(due[0].duration_millis, 180_000);
    assert_eq!(due[0].listened_millis, 90_000);
    assert!(
        fixture
            .database
            .complete_listen_delivery(due[0].outbox_key)
            .await
            .expect("complete one delivery")
    );
    assert!(
        fixture
            .database
            .defer_listen_delivery(due[1].outbox_key, 20, Some("offline"))
            .await
            .expect("defer one delivery")
    );
    assert_eq!(
        (&library::MemberRead {
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("source"))).cloned()
            })
        })
            .rows(&fixture.database, 0, 100, &cancel)
            .await
            .map(|rows| rows
                .into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>())
            .expect("History survives delivery")
            .len(),
        1
    );
    assert_eq!(
        fixture
            .database
            .due_listen_deliveries(10, 100, &cancel)
            .await
            .expect("completed target removed")
            .len(),
        10
    );
    while observed.try_recv().is_ok() {}
    let completed = ListenWrite {
        listened_millis: 120_000,
        skipped: false,
        ..listen.clone()
    };
    assert_eq!(
        fixture
            .database
            .record_listen(&completed, None, &[])
            .await
            .unwrap(),
        key
    );
    assert!(observed.try_recv().unwrap().activity);
    for repeated in [&listen, &completed] {
        assert_eq!(
            fixture
                .database
                .record_listen(repeated, None, &[])
                .await
                .unwrap(),
            key
        );
        assert!(observed.try_recv().is_err());
    }
    let mut raw = connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_as::<_, (i64, bool)>(
            "SELECT listened_millis,skipped FROM listens WHERE listen_key=?1"
        )
        .bind(key)
        .fetch_one(&mut raw)
        .await
        .unwrap(),
        (120_000, false)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listens")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM listen_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        11
    );
}

#[tokio::test]
async fn history_preserves_latest_matching_facts_and_source_scope() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    for (index, (source, uri, title, started_at)) in [
        (Some("source"), "https://example.org/a", "Old title", 1),
        (Some("source"), "https://example.org/a", "New title", 2),
        (Some("other"), "https://example.org/b", "Other account", 2),
        (
            Some("forgotten"),
            "https://example.org/c",
            "Retained history",
            3,
        ),
        (None, "https://example.org/d", "Direct media", 4),
    ]
    .into_iter()
    .enumerate()
    {
        let key = fixture
            .database
            .record_listen(
                &ListenWrite {
                    external_id: Some(format!("history-{index}")),
                    media_uri: uri.into(),
                    title: title.into(),
                    artist: "Artist".into(),
                    album: "Album".into(),
                    duration_millis: 1000,
                    disc_number: None,
                    track_number: None,
                    year: None,
                    release_date: None,
                    source_format: None,
                    musicbrainz_recording_id: None,
                    musicbrainz_release_track_id: None,
                    started_at,
                    local_period: "2023-11".into(),
                    listened_millis: 900,
                    skipped: false,
                },
                None,
                &[],
            )
            .await
            .unwrap();
        sqlx::query("UPDATE listens SET source_id=?2 WHERE listen_key=?1")
            .bind(key)
            .bind(source)
            .execute(&mut raw)
            .await
            .unwrap();
    }
    let cancel = ReadCancellation::new();
    let all = (&library::MemberRead {
        descending: true,
        ..library::MemberRead::new(library::Collection::History { source: None })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|row| row.metadata.title.as_str())
            .collect::<Vec<_>>(),
        [
            "Direct media",
            "Retained history",
            "Other account",
            "New title"
        ]
    );
    assert_eq!(all[3].state.play_count, 2);
    let current = (&library::MemberRead {
        descending: true,
        ..library::MemberRead::new(library::Collection::History {
            source: (Some(&SourceId::new("source"))).cloned(),
        })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(current, vec![all[3].clone()]);
    let matching = (&library::MemberRead {
        filter: (" old TITLE ").into(),
        descending: true,
        ..library::MemberRead::new(library::Collection::History { source: None })
    })
        .rows(&fixture.database, 0, 100, &cancel)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0].metadata.title, "Old title");
    assert_eq!(matching[0].state.last_played, Some(1));
    assert_eq!(matching[0].state.play_count, 2);
    assert!(
        (&library::MemberRead {
            filter: ("old").into(),
            descending: true,
            ..library::MemberRead::new(library::Collection::History {
                source: (Some(&SourceId::new("other"))).cloned()
            })
        })
            .rows(&fixture.database, 0, 100, &cancel)
            .await
            .map(|rows| rows
                .into_iter()
                .map(library::MemberRow::into_track)
                .collect::<Vec<_>>())
            .unwrap()
            .is_empty()
    );
}
