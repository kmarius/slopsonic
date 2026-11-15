use super::*;
use crate::{HomeEntryInput, NativeUserState, PlaylistEntryObservation};

pub(super) async fn headers(
    connection: &mut SqliteConnection,
    albums: &[&AlbumObservation],
    folders: &[FolderObservation],
    observed: Option<i64>,
) -> LibraryResult<bool> {
    let mut changed = false;
    super::cache::index_credits(
        connection,
        albums
            .iter()
            .flat_map(|album| &album.metadata.album_artists),
    )
    .await?;
    super::cache::index_tags(
        connection,
        "genre",
        albums.iter().flat_map(|album| &album.metadata.genres),
    )
    .await?;
    let keys: Vec<i64> = sqlx::query_scalar(
        "SELECT album_key FROM albums WHERE object_id IN(SELECT value FROM json_each(?1))",
    )
    .bind(serde_json::to_string(
        &albums
            .iter()
            .map(|album| &album.object_id)
            .collect::<Vec<_>>(),
    )?)
    .fetch_all(&mut *connection)
    .await?;
    changed |= !super::index::albums(connection, &keys).await?.is_empty();
    for folder in folders {
        let rows = serde_json::to_string(&folder.tracks)?;
        changed|=sqlx::query("INSERT OR IGNORE INTO track_folders(track_key,folder_key,position,observed_stamp) SELECT track.track_key,folder.folder_key,coalesce((SELECT max(position)+1 FROM track_folders WHERE track_key=track.track_key),0),coalesce(?3,0) FROM json_each(?2) item CROSS JOIN tracks track ON track.object_id=item.value CROSS JOIN folders folder ON folder.object_id=?1 WHERE folder.observed_stamp<=coalesce(?3,9223372036854775807)").bind(&folder.object_id).bind(&rows).bind(observed).execute(&mut *connection).await?.rows_affected()>0;
        sqlx::query("UPDATE track_folders SET observed_stamp=?3 WHERE folder_key=(SELECT folder_key FROM folders WHERE object_id=?1 AND observed_stamp<=coalesce(?3,9223372036854775807)) AND track_key IN(SELECT track.track_key FROM json_each(?2) item CROSS JOIN tracks track ON track.object_id=item.value) AND observed_stamp<?3").bind(&folder.object_id).bind(rows).bind(observed).execute(&mut *connection).await?;
    }
    Ok(changed)
}

pub(super) async fn playlists(
    connection: &mut SqliteConnection,
    inputs: &[PlaylistObservation],
    observed: Option<i64>,
    admit: bool,
    reference: bool,
) -> LibraryResult<bool> {
    let mut changed = false;
    for input in inputs {
        let row_changed = sqlx::query("WITH previous AS (SELECT artwork_binding FROM playlists WHERE object_id=?1),
            candidates AS (
                SELECT value FROM json_each(?4)
                UNION ALL SELECT candidate.value FROM previous CROSS JOIN json_each(previous.artwork_binding) candidate
                WHERE json_extract(candidate.value,'$.image.automatic') IS NOT NULL
                  AND NOT EXISTS(SELECT 1 FROM json_each(?4) incoming WHERE json_extract(incoming.value,'$.image.automatic') IS NOT NULL)
            ), binding AS (
                SELECT CASE WHEN ?9 AND ?4 IS NULL THEN (SELECT artwork_binding FROM previous)
                    ELSE CAST(nullif(json_group_array(json(value)),'[]') AS BLOB) END bytes FROM candidates
            ) INSERT INTO playlists(object_id,name,normalized_name,sort_text,artwork_binding,writable,metadata_writable,position,indexed,observed_stamp)
            VALUES(?1,?2,lower(?2),?3,(SELECT bytes FROM binding),coalesce(?5,1),coalesce(?6,1),(SELECT coalesce(max(position),-1)+1 FROM playlists),?7,coalesce(?8,0))
            ON CONFLICT(object_id) DO UPDATE SET name=excluded.name,normalized_name=excluded.normalized_name,sort_text=excluded.sort_text,artwork_binding=excluded.artwork_binding,writable=coalesce(?5,playlists.writable),metadata_writable=coalesce(?6,playlists.metadata_writable),indexed=max(playlists.indexed,excluded.indexed),observed_stamp=coalesce(?8,playlists.observed_stamp) WHERE playlists.observed_stamp<=coalesce(?8,9223372036854775807) AND(playlists.name,playlists.sort_text,playlists.artwork_binding,playlists.writable,playlists.metadata_writable,playlists.indexed) IS NOT(excluded.name,excluded.sort_text,excluded.artwork_binding,coalesce(?5,playlists.writable),coalesce(?6,playlists.metadata_writable),max(playlists.indexed,excluded.indexed))")
            .bind(&input.object_id).bind(&input.name).bind(&input.sort_text).bind(&input.artwork_binding).bind(input.writable).bind(input.metadata_writable).bind(admit).bind(observed).bind(reference).execute(&mut *connection).await?.rows_affected()>0;
        if row_changed {
            changed = true;
        }
        if let Some(stamp) = observed {
            sqlx::query(
                "UPDATE playlists SET observed_stamp=?2 WHERE object_id=?1 AND observed_stamp<?2",
            )
            .bind(&input.object_id)
            .bind(stamp)
            .execute(&mut *connection)
            .await?;
        }
    }
    Ok(changed)
}

pub(crate) async fn remove_items_on(
    connection: &mut SqliteConnection,
    kind: &str,
    ids: &[String],
    observed: Option<i64>,
) -> LibraryResult<CatalogUpdate> {
    let (table, key) = if kind == "playlist" {
        ("playlists", "playlist_key")
    } else {
        super::cache::table(kind)
    };
    let mut update = CatalogUpdate::default();
    let observed = super::cache::observation_stamp(connection, observed).await?;
    let keys:Vec<i64>=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {key} FROM {table} WHERE indexed=1 AND object_id IN(SELECT value FROM json_each(?1)) AND observed_stamp<=?2"))).bind(serde_json::to_string(ids)?).bind(observed).fetch_all(&mut *connection).await?;
    let albums: Vec<i64> = if kind == "track" {
        sqlx::query_scalar("SELECT DISTINCT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL").bind(serde_json::to_string(&keys)?).fetch_all(&mut *connection).await?
    } else {
        Vec::new()
    };
    let changed=sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE {table} SET indexed=0,observed_stamp=?2 WHERE object_id IN(SELECT value FROM json_each(?1)) AND observed_stamp<=?2"))).bind(serde_json::to_string(ids)?).bind(observed).execute(&mut *connection).await?.rows_affected()>0;
    if kind == "playlist" {
        update.playlists |= !keys.is_empty();
    } else {
        update.metadata |= !keys.is_empty();
    }
    if changed {
        super::index::albums(connection, if kind == "album" { &keys } else { &albums }).await?;
        super::artwork::refresh_choices(
            connection,
            if kind == "track" { &keys } else { &[] },
            if kind == "album" { &keys } else { &albums },
            if kind == "artist" { &keys } else { &[] },
            false,
        )
        .await?;
    }
    Ok(update)
}

pub(crate) async fn write_home_on(
    connection: &mut SqliteConnection,
    section: &str,
    title: Option<&str>,
    entries: &[HomeEntryInput],
    after: i64,
    last: bool,
) -> LibraryResult<CatalogUpdate> {
    let mut update = CatalogUpdate::default();
    let rows = serde_json::to_string(
        &entries
            .iter()
            .map(|entry| {
                (
                    entry.position,
                    entry.kind.as_str(),
                    &entry.entity_object_id,
                    &entry.title,
                    &entry.subtitle,
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let expected = "WITH expected AS(SELECT json_extract(value,'$[0]') position,json_extract(value,'$[1]') entity_kind,CASE json_extract(value,'$[1]') WHEN 'track' THEN(SELECT track_key FROM tracks WHERE object_id=json_extract(value,'$[2]')) WHEN 'album' THEN(SELECT album_key FROM albums WHERE object_id=json_extract(value,'$[2]')) WHEN 'artist' THEN(SELECT artist_key FROM artists WHERE object_id=json_extract(value,'$[2]')) WHEN 'playlist' THEN(SELECT playlist_key FROM playlists WHERE object_id=json_extract(value,'$[2]')) END entity_key,json_extract(value,'$[3]') title,json_extract(value,'$[4]') subtitle FROM json_each(?2)) ";
    let removed = sqlx::query(sqlx::AssertSqlSafe(format!("{expected} DELETE FROM home_entries WHERE section_id=?1 AND position>?4 AND(?5 IS NULL OR position<=?5) AND NOT EXISTS(SELECT 1 FROM expected WHERE expected.entity_key IS NOT NULL AND expected.position=home_entries.position AND(expected.entity_kind,expected.entity_key,expected.title,expected.subtitle,?3) IS(home_entries.entity_kind,home_entries.entity_key,home_entries.title,home_entries.subtitle,home_entries.section_title))"))).bind(section).bind(&rows).bind(title).bind(after).bind(if last { None } else { entries.last().map(|entry|entry.position) }).execute(&mut *connection).await?.rows_affected();
    let added = sqlx::query(sqlx::AssertSqlSafe(format!("{expected} INSERT OR IGNORE INTO home_entries(section_id,position,entity_kind,entity_key,title,subtitle,section_title) SELECT ?1,position,entity_kind,entity_key,title,subtitle,?3 FROM expected WHERE entity_key IS NOT NULL"))).bind(section).bind(rows).bind(title).execute(&mut *connection).await?.rows_affected();
    update.home = removed > 0 || added > 0;
    Ok(update)
}

impl Database {
    /// The albums among `played` (object id, the server's last play) whose tracks' stored
    /// plays are all older, so their tracks' play state is behind the server's.
    pub async fn albums_played_since(
        &self,
        played: &[(String, i64)],
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT json_extract(requested.value,'$[0]') FROM json_each(?1) requested WHERE NOT EXISTS(SELECT 1 FROM albums album JOIN tracks track USING(album_key) JOIN native_media_state state ON state.media_uri=track.media_uri WHERE album.object_id=json_extract(requested.value,'$[0]') AND state.last_played>=json_extract(requested.value,'$[1]'))")
            .bind(serde_json::to_string(played)?).fetch_all(&mut *connection).await?)
    }

    pub async fn source_contains_entity(
        &self,
        kind: &str,
        id: &str,
        indexed: bool,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        let (table, _) = if kind == "playlist" {
            ("playlists", "playlist_key")
        } else {
            super::cache::table(kind)
        };
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {table}.object_id=?1 AND(?2=0 OR {table}.indexed=1))"))).bind(id).bind(indexed).fetch_one(&mut *connection).await?)
    }

    pub async fn omitted_album_track_page(
        &self,
        id: &str,
        observed: i64,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT track.object_id FROM tracks track JOIN albums album USING(album_key) WHERE album.object_id=?1 AND track.indexed=1 AND track.observed_stamp<?2 AND track.object_id>coalesce(?3,'') ORDER BY track.object_id LIMIT ?4").bind(id).bind(observed).bind(after).bind(limit.min(128) as i64).fetch_all(&mut *connection).await?)
    }

    pub async fn source_playlist_is_fresh(
        &self,
        id: &str,
        revision: Option<&str>,
        now: i64,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM playlists WHERE playlists.object_id=?1 AND provider_revision IS ?2 AND(?2 IS NOT NULL OR playlists.valid_until>=?3))").bind(id).bind(revision).bind(now).fetch_one(&mut *connection).await?)
    }
}

impl super::Catalog<'_> {
    pub async fn replace_source_home_section(
        &mut self,
        section: &str,
        title: Option<&str>,
        entries: &[HomeEntryInput],
    ) -> LibraryResult<()> {
        self.write_source_home_page(section, title, entries, -1, true)
            .await
    }

    pub(crate) async fn write_source_home_page(
        &mut self,
        section: &str,
        title: Option<&str>,
        entries: &[HomeEntryInput],
        after: i64,
        last: bool,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let update = write_home_on(&mut tx, section, title, entries, after, last).await?;
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn write_source_playlist_page(
        &mut self,
        id: &str,
        offset: usize,
        entries: &[PlaylistEntryObservation],
        complete: bool,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let playlist: i64 =
            sqlx::query_scalar("SELECT playlist_key FROM playlists WHERE object_id=?1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        let mut update = CatalogUpdate::default();
        if offset == 0 {
            sqlx::query("DELETE FROM native_playlist_pending WHERE playlist_key=?1")
                .bind(playlist)
                .execute(&mut *tx)
                .await?;
        }
        for (page_index, page) in entries.chunks(128).enumerate() {
            let rows = serde_json::to_string(
                &page
                    .iter()
                    .enumerate()
                    .map(|(position, entry)| {
                        (
                            &entry.object_id,
                            &entry.track_id,
                            offset + page_index * 128 + position,
                            crate::entity_uri("track", &entry.track_id),
                        )
                    })
                    .collect::<Vec<_>>(),
            )?;
            sqlx::query("INSERT INTO tracks(object_id,media_uri,indexed) SELECT json_extract(value,'$[1]'),json_extract(value,'$[3]'),0 FROM json_each(?1) WHERE true ON CONFLICT(object_id) DO NOTHING ON CONFLICT(media_uri) DO UPDATE SET object_id=excluded.object_id,observed_stamp=0 WHERE tracks.object_id IS NULL").bind(&rows).execute(&mut *tx).await?;
            sqlx::query("INSERT OR REPLACE INTO native_playlist_pending(playlist_key,object_id,track_key,position) SELECT ?1,json_extract(item.value,'$[0]'),track.track_key,json_extract(item.value,'$[2]') FROM json_each(?2) item CROSS JOIN tracks track ON track.object_id=json_extract(item.value,'$[1]')").bind(playlist).bind(rows).execute(&mut *tx).await?;
        }
        if complete {
            update.playlists=sqlx::query_scalar("SELECT EXISTS(SELECT object_id,track_key,position FROM playlist_entries WHERE playlist_key=?1 EXCEPT SELECT object_id,track_key,position FROM native_playlist_pending WHERE playlist_key=?1) OR EXISTS(SELECT object_id,track_key,position FROM native_playlist_pending WHERE playlist_key=?1 EXCEPT SELECT object_id,track_key,position FROM playlist_entries WHERE playlist_key=?1)").bind(playlist).fetch_one(&mut *tx).await?;
            if update.playlists {
                sqlx::query("DELETE FROM playlist_entries WHERE playlist_key=?1 AND object_id NOT IN(SELECT object_id FROM native_playlist_pending WHERE playlist_key=?1)").bind(playlist).execute(&mut *tx).await?;
                sqlx::query("UPDATE playlist_entries SET position=position+(SELECT coalesce(max(position),0)+1 FROM playlist_entries WHERE playlist_key=?1)+(SELECT coalesce(max(position),0)+1 FROM native_playlist_pending WHERE playlist_key=?1) WHERE playlist_key=?1").bind(playlist).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO playlist_entries(playlist_key,object_id,track_key,position) SELECT playlist_key,object_id,track_key,row_number() OVER(ORDER BY position,object_id)-1 FROM native_playlist_pending WHERE playlist_key=?1 ON CONFLICT(playlist_key,object_id) DO UPDATE SET track_key=excluded.track_key,position=excluded.position").bind(playlist).execute(&mut *tx).await?;
            }
            sqlx::query("DELETE FROM native_playlist_pending WHERE playlist_key=?1")
                .bind(playlist)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn write_native_user_data(
        &mut self,
        kind: &str,
        rows: &[(String, NativeUserState)],
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let update = super::cache::users(
            &mut tx,
            kind,
            &rows
                .iter()
                .map(|(id, state)| (id.as_str(), state))
                .collect::<Vec<_>>(),
            None,
        )
        .await?;
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn replace_source_favorites(
        &mut self,
        kind: &str,
        ids: &[String],
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let (table, _) = super::cache::table(kind);
        let mut update = CatalogUpdate::default();
        update.metadata = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO native_media_state(media_uri,favorite) SELECT media_uri,object_id IN(SELECT value FROM json_each(?1)) FROM {table} WHERE object_id IS NOT NULL ON CONFLICT(media_uri) DO UPDATE SET favorite=excluded.favorite WHERE favorite IS NOT excluded.favorite"))).bind(serde_json::to_string(ids)?).execute(&mut *tx).await?.rows_affected() > 0;
        tx.commit().await?;
        self.changes.merge(update);
        Ok(())
    }
    pub async fn remove_source_items(&mut self, kind: &str, ids: &[String]) -> LibraryResult<()> {
        for page in ids.chunks(128) {
            let mut writer = self.database.writer().await?;
            let mut tx = writer
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
                .begin()
                .await?;
            let update = remove_items_on(&mut tx, kind, page, self.observed).await?;
            tx.commit().await?;
            self.changes.merge(update);
        }
        Ok(())
    }
    pub async fn complete_album_observation(
        &mut self,
        id: &str,
        observed: i64,
    ) -> LibraryResult<()> {
        let mut update = CatalogUpdate::default();
        loop {
            let mut writer = self.database.writer().await?;
            let mut tx = writer
                .as_mut()
                .ok_or(LibraryError::WriterUnavailable)?
                .begin()
                .await?;
            let result = &mut update;
            let keys:Vec<i64>=sqlx::query_scalar("SELECT track_key FROM tracks WHERE album_key=(SELECT album_key FROM albums WHERE object_id=?1) AND observed_stamp<?2 ORDER BY track_key LIMIT 128").bind(id).bind(observed).fetch_all(&mut *tx).await?;
            if keys.is_empty() {
                tx.commit().await?;
                self.changes.merge(update.clone());
                break;
            }
            let albums:Vec<i64>=sqlx::query_scalar("SELECT DISTINCT album_key FROM tracks WHERE track_key IN(SELECT value FROM json_each(?1)) AND album_key IS NOT NULL").bind(serde_json::to_string(&keys)?).fetch_all(&mut *tx).await?;
            sqlx::query("UPDATE tracks SET album_key=NULL,observed_stamp=?2 WHERE track_key IN(SELECT value FROM json_each(?1))").bind(serde_json::to_string(&keys)?).bind(observed).execute(&mut *tx).await?;
            super::index::albums(&mut tx, &albums).await?;
            super::artwork::refresh_choices(&mut tx, &keys, &albums, &[], false).await?;
            result.metadata = true;
            tx.commit().await?;
            self.changes.merge(update.clone());
        }
        self.changes.merge(update);
        Ok(())
    }
    pub async fn write_source_playlist_freshness(
        &mut self,
        id: &str,
        revision: Option<&str>,
        valid_until: Option<i64>,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::query("UPDATE playlists SET provider_revision=?2,valid_until=?3 WHERE object_id=?1 AND(provider_revision,valid_until) IS NOT(?2,?3)").bind(id).bind(revision).bind(valid_until).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn complete_source_folder_membership(
        &mut self,
        id: &str,
        observed: i64,
    ) -> LibraryResult<()> {
        let mut writer = self.database.writer().await?;
        let mut tx = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let tracks:Vec<i64>=sqlx::query_scalar("DELETE FROM track_folders WHERE folder_key=(SELECT folder_key FROM folders WHERE object_id=?1) AND observed_stamp<?2 RETURNING track_key").bind(id).bind(observed).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        self.changes.metadata |= !tracks.is_empty();
        Ok(())
    }
}
