//! Persists bounded Local observations, dependency paths, and point playback access.
//! Walking, parsing, retries, and filesystem policy remain owned by Sources.

use sqlx::{Connection, FromRow};

use crate::{
    Database, LibraryError, LibraryResult, LocalAccessFileKey, ReadCancellation, SourceKey,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DownloadStatus {
    pub total: u64,
    pub downloaded: u64,
}

#[derive(Debug, FromRow)]
pub struct DownloadMetadata {
    pub channels: Option<u32>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub disc_number: i64,
    pub track_number: i64,
    pub duration_millis: i64,
    pub source_format: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, FromRow)]
pub(crate) struct LocalLocatorWrite {
    pub source_id: Option<String>,
    pub media_uri: String,
    pub origin: String,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub access_uri: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalAccessWrite {
    pub media_uri: String,
    pub origin: LocalAccessOrigin,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub size_bytes: i64,
    pub mtime_ns: i64,
    pub device_id: Option<i64>,
    pub inode: Option<i64>,
    pub parser_version: i64,
    pub access_uri: String,
    pub loudness_analysis_key: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum LocalAccessOrigin {
    Download,
}

impl LocalAccessOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
        }
    }
}

impl Database {
    pub async fn downloaded_count(&self, source_id: Option<&str>) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        let count = if let Some(source_id) = source_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.local_locators
                 WHERE source_key=(SELECT source_key FROM main.sources WHERE object_id=?1)
                   AND origin='download'",
            )
            .bind(source_id)
            .fetch_one(&mut *connection)
            .await
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.local_locators
                 WHERE source_key IS NULL AND origin='download'",
            )
            .fetch_one(&mut *connection)
            .await
        };
        Ok(usize::try_from(count?).unwrap_or_default())
    }

    pub async fn download_metadata(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<DownloadMetadata>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as(
            "SELECT COALESCE(title,media_uri) title,COALESCE(display_artist,'') artist,COALESCE(display_album,'') album,COALESCE(disc_number,0) disc_number,COALESCE(track_number,0) track_number,
                    COALESCE(duration_millis,0) duration_millis,source_format,json_extract(metadata,'$.audio_properties.channels') channels
             FROM tracks WHERE media_uri=?1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn playback_access(
        &self,
        media_uri: &str,
    ) -> LibraryResult<Option<(String, LocalAccessOrigin)>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_as::<_, (String, LocalAccessOrigin)>(
            "SELECT access_uri,origin FROM local_access_files WHERE media_uri=?1 ORDER BY local_access_file_key LIMIT 1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn catalog_missing_media(
        &self,
        media: &[(String, crate::SourceId)],
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT json_extract(requested.value,'$[0]') FROM json_each(?1) requested
            WHERE EXISTS(SELECT 1 FROM main.sources WHERE object_id=json_extract(requested.value,'$[1]') AND catalog_complete=1)
              AND NOT EXISTS(SELECT 1 FROM main.tracks WHERE media_uri=json_extract(requested.value,'$[0]') AND indexed=1)")
            .bind(serde_json::to_string(media)?).fetch_all(&mut *connection).await?)
    }

    pub async fn source_counts(
        &self,
        source: SourceKey,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, usize)> {
        let (_permit, mut connection) = self.acquire_general(cancellation).await?;
        let(album,track)=sqlx::query_as::<_,(i64,i64)>("SELECT (SELECT count(*) FROM albums album WHERE source_key=?1 AND (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1))),(SELECT count(*) FROM tracks WHERE source_key=?1 AND indexed=1)").bind(source).fetch_one(&mut *connection).await?;
        Ok((
            usize::try_from(album).unwrap_or_default(),
            usize::try_from(track).unwrap_or_default(),
        ))
    }

    pub async fn all_source_counts(
        &self,
    ) -> LibraryResult<std::collections::HashMap<crate::SourceId, (usize, usize)>> {
        let mut connection = self.acquire_reader().await?;
        let rows = sqlx::query_as::<_, (String, i64, i64)>(
            "SELECT object_id,
                (SELECT count(*) FROM albums album WHERE source_key=source.source_key AND (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1))),
                (SELECT count(*) FROM tracks WHERE source_key=source.source_key AND indexed=1)
             FROM sources source",
        )
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, albums, tracks)| {
                (crate::SourceId::new(id), (albums as usize, tracks as usize))
            })
            .collect())
    }

    pub async fn write_local_access(
        &self,
        source: Option<SourceKey>,
        records: &[&LocalAccessWrite],
    ) -> LibraryResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        write_local_access_on(&mut transaction, source, records).await?;
        transaction.commit().await?;
        Ok(())
    }
}

async fn refresh_access_audio(
    connection: &mut sqlx::SqliteConnection,
    uris: &[&str],
) -> LibraryResult<()> {
    let chosen = "coalesce((SELECT access.loudness_analysis_key FROM local_access_files access WHERE access.media_uri=tracks.media_uri ORDER BY access.local_access_file_key LIMIT 1),source_loudness_analysis_key)";
    let albums = sqlx::query_scalar::<_, Option<crate::AlbumKey>>(sqlx::AssertSqlSafe(format!(
        "UPDATE tracks SET loudness_analysis_key={chosen} WHERE media_uri IN (SELECT value FROM json_each(?1)) AND loudness_analysis_key IS NOT {chosen} RETURNING album_key"
    ))).bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
    let albums = albums
        .into_iter()
        .flatten()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    crate::loudness::invalidate_album_audio(connection, &albums, false).await?;
    Ok(())
}

fn validate_local_access(access: &LocalAccessWrite) -> LibraryResult<()> {
    if access.path.is_empty()
        || access.root.is_empty()
        || access.media_uri.is_empty()
        || access.access_uri.is_empty()
        || access.size_bytes < 0
        || access.parser_version < 1
    {
        return Err(LibraryError::InvalidRequest(
            "invalid Local access file".to_string(),
        ));
    }
    Ok(())
}

impl Database {}

pub(crate) async fn write_local_locator(
    connection: &mut sqlx::SqliteConnection,
    locator: &LocalLocatorWrite,
) -> LibraryResult<LocalAccessFileKey> {
    crate::catalog::remember_track(
        connection,
        &crate::TrackReference {
            media_uri: locator.media_uri.clone(),
            ..Default::default()
        },
    )
    .await?;
    let source = match &locator.source_id {
        Some(id) => Some(crate::db::write_source_identity(connection, id).await?),
        None => None,
    };
    Ok(sqlx::query_scalar("INSERT INTO main.local_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(media_uri,origin) DO UPDATE SET source_key=excluded.source_key,path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri RETURNING local_access_file_key")
        .bind(source).bind(&locator.media_uri).bind(&locator.origin).bind(&locator.path).bind(&locator.root).bind(&locator.relative_path).bind(&locator.access_uri).fetch_one(connection).await?)
}

impl Database {}

impl crate::Catalog<'_> {}

pub(crate) async fn write_local_access_on(
    connection: &mut sqlx::SqliteConnection,
    source: Option<SourceKey>,
    records: &[&LocalAccessWrite],
) -> LibraryResult<()> {
    for access in records {
        validate_local_access(access)?;
    }
    let source_id = if let Some(source) = source {
        sqlx::query_scalar::<_, String>("SELECT object_id FROM main.sources WHERE source_key=?1")
            .bind(source)
            .fetch_optional(&mut *connection)
            .await?
    } else {
        None
    };
    for access in records {
        sqlx::query("DELETE FROM main.local_access_metadata WHERE access_uri IN (SELECT access_uri FROM main.local_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3) AND NOT EXISTS(SELECT 1 FROM local_locators other WHERE other.access_uri=local_access_metadata.access_uri AND (other.media_uri<>?1 OR other.origin<>?2 OR other.path=?3))").bind(&access.media_uri).bind(access.origin.as_str()).bind(&access.path).execute(&mut *connection).await?;
        sqlx::query(
            "DELETE FROM main.local_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3",
        )
        .bind(&access.media_uri)
        .bind(access.origin.as_str())
        .bind(&access.path)
        .execute(&mut *connection)
        .await?;
        write_local_locator(
            &mut *connection,
            &LocalLocatorWrite {
                source_id: source_id.clone(),
                media_uri: access.media_uri.clone(),
                origin: access.origin.as_str().to_owned(),
                path: access.path.clone(),
                root: access.root.clone(),
                relative_path: access.relative_path.clone(),
                access_uri: access.access_uri.clone(),
            },
        )
        .await?;
        sqlx::query("INSERT INTO main.local_access_metadata(access_uri,size_bytes,mtime_ns,
            device_id,inode,parser_version,loudness_analysis_key) VALUES(?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(access_uri) DO UPDATE SET size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,
            device_id=excluded.device_id,inode=excluded.inode,parser_version=excluded.parser_version,
            loudness_analysis_key=excluded.loudness_analysis_key")
            .bind(&access.access_uri).bind(access.size_bytes).bind(access.mtime_ns).bind(access.device_id).bind(access.inode)
            .bind(access.parser_version).bind(access.loudness_analysis_key.as_ref().map(|key|key.as_slice()))
            .execute(&mut *connection).await?;
    }
    refresh_access_audio(
        &mut *connection,
        &records
            .iter()
            .map(|row| row.media_uri.as_str())
            .collect::<Vec<_>>(),
    )
    .await?;
    Ok(())
}
