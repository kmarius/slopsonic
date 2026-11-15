use std::path::Path;

use library::{Database, ReadCancellation};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

#[tokio::test]
async fn reopening_without_statistics_preserves_library_order() {
    let fixture = super::support::fixture().await;
    let before = fixture
        .database
        .selected_track_uris(
            &library::MemberRead {
                collection: library::Collection::Library,
                sort: Some(library::CatalogSort::Title),
                ..library::MemberRead::new(library::Collection::Library)
            }
            .selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql("DROP TABLE IF EXISTS sqlite_stat1;")
        .execute(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    fixture.database.close().await.unwrap();
    let durable = durable_rows(&fixture.path).await;
    let database = Database::open(&fixture.path).await.unwrap();
    assert_eq!(durable_rows(&fixture.path).await, durable);
    let after = database
        .selected_track_uris(
            &library::MemberRead {
                collection: library::Collection::Library,
                sort: Some(library::CatalogSort::Title),
                ..library::MemberRead::new(library::Collection::Library)
            }
            .selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(after.len(), fixture.tracks.len());
    database.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    let schema_version: i64 = sqlx::query_scalar("PRAGMA schema_version")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    raw.close().await.unwrap();
    let reopened = Database::open(&fixture.path).await.unwrap();
    assert_eq!(durable_rows(&fixture.path).await, durable);
    let mut raw = super::support::connection(&fixture.path).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("PRAGMA schema_version")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        schema_version
    );
    raw.close().await.unwrap();
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn missing_indexed_column_returns_error_without_replacing_the_library() {
    let fixture = super::support::fixture().await;
    fixture.database.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%native_artwork%'",
    )
    .fetch_all(&mut raw)
    .await
    .unwrap();
    for trigger in triggers {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TRIGGER {trigger}")))
            .execute(&mut raw)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        "DROP INDEX tracks_native_artwork_idx;
        ALTER TABLE tracks DROP COLUMN native_artwork;",
    )
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Database::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}

#[tokio::test]
async fn index_setup_failure_returns_error_without_replacing_the_library() {
    let fixture = super::support::fixture().await;
    super::support::server_playlist(&fixture.database, "Keep me", &[]).await;
    fixture.database.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql(
        "DROP INDEX tracks_global_shuffle_idx; CREATE TABLE tracks_global_shuffle_idx(value TEXT);",
    )
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Database::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}

#[tokio::test]
async fn malformed_library_schema_returns_error_without_replacing_the_file() {
    let fixture = super::support::fixture().await;
    fixture.database.close().await.unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql=replace(sql,'CREATE TABLE','BROKEN TABLE') WHERE name='sources';").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = std::fs::read(&fixture.path).unwrap();
    assert!(Database::open(&fixture.path).await.is_err());
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
}

async fn connection(path: &Path, create: bool) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(create),
    )
    .await
    .expect("open schema test connection")
}

#[tokio::test]
async fn unreadable_library_returns_error_and_preserves_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite3");
    std::fs::write(&path, b"not a sqlite database").unwrap();
    assert!(Database::open(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not a sqlite database");
}
#[tokio::test]
async fn deleting_or_corrupting_catalog_preserves_exact_durable_records() {
    let fixture = super::support::fixture().await;
    let path = fixture.path.clone();
    let catalog = path.with_extension("catalog.sqlite");
    let uri = &fixture.track_uris[0];
    super::support::server_playlist(
        &fixture.database,
        "Durable duplicate list",
        &[uri.clone(), uri.clone()],
    )
    .await;
    let mut raw = connection(&path, false).await;
    sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating) VALUES(?1,1,4)")
        .bind(uri)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO scrobble_outbox(play_id,service,account_id,track_title,artist_name,album_title,started_at,duration_millis,attempts,next_attempt_at,last_error) VALUES('play','lastfm','account','Snapshot','Artist','Album',100,100000,3,200,'retry');").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    fixture.database.close().await.unwrap();
    let mut before = durable_rows(&path).await;
    for damage in 0..3 {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = catalog.as_os_str().to_os_string();
            file.push(suffix);
            match std::fs::remove_file(file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("{error}"),
            }
        }
        match damage {
            1 => std::fs::write(&catalog, b"damaged catalog").unwrap(),
            2 => {
                let mut old = SqliteConnection::connect_with(
                    &SqliteConnectOptions::new()
                        .filename(&catalog)
                        .create_if_missing(true),
                )
                .await
                .unwrap();
                sqlx::raw_sql("PRAGMA user_version=1; CREATE TABLE obsolete(value TEXT)")
                    .execute(&mut old)
                    .await
                    .unwrap();
                old.close().await.unwrap();
            }
            _ => {}
        }
        let database = Database::open(&path).await.unwrap();
        assert_eq!(durable_rows(&path).await, before);
        let mut raw = connection(&path, false).await;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM tracks")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            4
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA integrity_check")
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "ok"
        );
        raw.close().await.unwrap();
        // Ordinary rescan rejoins effective favorites/ratings by the unchanged media URI.
        let (_, object) = library::entity_parts(uri).unwrap();
        database.reconcile_server("Source").await.unwrap();
        let mut catalog = database.catalog(None).await.unwrap();
        let observed = catalog.start_observation().await.unwrap();
        catalog
            .apply_catalog(
                &library::CatalogBatch {
                    tracks: vec![library::TrackObservation {
                        object_id: object.to_owned(),
                        metadata: library::TrackMetadata {
                            title: "Changed tags".into(),
                            artist: "Artist".into(),
                            sort_text: "changed tags".into(),
                            duration_millis: 100000,
                            disc_number: 1,
                            track_number: 1,
                            ..Default::default()
                        },
                        user_data: library::NativeUserState {
                            favorite: Some(false),
                            rating: Some(Some(99)),
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
        catalog
            .complete_source_observation(observed, &["track"], None)
            .await
            .unwrap();
        let mut raw = connection(&path, false).await;
        assert_eq!(sqlx::query_as::<_,(bool,i64)>("SELECT state.favorite,state.rating FROM tracks track JOIN user_media_state state USING(media_uri) WHERE track.media_uri=?1").bind(uri).fetch_one(&mut raw).await.unwrap(),(true,4));
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT title FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_one(&mut raw)
                .await
                .unwrap(),
            "Changed tags"
        );
        raw.close().await.unwrap();
        before = durable_rows(&path).await;
        database.close().await.unwrap();
    }
}

async fn durable_rows(path: &Path) -> Vec<(String, Vec<String>)> {
    let mut raw = connection(path, false).await;
    let tables=sqlx::query_scalar::<_,String>("SELECT name FROM main.sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&mut raw).await.unwrap();
    let mut result = Vec::new();
    for table in tables {
        let columns = sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?1)")
            .bind(&table)
            .fetch_all(&mut raw)
            .await
            .unwrap();
        let values = columns
            .iter()
            .map(|column| {
                format!(
                    "CASE WHEN typeof(\"{0}\")='blob' THEN hex(\"{0}\") ELSE \"{0}\" END",
                    column.replace('"', "\"\"")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let query = format!("SELECT json_array({values}) FROM main.\"{table}\" ORDER BY {values}");
        let rows = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query))
            .fetch_all(&mut raw)
            .await
            .unwrap();
        result.push((table, rows));
    }
    raw.close().await.unwrap();
    result
}

#[tokio::test]
async fn unsupported_version_preserves_library_data() {
    let fixture = super::support::fixture().await;
    super::support::server_playlist(&fixture.database, "Keep me", &[]).await;
    fixture.database.close().await.unwrap();
    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql("ALTER TABLE playlists ADD COLUMN irrelevant TEXT; ALTER TABLE playlists DROP COLUMN provider_revision; CREATE TABLE extra(value TEXT); INSERT INTO extra VALUES('keep in the original'); PRAGMA user_version=999;").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Database::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}

#[tokio::test]
async fn missing_scrobble_column_returns_error_without_replacing_user_data() {
    let fixture = super::support::fixture().await;
    super::support::server_playlist(&fixture.database, "Keep me", &[]).await;
    fixture.database.close().await.unwrap();
    let mut raw = connection(&fixture.path, false).await;
    sqlx::raw_sql("DROP INDEX scrobble_outbox_due_idx; ALTER TABLE scrobble_outbox DROP COLUMN next_attempt_at;").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Database::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}

#[tokio::test]
async fn single_server_migrations_rewrite_local_state_and_drop_the_server_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.sqlite");
    Database::open(&path).await.unwrap().close().await.unwrap();
    let mut raw = super::support::connection(&path).await;
    let old = |kind: &str, object: &str| {
        format!("slopsonic:source/{kind}/slopsonic%2Dsource%2Dabc/{object}")
    };
    let former = |kind: &str, object: &str| {
        format!("slopsonic:source/{kind}/slopsonic%2Dsource%2Dold/{object}")
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE download_files ADD COLUMN source_id TEXT;
         ALTER TABLE sources ADD COLUMN kind TEXT;
         ALTER TABLE sources ADD COLUMN normalized_name TEXT;
         ALTER TABLE sources ADD COLUMN artwork_digest BLOB;
         ALTER TABLE tracks ADD COLUMN source_key INTEGER;
         ALTER TABLE albums ADD COLUMN source_key INTEGER;
         ALTER TABLE artists ADD COLUMN source_key INTEGER;
         CREATE TABLE download_locators (
             download_access_file_key INTEGER PRIMARY KEY,
             media_uri TEXT NOT NULL CHECK(media_uri<>''),
             origin TEXT NOT NULL CHECK(origin='download'),
             path TEXT NOT NULL CHECK(path<>''), root TEXT NOT NULL, relative_path TEXT NOT NULL,
             access_uri TEXT NOT NULL CHECK(access_uri<>''),
             source_key INTEGER,
             UNIQUE(media_uri,origin)
         ) STRICT;
         CREATE TABLE download_access_metadata(access_uri TEXT PRIMARY KEY) STRICT;
         CREATE VIEW download_access_files AS SELECT * FROM download_locators;
         CREATE INDEX download_source_idx ON download_files(source_id,media_uri,encoding);
         INSERT INTO sources(source_key,object_id) VALUES(6,'slopsonic-source-old'),(7,'slopsonic-source-abc');
         INSERT INTO albums(album_key,source_key,object_id,media_uri,indexed) VALUES(1,7,'a','{album}',1);
         INSERT INTO tracks(track_key,source_key,object_id,media_uri,album_key,indexed)
             VALUES(1,7,'t1','{queued}',1,1),(2,7,'t2','{other}',1,1),(3,6,'t3','{stale_track}',NULL,1);
         INSERT INTO queue_entries(entry_id,track_key) VALUES(1,1),(2,3);
         INSERT INTO queue_positions(order_kind,position,entry_id) VALUES(0,0,1),(0,1,2);
         INSERT INTO favorite_outbox(media_uri,favorite,next_attempt_at)
             VALUES('{favorite}',1,0),('{stale_favorite}',1,0);
         INSERT INTO rating_outbox(media_uri,rating,next_attempt_at) VALUES('{queued}',4,0);
         INSERT INTO user_media_state(media_uri,favorite) VALUES('{favorite}',1),('{stale_favorite}',1);
         INSERT INTO download_files(media_uri,encoding,revision,path,managed,source_id)
             VALUES('{queued}','original','','file:///music/t1.flac',1,'slopsonic-source-abc'),
                   ('{stale_track}','original','','file:///music/t3.flac',1,'slopsonic-source-old');
         INSERT INTO download_locators(source_key,media_uri,origin,path,root,relative_path,access_uri)
             VALUES(7,'{queued}','download','/music/t1.flac','/music','t1.flac','file:///music/t1.flac'),
                   (6,'{stale_track}','download','/music/t3.flac','/music','t3.flac','file:///music/t3.flac');
         PRAGMA user_version=71;",
        album = old("album", "a"),
        queued = old("track", "t1"),
        other = old("track", "t2"),
        favorite = old("album", "a%2F1"),
        stale_track = former("track", "t3"),
        stale_favorite = former("album", "b"),
    )))
    .execute(&mut raw)
    .await
    .unwrap();
    raw.close().await.unwrap();

    Database::open(&path).await.unwrap().close().await.unwrap();

    let mut raw = super::support::connection(&path).await;
    let tracks: Vec<(i64, String, bool, Option<String>)> = sqlx::query_as(
        "SELECT track_key,media_uri,indexed,object_id FROM tracks ORDER BY track_key",
    )
    .fetch_all(&mut raw)
    .await
    .unwrap();
    // A queued track of an older server keeps its old URI and loses its object id, so it is
    // never sent to this one.
    assert_eq!(
        tracks,
        [
            (
                1,
                "slopsonic:track/t1".to_string(),
                false,
                Some("t1".into())
            ),
            (3, former("track", "t3"), false, None)
        ]
    );
    assert_eq!(
        uris(&mut raw, "SELECT media_uri FROM favorite_outbox").await,
        ["slopsonic:album/a%2F1"]
    );
    assert_eq!(
        uris(&mut raw, "SELECT media_uri FROM user_media_state").await,
        ["slopsonic:album/a%2F1"]
    );
    assert_eq!(
        uris(&mut raw, "SELECT media_uri FROM rating_outbox").await,
        ["slopsonic:track/t1"]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT rating FROM rating_outbox")
            .fetch_one(&mut raw)
            .await
            .unwrap(),
        2,
        "two old half-star units are one whole star"
    );
    assert_eq!(
        uris(&mut raw, "SELECT media_uri FROM download_files").await,
        ["slopsonic:track/t1"]
    );
    let count = |table: &str| format!("SELECT count(*) FROM {table}");
    for (table, expected) in [("albums", 0), ("queue_entries", 2), ("sources", 1)] {
        let rows: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(count(table)))
            .fetch_one(&mut raw)
            .await
            .unwrap();
        assert_eq!(rows, expected, "{table}");
    }
    let server: String = sqlx::query_scalar("SELECT object_id FROM sources")
        .fetch_one(&mut raw)
        .await
        .unwrap();
    assert_eq!(server, "server");
    assert_eq!(
        library::entity_parts("slopsonic:album/a%2F1"),
        Some(("album".into(), "a/1".into()))
    );
    assert_eq!(
        uris(&mut raw, "SELECT media_uri FROM download_files").await,
        ["slopsonic:track/t1"]
    );
    let source_column: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('download_files') WHERE name='source_id'",
    )
    .fetch_one(&mut raw)
    .await
    .unwrap();
    assert_eq!(source_column, 0);
    let key_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master master JOIN pragma_table_info(master.name) column
         WHERE master.type='table' AND master.name<>'sources' AND column.name='source_key'",
    )
    .fetch_one(&mut raw)
    .await
    .unwrap();
    assert_eq!(key_columns, 0);
    raw.close().await.unwrap();
    // The upgraded library keeps working: the queue restores and a sync writes the catalog again.
    let database = Database::open(&path).await.unwrap();
    database.reconcile_server("Server").await.unwrap();
    let mut catalog = database.catalog(None).await.unwrap();
    let mut batch = library::CatalogBatch::default();
    batch.tracks.push(library::TrackObservation {
        object_id: "t1".into(),
        ..Default::default()
    });
    catalog.apply_catalog(&batch, true).await.unwrap();
    assert!(
        database
            .is_server_track("slopsonic:track/t1", &library::ReadCancellation::new())
            .await
            .unwrap()
    );
    database.close().await.unwrap();
}

async fn uris(raw: &mut SqliteConnection, sql: &'static str) -> Vec<String> {
    sqlx::query_scalar(sql).fetch_all(raw).await.unwrap()
}
