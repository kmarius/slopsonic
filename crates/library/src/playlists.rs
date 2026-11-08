//! Owns Playlist identity, exact duplicate occurrences, ordering, filtering, and edits.
//! Entries keep song references; song metadata belongs to the shared library.

use sqlx::{Connection, FromRow, SqliteConnection};

use crate::{
    CatalogSort, Database, FolderKey, LibraryError, LibraryResult, MemberSelection,
    PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey, TrackKey,
};

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct PlaylistAccess {
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub object_id: String,
    pub writable: bool,
    pub metadata_writable: bool,
}

pub(crate) const PLAYLIST_ROW_LIMIT: usize = 128;

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct PlaylistRow {
    pub playlist_key: PlaylistKey,
    pub source_key: Option<SourceKey>,
    pub source_id: Option<String>,
    pub object_id: String,
    pub name: String,
    pub writable: bool,
    pub metadata_writable: bool,
    pub artwork_binding: Option<Vec<u8>>,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    #[sqlx(json)]
    pub genres: Vec<PlaylistGenreLink>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct PlaylistGenreLink {
    pub genre_key: crate::GenreKey,
    pub name: String,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct PlaylistEditEntry {
    pub playlist_entry_key: PlaylistEntryKey,
    pub track_key: TrackKey,
    pub object_id: String,
    #[sqlx(try_from = "i64")]
    pub position: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, FromRow)]
pub struct PlaylistEntryView {
    pub playlist_entry_key: PlaylistEntryKey,
    pub position: i64,
    pub source_entry_id: Option<String>,
    #[sqlx(flatten)]
    pub track: crate::TrackView,
}

impl From<PlaylistEntryView> for crate::TrackReference {
    fn from(entry: PlaylistEntryView) -> Self {
        entry.track.into()
    }
}

impl Database {
    pub async fn playlist_access(
        &self,
        playlist: PlaylistKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistAccess>> {
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as("SELECT playlist.source_key,source.object_id source_id,playlist.object_id,
            playlist.writable,playlist.metadata_writable
            FROM playlists playlist LEFT JOIN sources source USING(source_key) WHERE playlist_key=?1")
            .bind(playlist).fetch_optional(&mut *connection).await?)
    }

    pub async fn playlist_key_by_object(
        &self,
        source: SourceKey,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistKey>> {
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT playlist_key FROM playlists WHERE source_key=?1 AND object_id=?2",
        )
        .bind(source)
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn playlist_key_by_identity(
        &self,
        source_id: Option<&crate::SourceId>,
        object_id: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<PlaylistKey>> {
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT playlist_key FROM playlists
             WHERE source_key IS (SELECT source_key FROM sources WHERE object_id=?1)
               AND (?1 IS NULL OR source_key IS NOT NULL) AND object_id=?2 AND name IS NOT NULL",
        )
        .bind(source_id.map(crate::SourceId::as_str))
        .bind(object_id)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn all_playlist_track_order(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT track.media_uri FROM tracks track
             WHERE track.source_key=?1
               AND EXISTS (SELECT 1 FROM playlist_entries entry WHERE entry.track_key=track.track_key)
               AND (?2 IS NULL OR EXISTS (SELECT 1 FROM track_folders scope WHERE scope.track_key=track.track_key AND scope.folder_key=?2))
             ORDER BY track.sort_text,track.track_key",
        )
        .bind(source)
        .bind(folder)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn playlist_destinations(
        &self,
        source: Option<SourceKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let mut connection = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let query = playlist_list_query(source, None, CatalogSort::Position, false, "");
        let order: Vec<PlaylistKey> = sqlx::query_scalar_with(
            sqlx::AssertSqlSafe(query.select(&query.key)),
            query.arguments,
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut rows = Vec::with_capacity(order.len());
        for keys in order.chunks(PLAYLIST_ROW_LIMIT) {
            rows.extend(Self::load_playlist_rows(&mut transaction, keys).await?);
        }
        rows.retain(|row| row.writable);
        transaction.commit().await?;
        Ok(rows)
    }

    pub(crate) async fn load_playlist_rows(
        connection: &mut SqliteConnection,
        keys: &[PlaylistKey],
    ) -> LibraryResult<Vec<PlaylistRow>> {
        Self::load_playlist_requested(connection, crate::query::RequestedKeys::batch(keys)?).await
    }

    pub async fn playlist_edit_entries(
        &self,
        playlist: PlaylistKey,
        selection: &MemberSelection,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistEditEntry>> {
        let mut connection = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let selected = crate::selection::selected_tracks_on(&mut transaction, selection).await?;
        let keys = selected
            .into_iter()
            .map(|(_, entry)| {
                entry.ok_or_else(|| {
                    LibraryError::InvalidRequest(
                        "Selection does not contain playlist entries".into(),
                    )
                })
            })
            .collect::<LibraryResult<Vec<_>>>()?;
        let mut entries = sqlx::query_as::<_, PlaylistEditEntry>(
            "SELECT entry.playlist_entry_key,entry.track_key,entry.object_id,entry.position
             FROM json_each(?1) requested JOIN playlist_entries entry ON entry.playlist_entry_key=requested.value
             WHERE entry.playlist_key=?2 ORDER BY entry.position"
        ).bind(serde_json::to_string(&keys)?).bind(playlist).fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        entries.sort_by_key(|entry| entry.position);
        Ok(entries)
    }

    pub async fn source_playlist_object_ids(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT object_id FROM playlists WHERE source_key=?1 AND name IS NOT NULL AND indexed ORDER BY sort_text,playlist_key",
        )
        .bind(source)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn source_playlist_track_ids(
        &self,
        source: SourceKey,
        playlist: Option<PlaylistKey>,
        songs: &MemberSelection,
        skip_existing: bool,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        let mut transaction = connection.begin().await?;
        let tracks = playlist_tracks_on(&mut transaction, source, songs)
            .await?
            .ok_or_else(|| {
                LibraryError::InvalidRequest("Playlist Track is no longer current".into())
            })?;
        let ids = sqlx::query_scalar("SELECT track.object_id FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value
            WHERE NOT ?2 OR ?3 IS NULL OR NOT EXISTS(SELECT 1 FROM playlist_entries existing WHERE existing.playlist_key=?3 AND existing.track_key=track.track_key)
            ORDER BY CAST(requested.key AS INTEGER)")
            .bind(serde_json::to_string(&tracks)?).bind(skip_existing).bind(playlist)
            .fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(ids)
    }
}

async fn playlist_tracks_on(
    connection: &mut SqliteConnection,
    source: SourceKey,
    selection: &MemberSelection,
) -> LibraryResult<Option<Vec<TrackKey>>> {
    let mut pending = vec![selection];
    let mut tracks = Vec::new();
    while let Some(selection) = pending.pop() {
        match selection {
            MemberSelection::Concat(parts) => pending.extend(parts.iter().rev()),
            MemberSelection::Uris(uris) => {
                let missing:Vec<String>=sqlx::query_scalar("SELECT DISTINCT requested.value FROM json_each(?1) requested WHERE NOT EXISTS(SELECT 1 FROM tracks WHERE media_uri=requested.value)").bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
                if !missing.is_empty() {
                    return Ok(None);
                }
                tracks.extend(
                    crate::selection::selected_tracks_on(connection, selection)
                        .await?
                        .into_iter()
                        .map(|(key, _)| key),
                );
            }
            _ => tracks.extend(
                crate::selection::selected_tracks_on(connection, selection)
                    .await?
                    .into_iter()
                    .map(|(track, _)| track),
            ),
        }
    }
    if sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM json_each(?1) requested JOIN tracks ON tracks.track_key=requested.value WHERE tracks.source_key IS NOT ?2)")
            .bind(serde_json::to_string(&tracks)?).bind(source).fetch_one(&mut *connection).await?
    {
        let id: String = sqlx::query_scalar("SELECT object_id FROM sources WHERE source_key=?1").bind(source).fetch_one(&mut *connection).await?;
        return Err(LibraryError::PlaylistSourceMismatch(crate::SourceId::new(id)));
    }
    Ok(Some(tracks))
}

#[cfg(test)]
mod point_projection_tests {
    use super::*;

    #[tokio::test]
    async fn small_native_playlist_reads_stay_bounded_as_unrelated_catalog_grows() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let directory = tempfile::tempdir().unwrap();
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .unwrap();
        let mut writer = database.writer().await.unwrap();
        let connection = writer.as_mut().unwrap();
        sqlx::raw_sql("INSERT INTO sources(source_key,object_id,display_name,normalized_name,artwork_digest) VALUES (1,'source','Source','source',zeroblob(32));\n          INSERT INTO playlists(playlist_key,source_key,object_id,name,normalized_name,sort_text,position)\n          VALUES(1,1,'small','Small','small','small',0),(2,1,'large','Large','large','large',1);\n          WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<15)\n          INSERT INTO tracks(track_key,source_key,object_id,media_uri,native_artwork,metadata) SELECT x,1,CAST(x AS TEXT),'track:'||x,X'01',jsonb(json_object('title',COALESCE('Title',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('title',''),'duration_millis',COALESCE(1000,0))) FROM n;\n          INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)\n          SELECT 1,object_id,track_key,track_key FROM tracks;")
            .execute(&mut *connection).await.unwrap();
        for grow in [false, true] {
            if grow {
                sqlx::raw_sql("WITH RECURSIVE n(x) AS (VALUES(16) UNION ALL SELECT x+1 FROM n WHERE x<10000)\n                  INSERT INTO tracks(track_key,source_key,object_id,media_uri,native_artwork,metadata) SELECT x,1,CAST(x AS TEXT),'track:'||x,X'01',jsonb(json_object('title',COALESCE('Title',''),'album',COALESCE('',''),'artist',COALESCE('',''),'sort_text',COALESCE('title',''),'duration_millis',COALESCE(1000,0))) FROM n;\n                  INSERT INTO playlist_entries(playlist_key,object_id,track_key,position)\n                  SELECT 2,object_id,track_key,track_key FROM tracks;")
                    .execute(&mut *connection).await.unwrap();
            }
            let work = Arc::new(AtomicUsize::new(0));
            let counter = work.clone();
            connection
                .lock_handle()
                .await
                .unwrap()
                .set_progress_handler(100, move || {
                    counter.fetch_add(100, Ordering::Relaxed);
                    true
                });
            let rows = Database::load_playlist_rows(connection, &[PlaylistKey::from_raw(1)])
                .await
                .unwrap();
            let entry: String = sqlx::query_scalar(
                "SELECT media_uri FROM playlist_entries JOIN tracks USING(track_key) WHERE playlist_entry_key=1",
            )
            .fetch_one(&mut *connection)
            .await
            .unwrap();
            let order: Vec<i64> = sqlx::query_scalar("SELECT playlist_entry_key FROM playlist_entries WHERE playlist_key=1 ORDER BY position,playlist_entry_key")
                .fetch_all(&mut *connection).await.unwrap();
            for sort in [CatalogSort::Artist, CatalogSort::AlbumArtist] {
                let query = playlist_query(PlaylistKey::from_raw(1), None, sort, false, "");
                let ordered = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(
                    query.select(&query.entry_key),
                ))
                .fetch_all(&mut *connection)
                .await
                .unwrap();
                assert_eq!(ordered.len(), 15);
            }
            connection
                .lock_handle()
                .await
                .unwrap()
                .remove_progress_handler();
            assert_eq!(rows[0].track_count, 15);
            assert!(rows[0].artwork_binding.is_some());
            assert_eq!(entry, "track:1");
            assert_eq!(order.len(), 15);
            assert!(
                work.load(Ordering::Relaxed) < 5000,
                "small playlist visited unrelated rows: {} instructions",
                work.load(Ordering::Relaxed)
            );
        }
    }
}

impl Database {
    pub(crate) async fn load_playlist_requested(
        connection: &mut SqliteConnection,
        requested: crate::query::RequestedKeys,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        let sql = format!(
            "WITH requested AS ({})
         SELECT playlist.playlist_key,playlist.source_key,source.object_id source_id,
                playlist.object_id,
                playlist.name,playlist.writable,playlist.metadata_writable,{} artwork_binding,
                COALESCE(totals.track_count,0) track_count,COALESCE(totals.duration_millis,0) duration_millis,COALESCE(totals.downloaded_count,0) downloaded_count,
                (SELECT json_group_array(json_object('genre_key',genre_key,'name',name)) FROM (
                    SELECT genre.genre_key,genre.name FROM playlist_genres relation JOIN genres genre USING(genre_key)
                    WHERE relation.playlist_key=playlist.playlist_key
                    ORDER BY relation.track_count DESC,genre.sort_text,genre.genre_key LIMIT 2
                )) genres
         FROM requested JOIN playlists playlist ON playlist.playlist_key=requested.value
         {} LEFT JOIN sources source USING(source_key)
         WHERE playlist.name IS NOT NULL ORDER BY requested.ordinal",
            requested.sql,
            crate::artwork::preview_sql("playlist.artwork_binding"),
            crate::collections::global_totals_join("playlist", false)
        );
        let mut rows = sqlx::query_as_with::<_, PlaylistRow, _>(
            sqlx::AssertSqlSafe(sql),
            requested.arguments.clone(),
        )
        .fetch_all(&mut *connection)
        .await?;
        for row in &mut rows {
            row.artwork_binding = Some(crate::artwork::playlist_artwork(
                row.artwork_binding.take(),
                (row.source_id.clone(), row.object_id.clone()),
                (None, None),
            )?);
        }
        Ok(rows)
    }
}

pub(crate) async fn load_playlist_entry_requested(
    connection: &mut SqliteConnection,
    requested: crate::query::RequestedKeys,
) -> LibraryResult<Vec<PlaylistEntryView>> {
    let songs = crate::query::RequestedKeys {
        sql: "SELECT entry.track_key value,entries.ordinal FROM entries JOIN playlist_entries entry ON entry.playlist_entry_key=entries.value".into(),
        arguments: Default::default(),
    };
    let sql = format!(
        "WITH entries AS ({}),songs AS ({})
         SELECT entry.playlist_entry_key,entry.position,entry.object_id source_entry_id,songs.*
         FROM entries JOIN playlist_entries entry ON entry.playlist_entry_key=entries.value
         JOIN songs ON songs.__ordinal=entries.ordinal ORDER BY entries.ordinal",
        requested.sql,
        crate::tracks::track_requested_sql(&songs)
    );
    let rows = sqlx::query_as_with::<_, PlaylistEntryView, _>(
        sqlx::AssertSqlSafe(sql),
        requested.arguments,
    )
    .fetch_all(&mut *connection)
    .await?;
    Ok(rows)
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct PlaylistEntryWrite {
    pub object_id: String,
    pub position: i64,
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub track: crate::TrackReference,
}

pub(crate) fn playlist_query(
    key: PlaylistKey,
    folder: Option<FolderKey>,
    sort: CatalogSort,
    descending: bool,
    filter: &str,
) -> crate::query::OrderedQuery {
    let mut query = crate::query::OrderedQuery {

        key: "entry.playlist_entry_key".into(),
        text: sort.section_text(true),
        from: "playlist_entries entry JOIN playlists playlist USING(playlist_key) JOIN tracks track USING(track_key)".into(),
        predicate: format!("entry.playlist_key={}", key.raw()),
        entry_key: "entry.playlist_entry_key".into(),
        arguments: Default::default(),
        order: sort.order_terms(descending, true)
    };
    if sort != CatalogSort::Position {
        query.order.push("entry.position ASC".into());
    }
    if let Some(folder) = folder {
        query.predicate.push_str(&format!(" AND EXISTS(SELECT 1 FROM tracks track JOIN track_folders scope USING(track_key) WHERE track.track_key=entry.track_key AND scope.folder_key={})",folder.raw()));
    }
    crate::tracks::track_filter(&mut query, None, filter, false);
    query
}

pub(crate) fn playlist_list_query(
    source: Option<SourceKey>,
    folder: Option<FolderKey>,
    sort: CatalogSort,
    descending: bool,
    filter: &str,
) -> crate::query::OrderedQuery {
    let source_scope = source.map_or_else(
        || "playlist.source_key IS NULL".into(),
        |source| format!("playlist.source_key={}", source.raw()),
    );
    let mut predicate =
        format!("playlist.name IS NOT NULL AND playlist.indexed AND {source_scope}");
    if !filter.trim().is_empty() {
        predicate.push_str(&format!(
            " AND instr(playlist.normalized_name,{})>0",
            crate::query::quote(&filter.trim().to_lowercase())
        ));
    }
    let field = match sort {
        CatalogSort::Position => "playlist.position",
        CatalogSort::Title => "playlist.sort_text",
        CatalogSort::TrackCount => "COALESCE(totals.track_count,0)",
        CatalogSort::Duration => "COALESCE(totals.duration_millis,0)",
        _ => unreachable!("playlist sort field"),
    };
    let direction = if descending { "DESC" } else { "ASC" };
    crate::query::OrderedQuery {
        key: "playlist.playlist_key".into(),
        text: (sort == CatalogSort::Title).then(|| "playlist.sort_text".into()),
        from: format!(
            "{} {}",
            if let (Some(source), Some(folder)) = (source, folder) {
                format!(
                    "(SELECT scope.entity_key playlist_key FROM collection_totals scope CROSS JOIN playlists parent ON parent.playlist_key=scope.entity_key WHERE scope.kind='playlist' AND scope.role=0 AND coalesce(scope.folder_key,'')={} AND scope.track_count>0 AND parent.source_key={} UNION ALL SELECT playlist_key FROM playlists WHERE source_key IS NULL) candidate CROSS JOIN playlists playlist USING(playlist_key)",
                    folder.raw(),
                    source.raw()
                )
            } else {
                "playlists playlist".into()
            },
            crate::collections::global_totals_join("playlist", false)
        ),
        predicate,
        order: vec![if sort == CatalogSort::Title {
            format!("{field} {direction},playlist.playlist_key")
        } else {
            format!("{field} {direction},playlist.sort_text,playlist.playlist_key")
        }],
        entry_key: "NULL".into(),
        arguments: Default::default(),
    }
}
