use sqlx::{Connection, FromRow};

use crate::{Database, DownloadAccessFileKey, LibraryError, LibraryResult, ReadCancellation};

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
}

impl Database {
    pub async fn downloaded_count(&self) -> LibraryResult<usize> {
        let mut connection = self.acquire_general(&ReadCancellation::new()).await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM main.download_locators WHERE origin='download'",
        )
        .fetch_one(&mut *connection)
        .await?;
        Ok(usize::try_from(count).unwrap_or_default())
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
        let mut connection = self.acquire_playable().await?;
        Ok(sqlx::query_scalar(
            "SELECT access_uri FROM download_access_files WHERE media_uri=?1 ORDER BY download_access_file_key LIMIT 1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }

    pub async fn write_download_access(
        &self,
        records: &[&DownloadAccessWrite],
    ) -> LibraryResult<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        write_download_access_on(&mut transaction, records).await?;
        transaction.commit().await?;
        Ok(())
    }
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
    Ok(sqlx::query_scalar("INSERT INTO main.download_locators(media_uri,origin,path,root,relative_path,access_uri) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(media_uri,origin) DO UPDATE SET path=excluded.path,root=excluded.root,relative_path=excluded.relative_path,access_uri=excluded.access_uri RETURNING download_access_file_key")
        .bind(&locator.media_uri).bind(&locator.origin).bind(&locator.path).bind(&locator.root).bind(&locator.relative_path).bind(&locator.access_uri).fetch_one(connection).await?)
}

pub(crate) async fn write_download_access_on(
    connection: &mut sqlx::SqliteConnection,
    records: &[&DownloadAccessWrite],
) -> LibraryResult<()> {
    for access in records {
        validate_local_access(access)?;
    }
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
            device_id,inode,parser_version) VALUES(?1,?2,?3,?4,?5,?6)
            ON CONFLICT(access_uri) DO UPDATE SET size_bytes=excluded.size_bytes,mtime_ns=excluded.mtime_ns,
            device_id=excluded.device_id,inode=excluded.inode,parser_version=excluded.parser_version")
            .bind(&access.access_uri).bind(access.size_bytes).bind(access.mtime_ns).bind(access.device_id).bind(access.inode)
            .bind(access.parser_version)
            .execute(&mut *connection).await?;
    }
    Ok(())
}
