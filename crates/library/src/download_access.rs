use sqlx::{Connection, FromRow};

use crate::{
    Database, DownloadAccessFileKey, LibraryError, LibraryResult, ReadCancellation, SourceKey,
};

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
pub(crate) struct DownloadLocatorWrite {
    pub source_id: Option<String>,
    pub media_uri: String,
    pub origin: String,
    pub path: String,
    pub root: String,
    pub relative_path: String,
    pub access_uri: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DownloadAccessWrite {
    pub media_uri: String,
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

impl Database {
    pub async fn downloaded_count(&self, source_id: Option<&str>) -> LibraryResult<usize> {
        let (_permit, mut connection) = self.acquire_general(&ReadCancellation::new()).await?;
        let count = if let Some(source_id) = source_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.download_locators
                 WHERE source_key=(SELECT source_key FROM main.sources WHERE object_id=?1)
                   AND origin='download'",
            )
            .bind(source_id)
            .fetch_one(&mut *connection)
            .await
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM main.download_locators
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

    pub async fn playback_access(&self, media_uri: &str) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(
            "SELECT access_uri FROM download_access_files WHERE media_uri=?1 ORDER BY download_access_file_key LIMIT 1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn write_download_access(
        &self,
        source: Option<SourceKey>,
        records: &[&DownloadAccessWrite],
    ) -> LibraryResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        write_download_access_on(&mut transaction, source, records).await?;
        transaction.commit().await?;
        Ok(())
    }
}

async fn refresh_access_audio(
    connection: &mut sqlx::SqliteConnection,
    uris: &[&str],
) -> LibraryResult<()> {
    let chosen = "coalesce((SELECT access.loudness_analysis_key FROM download_access_files access WHERE access.media_uri=tracks.media_uri ORDER BY access.download_access_file_key LIMIT 1),source_loudness_analysis_key)";
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

fn validate_local_access(access: &DownloadAccessWrite) -> LibraryResult<()> {
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

pub(crate) async fn write_download_locator(
    connection: &mut sqlx::SqliteConnection,
    locator: &DownloadLocatorWrite,
) -> LibraryResult<DownloadAccessFileKey> {
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
    Ok(sqlx::query_scalar("INSERT INTO main.download_locators(source_key,media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(media_uri,origin) DO UPDATE SET source_key=excluded.source_key,path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri RETURNING download_access_file_key")
        .bind(source).bind(&locator.media_uri).bind(&locator.origin).bind(&locator.path).bind(&locator.root).bind(&locator.relative_path).bind(&locator.access_uri).fetch_one(connection).await?)
}

pub(crate) async fn write_download_access_on(
    connection: &mut sqlx::SqliteConnection,
    source: Option<SourceKey>,
    records: &[&DownloadAccessWrite],
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
        sqlx::query("DELETE FROM main.download_access_metadata WHERE access_uri IN (SELECT access_uri FROM main.download_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3) AND NOT EXISTS(SELECT 1 FROM download_locators other WHERE other.access_uri=download_access_metadata.access_uri AND (other.media_uri<>?1 OR other.origin<>?2 OR other.path=?3))").bind(&access.media_uri).bind("download").bind(&access.path).execute(&mut *connection).await?;
        sqlx::query(
            "DELETE FROM main.download_locators WHERE media_uri=?1 AND origin=?2 AND path<>?3",
        )
        .bind(&access.media_uri)
        .bind("download")
        .bind(&access.path)
        .execute(&mut *connection)
        .await?;
        write_download_locator(
            &mut *connection,
            &DownloadLocatorWrite {
                source_id: source_id.clone(),
                media_uri: access.media_uri.clone(),
                origin: "download".to_owned(),
                path: access.path.clone(),
                root: access.root.clone(),
                relative_path: access.relative_path.clone(),
                access_uri: access.access_uri.clone(),
            },
        )
        .await?;
        sqlx::query("INSERT INTO main.download_access_metadata(access_uri,size_bytes,mtime_ns,
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
