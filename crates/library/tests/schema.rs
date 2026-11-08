use std::path::Path;

use library::{Database, ReadCancellation, SourceId};
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

#[tokio::test]
async fn reopening_without_statistics_preserves_library_order() {
    let fixture = super::support::fixture().await;
    let before = fixture
        .database
        .selected_track_uris(
            &library::MemberRead {
                collection: library::Collection::Source(fixture.source),
                sort: Some(library::CatalogSort::Title),
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
            }
            .selection(),
            None,
            &ReadCancellation::new(),
        )
        .await
        .unwrap();
    let mut raw = super::support::connection(&fixture.path).await;
    sqlx::raw_sql("DROP TABLE IF EXISTS sqlite_stat1;
        INSERT INTO listens(media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped)
            SELECT media_uri,'Captured title','Captured artist','Captured album',100,'1970-01',180000,90000,1 FROM tracks LIMIT 1")
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
                collection: library::Collection::Source(fixture.source),
                sort: Some(library::CatalogSort::Title),
                ..library::MemberRead::new(library::Collection::Source(fixture.source))
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
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%local_play_count%'",
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
        "DROP INDEX tracks_local_play_order_idx; DROP INDEX tracks_global_play_order_idx;
        DROP INDEX tracks_local_shuffle_idx; DROP INDEX tracks_global_local_shuffle_idx;
        ALTER TABLE tracks DROP COLUMN local_play_count;",
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
        "DROP INDEX tracks_local_shuffle_idx; CREATE TABLE tracks_local_shuffle_idx(value TEXT);",
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
    sqlx::query("INSERT INTO user_media_state(media_uri,favorite,rating) VALUES(?1,1,87)")
        .bind(uri)
        .execute(&mut raw)
        .await
        .unwrap();
    sqlx::query("INSERT INTO listens(external_id,source_id,media_uri,track_title,artist_name,album_title,started_at,local_period,duration_millis,listened_millis,skipped) VALUES(NULL,'source',?1,'Snapshot','Artist','Album',100,'1970-01',100000,90000,0)").bind(uri).execute(&mut raw).await.unwrap();
    sqlx::raw_sql("INSERT INTO listen_outbox(listen_key,service,account_id,attempts,next_attempt_at,last_error) SELECT listen_key,'lastfm','account',3,200,'retry' FROM listens;").execute(&mut raw).await.unwrap();
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
        let (_, _, object) = library::source_entity_parts(uri).unwrap();
        database
            .reconcile_source(&SourceId::new("source"), "subsonic", "Source")
            .await
            .unwrap();
        let mut catalog = database.catalog("source", None).await.unwrap();
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
        assert_eq!(sqlx::query_as::<_,(bool,i64)>("SELECT state.favorite,state.rating FROM tracks track JOIN user_media_state state USING(media_uri) WHERE track.media_uri=?1").bind(uri).fetch_one(&mut raw).await.unwrap(),(true,87));
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
async fn missing_listen_identity_returns_error_without_replacing_user_data() {
    let fixture = super::support::fixture().await;
    super::support::server_playlist(&fixture.database, "Keep me", &[]).await;
    fixture.database.close().await.unwrap();
    let mut raw = connection(&fixture.path, false).await;
    let triggers: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type='trigger' AND sql LIKE '%listens%'",
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
    sqlx::raw_sql("DROP INDEX listens_media_activity_idx; DROP INDEX listens_connect_key_idx; ALTER TABLE listens DROP COLUMN media_uri;").execute(&mut raw).await.unwrap();
    raw.close().await.unwrap();
    let before = durable_rows(&fixture.path).await;
    assert!(Database::open(&fixture.path).await.is_err());
    assert_eq!(durable_rows(&fixture.path).await, before);
}
