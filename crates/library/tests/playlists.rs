use library::{CatalogSort, FavoriteTarget, ReadCancellation};

use super::support::{connection, fixture};

#[tokio::test]
async fn playlist_pin_import_reads_only_the_owning_sources_identities() {
    let fixture = fixture().await;
    let database = &fixture.database;
    let (_, authored) = super::support::server_playlist(database, "Z saved", &[]).await;
    let mut raw = connection(&fixture.path).await;
    sqlx::query("INSERT INTO playlists(source_key,position,object_id,name,normalized_name,sort_text) VALUES(?1,(SELECT COALESCE(max(position)+1,0) FROM playlists),'native','A native','a native','a native')")
        .bind(fixture.source).execute(&mut raw).await.unwrap();
    let other: library::SourceKey = sqlx::query_scalar("INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('emby','Emby','emby',zeroblob(32)) RETURNING source_key")
        .fetch_one(&mut raw).await.unwrap();
    sqlx::query("INSERT INTO playlists(source_key,position,object_id,name,normalized_name,sort_text) VALUES(?1,(SELECT COALESCE(max(position)+1,0) FROM playlists),'other','Other','other','other')")
        .bind(other).execute(&mut raw).await.unwrap();
    drop(raw);
    let cancel = ReadCancellation::new();
    assert_eq!(
        database
            .source_playlist_object_ids(fixture.source, &cancel)
            .await
            .unwrap(),
        ["native".to_string(), authored]
    );
    assert_eq!(
        database
            .source_playlist_object_ids(other, &cancel)
            .await
            .unwrap(),
        ["other"]
    );
}

#[tokio::test]
async fn playlist_order_is_title_ordered_and_folder_scoped() {
    let fixture = fixture().await;
    let zulu = super::support::server_playlist(
        &fixture.database,
        "Zulu",
        &[fixture.track_uris[0].clone()],
    )
    .await
    .0;
    let alpha = super::support::server_playlist(
        &fixture.database,
        "Alpha",
        &[fixture.track_uris[1].clone()],
    )
    .await
    .0;
    let mut raw = connection(&fixture.path).await;
    let folder = sqlx::query_scalar("INSERT INTO folders(source_key,object_id,metadata,media_uri,normalized_name) VALUES (?1,'destination-folder',jsonb(json_object('name','Destination Folder','sort_text','destination folder')),'folder:'||COALESCE(?1,0)||':'||'destination-folder',lower(COALESCE(json_extract(jsonb(json_object('name','Destination Folder','sort_text','destination folder')),'$.name'),''))) RETURNING folder_key")
        .bind(fixture.source)
        .fetch_one(&mut raw)
        .await
        .expect("insert destination folder");
    sqlx::query("INSERT INTO track_folders(track_key,folder_key,position) VALUES (?1,?2,1)")
        .bind(fixture.tracks[0])
        .bind(folder)
        .execute(&mut raw)
        .await
        .expect("scope Zulu Playlist Track");
    drop(raw);

    let cancellation = ReadCancellation::new();
    let all = library::ListRead {
        kind: library::ListKind::Playlists,
        source: Some(fixture.source),
        sort: CatalogSort::Title,
        folder: None,
        filter: ("").into(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .playlists(&fixture.database, 0, 256, &cancellation)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>()
    })
    .expect("all Playlists");
    assert_eq!(all, [alpha, zulu]);
    let scoped = library::ListRead {
        kind: library::ListKind::Playlists,
        source: Some(fixture.source),
        sort: CatalogSort::Title,
        folder: Some(folder),
        filter: ("").into(),
        descending: false,
        favorites_only: false,
        downloaded_only: false,
        activity: true,
    }
    .playlists(&fixture.database, 0, 256, &cancellation)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| row.playlist_key)
            .collect::<Vec<_>>()
    })
    .expect("folder-scoped Playlists");
    assert_eq!(scoped, [zulu]);
    let destinations = fixture
        .database
        .playlist_destinations(Some(fixture.source), &cancellation)
        .await
        .unwrap();
    assert_eq!(
        destinations
            .iter()
            .find(|row| row.playlist_key == zulu)
            .unwrap()
            .genres[0]
            .name,
        "Rock"
    );
    assert_eq!(
        fixture
            .database
            .playlists(&[zulu], &cancellation)
            .await
            .unwrap()[0]
            .genres[0]
            .name,
        "Rock"
    );
}

#[tokio::test]
async fn playlist_automatic_covers_keep_distinct_saved_choices() {
    let fixture = fixture().await;
    let [first, second] = ["first-album", "second-album"].map(|item_id| {
        serde_json::to_vec(&[library::ArtworkCandidate::new(
            library::ArtworkImage::Native {
                source_id: library::SourceId::new("source"),
                item_id: item_id.into(),
                tag: None,
            },
            None,
        )])
        .unwrap()
    });
    fixture
        .database
        .catalog("source", None)
        .await
        .unwrap()
        .set_source_artwork(
            "album",
            &[
                ("album-a".into(), Some(first.clone())),
                ("album-b".into(), Some(second.clone())),
            ],
        )
        .await
        .unwrap();
    let playlist = super::support::server_playlist(
        &fixture.database,
        "Positional covers",
        &[
            fixture.track_uris[0].clone(),
            fixture.track_uris[0].clone(),
            fixture.track_uris[2].clone(),
            fixture.track_uris[1].clone(),
            fixture.track_uris[3].clone(),
        ],
    )
    .await
    .0;
    let row = fixture
        .database
        .playlists(&[playlist], &ReadCancellation::new())
        .await
        .expect("Playlist cover samples")
        .pop()
        .expect("Playlist row");
    let candidate =
        serde_json::from_slice::<Vec<library::ArtworkCandidate>>(&row.artwork_binding.unwrap())
            .unwrap()
            .remove(0);
    let candidate = fixture
        .database
        .resolve_playlist_artwork(candidate)
        .await
        .unwrap()
        .unwrap();
    let library::ArtworkImage::Playlist {
        automatic: Some(manifest),
        resolved: Some(recipes),
        ..
    } = candidate.image
    else {
        panic!("automatic playlist image")
    };
    assert_eq!(manifest.chosen.len(), 2);
    assert_eq!(recipes.len(), 2);
    let actual = recipes
        .iter()
        .map(|recipe| serde_json::to_vec(recipe).unwrap())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(actual, [first, second].into_iter().collect());
}

#[tokio::test]
async fn provider_playlist_payload_resolves_uri_only_at_the_protocol_boundary() {
    let fixture = fixture().await;
    let playlist = super::support::server_playlist(
        &fixture.database,
        "Provider list",
        &[fixture.track_uris[0].clone()],
    )
    .await
    .0;
    assert_eq!(
        fixture
            .database
            .source_playlist_track_ids(
                fixture.source,
                Some(playlist),
                &library::MemberSelection::Uris(
                    ((&[
                        fixture.track_uris[0].clone(),
                        fixture.track_uris[1].clone(),
                        fixture.track_uris[1].clone(),
                    ])
                        .to_vec())
                    .into()
                ),
                true
            )
            .await
            .expect("provider payload"),
        ["track-1", "track-1"]
    );
}

#[tokio::test]
async fn source_playlist_additions_reject_other_sources_and_missing_tracks() {
    let fixture = fixture().await;
    let mut raw = connection(&fixture.path).await;
    let destination: library::SourceKey = sqlx::query_scalar(
        "INSERT INTO sources(object_id,display_name,normalized_name,artwork_digest) VALUES ('destination','Destination','destination',zeroblob(32)) RETURNING source_key",
    ).fetch_one(&mut raw).await.unwrap();
    let own_uri = "test:destination-track".to_string();
    sqlx::query("INSERT INTO tracks(source_key,object_id,media_uri,metadata,normalized_search,indexed) VALUES (?1,'destination-track',?2,jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),lower(COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.title'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.artist'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.album'),'')||' '||COALESCE(json_extract(jsonb(json_object('title',COALESCE('Destination Track',''),'artist',COALESCE('',''),'album',COALESCE('',''),'sort_text',COALESCE('destination track',''),'duration_millis',COALESCE(1000,0))),'$.comment'),'')),1)")
        .bind(destination).bind(&own_uri).execute(&mut raw).await.unwrap();
    drop(raw);
    let database = &fixture.database;
    assert_eq!(
        database
            .source_playlist_track_ids(
                destination,
                None,
                &library::MemberSelection::Uris(std::slice::from_ref(&own_uri).to_vec().into()),
                false
            )
            .await
            .unwrap(),
        ["destination-track"]
    );
    for selection in [
        vec![fixture.track_uris[0].clone()],
        vec![own_uri.clone(), fixture.track_uris[0].clone()],
    ] {
        let error = database
            .source_playlist_track_ids(
                destination,
                None,
                &library::MemberSelection::Uris(selection.into()),
                true,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, library::LibraryError::PlaylistSourceMismatch(source)
            if source.as_str() == "destination")
        );
    }
    let missing = vec![own_uri, "test:missing-track".to_string()];
    assert!(matches!(
        database
            .source_playlist_track_ids(destination, None, &library::MemberSelection::Uris(missing.into()), false)
            .await
            .unwrap_err(),
        library::LibraryError::InvalidRequest(message) if message == "Playlist Track is no longer current"
    ));
}

#[tokio::test]
async fn cleared_rating_hides_the_server_rating() {
    let fixture = fixture().await;
    let target = FavoriteTarget::Track(fixture.track_uris[0].clone());
    let mut writer = fixture.database.writer().await.unwrap();
    sqlx::query("INSERT INTO native_media_state(media_uri,rating) VALUES(?1,60) ON CONFLICT(media_uri) DO UPDATE SET rating=60")
        .bind(target.media_uri())
        .execute(writer.as_mut().unwrap())
        .await
        .unwrap();
    drop(writer);
    let rating = || async {
        fixture
            .database
            .tracks(&[fixture.tracks[0]], &ReadCancellation::new())
            .await
            .unwrap()
            .remove(0)
            .state
            .rating
    };
    assert_eq!(rating().await, Some(6));
    fixture.database.set_rating(&target, None).await.unwrap();
    assert_eq!(rating().await, None);
}

#[tokio::test]
async fn rating_favorite_and_delivery_are_uri_owned() {
    let mut fixture = fixture().await;
    let cancellation = ReadCancellation::new();
    let target = FavoriteTarget::Track(fixture.track_uris[0].clone());
    assert_eq!(
        fixture
            .database
            .user_media_state(target.media_uri(), &ReadCancellation::new())
            .await
            .expect("read absent user state"),
        None
    );
    assert!(
        fixture
            .database
            .set_rating(&target, Some(8))
            .await
            .expect("set Rating")
    );
    assert!(
        fixture
            .database
            .set_favorite(&target, true)
            .await
            .expect("set Favorite")
    );
    let row = fixture
        .database
        .tracks(&[fixture.tracks[0]], &ReadCancellation::new())
        .await
        .expect("read Track")
        .remove(0);
    assert_eq!(row.state.rating, Some(8));
    assert!(row.state.favorite);
    assert_eq!(
        fixture
            .database
            .user_media_state(target.media_uri(), &ReadCancellation::new())
            .await
            .expect("read URI-owned user state"),
        Some((Some(true), Some(8)))
    );

    assert!(
        fixture
            .database
            .queue_remote_favorite(&target, false, 100)
            .await
            .expect("queue delivery")
    );
    assert!(
        fixture
            .database
            .due_remote_favorites(99, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    let album = FavoriteTarget::Album(fixture.album_uris[0].clone());
    fixture
        .database
        .queue_remote_favorite(&album, true, 100)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(100, 1, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), false)]
    );
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(100, 64, &cancellation)
            .await
            .unwrap(),
        [
            (target.media_uri().to_string(), false),
            (album.media_uri().to_string(), true)
        ]
    );
    assert!(
        fixture
            .database
            .acknowledge_remote_favorite(&album, true)
            .await
            .unwrap()
    );
    assert!(fixture.database.favorite(&album).await.unwrap());
    assert!(
        fixture
            .database
            .defer_remote_favorite(&target, false, 200)
            .await
            .expect("defer delivery")
    );
    fixture.database.clone().close().await.unwrap();
    fixture.database = library::Database::open(&fixture.path).await.unwrap();
    assert!(!fixture.database.favorite(&target).await.unwrap());
    assert!(
        fixture
            .database
            .due_remote_favorites(199, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(200, 64, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), false)]
    );
    fixture
        .database
        .queue_remote_favorite(&target, true, 201)
        .await
        .unwrap();
    assert!(
        !fixture
            .database
            .acknowledge_remote_favorite(&target, false)
            .await
            .unwrap()
    );
    assert!(
        !fixture
            .database
            .defer_remote_favorite(&target, false, 300)
            .await
            .unwrap()
    );
    assert_eq!(
        fixture
            .database
            .reject_remote_favorite(&target, false)
            .await
            .unwrap(),
        None
    );
    assert!(fixture.database.favorite(&target).await.unwrap());
    assert_eq!(
        fixture
            .database
            .due_remote_favorites(201, 64, &cancellation)
            .await
            .unwrap(),
        [(target.media_uri().to_string(), true)]
    );
    fixture
        .database
        .queue_remote_favorite(&target, false, 202)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .database
            .reject_remote_favorite(&target, false)
            .await
            .expect("reject delivery"),
        Some(true)
    );
    assert!(
        fixture
            .database
            .due_remote_favorites(300, 64, &cancellation)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .database
            .tracks(&[fixture.tracks[0]], &ReadCancellation::new())
            .await
            .expect("restored Track")[0]
            .state
            .favorite
    );
}
