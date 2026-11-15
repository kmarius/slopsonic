use sqlx::FromRow;

use crate::{Database, LibraryResult, ReadCancellation};

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

impl Database {
    pub async fn downloaded_count(&self) -> LibraryResult<usize> {
        let mut connection = self.acquire_general(&ReadCancellation::new()).await?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT count(DISTINCT media_uri) FROM main.download_files",
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
            "SELECT path FROM download_files WHERE media_uri=?1 ORDER BY rowid DESC LIMIT 1",
        )
        .bind(media_uri)
        .fetch_optional(&mut *connection)
        .await?)
    }
}
