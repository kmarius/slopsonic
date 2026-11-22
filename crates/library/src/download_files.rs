use crate::{Database, LibraryError, LibraryResult};
use sqlx::Connection;

#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct DownloadFile {
    pub media_uri: String,
    pub encoding: String,
    pub revision: String,
    pub path: String,
    pub owners: String,
    pub storage_root: String,
    pub completed_size: Option<i64>,
    pub retired_documents: String,
}

pub fn media_encoding_key(encoding: Option<crate::AudioEncoding>) -> String {
    match encoding {
        None => "original".into(),
        Some(encoding) if encoding == crate::AudioEncoding::mp3() => "mp3".into(),
        Some(encoding) => {
            serde_json::to_string(&encoding).expect("an audio encoding can be stored")
        }
    }
}

pub fn media_encoding_codec(key: &str) -> Option<crate::AudioCodec> {
    match key {
        "original" => None,
        "mp3" => Some(crate::AudioCodec::Mp3),
        key => serde_json::from_str::<crate::AudioEncoding>(key)
            .ok()
            .map(|encoding| encoding.codec),
    }
}

impl Database {
    pub async fn download_files(&self, uri: &str) -> LibraryResult<Vec<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(
            sqlx::query_as("SELECT * FROM download_files WHERE media_uri=?1")
                .bind(uri)
                .fetch_all(&mut *reader)
                .await?,
        )
    }

    pub async fn forget_download_file(&self, file: &DownloadFile) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        sqlx::query("DELETE FROM download_files WHERE media_uri=?1 AND encoding=?2 AND path=?3 AND revision=?4")
            .bind(&file.media_uri).bind(&file.encoding).bind(&file.path).bind(&file.revision).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn download_file(
        &self,
        uri: &str,
        encoding: &str,
    ) -> LibraryResult<Option<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(
            sqlx::query_as("SELECT * FROM download_files WHERE media_uri=?1 AND encoding=?2")
                .bind(uri)
                .bind(encoding)
                .fetch_optional(&mut *reader)
                .await?,
        )
    }

    pub async fn download_file_shared(&self, file: &DownloadFile) -> LibraryResult<bool> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM download_files WHERE path=?1 AND (media_uri,encoding,revision)<>(?2,?3,?4))")
            .bind(&file.path).bind(&file.media_uri).bind(&file.encoding).bind(&file.revision)
            .fetch_one(&mut *reader).await?)
    }

    pub async fn save_download_file(&self, file: &DownloadFile) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        save_download_on(
            writer.as_mut().ok_or(LibraryError::WriterUnavailable)?,
            file,
        )
        .await
    }

    /// Records a finished download; its track row is created when the catalog has none yet.
    pub async fn commit_download(&self, file: &DownloadFile) -> LibraryResult<()> {
        if file.media_uri.is_empty() || file.path.is_empty() || file.storage_root.is_empty() {
            return Err(LibraryError::InvalidRequest(
                "invalid download record".to_string(),
            ));
        }
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        crate::catalog::remember_track(
            &mut transaction,
            &crate::TrackReference {
                media_uri: file.media_uri.clone(),
                ..Default::default()
            },
        )
        .await?;
        save_download_on(&mut transaction, file).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn download_files_page(
        &self,
        after: &(String, String),
        owner: Option<&str>,
        stale: bool,
    ) -> LibraryResult<Vec<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM download_files WHERE (media_uri,encoding)>(?1,?2) AND (?3 IS NULL OR EXISTS(SELECT 1 FROM json_each(owners) WHERE value=json_extract(?3,'$'))) AND (NOT ?4 OR (revision<>'' AND revision<>(SELECT hex(audio_key) FROM tracks WHERE tracks.media_uri=download_files.media_uri))) ORDER BY media_uri,encoding LIMIT 128")
            .bind(&after.0).bind(&after.1).bind(owner).bind(stale).fetch_all(&mut *reader).await?)
    }
}

async fn save_download_on(
    connection: &mut sqlx::SqliteConnection,
    file: &DownloadFile,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO download_files(media_uri,encoding,revision,path,owners,storage_root,completed_size,retired_documents) VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(media_uri,encoding) DO UPDATE SET revision=excluded.revision,path=excluded.path,owners=excluded.owners,storage_root=excluded.storage_root,completed_size=excluded.completed_size,retired_documents=excluded.retired_documents")
        .bind(&file.media_uri).bind(&file.encoding).bind(&file.revision).bind(&file.path)
        .bind(if file.owners.is_empty() { "[]" } else { &file.owners }).bind(&file.storage_root).bind(file.completed_size)
        .bind(if file.retired_documents.is_empty() { "[]" } else { &file.retired_documents }).execute(connection).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forgetting_a_download_falls_back_to_the_remaining_one() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let uri = crate::entity_uri("track", "track");
        database.reconcile_server("Source").await.unwrap();
        database
            .catalog(None)
            .await
            .unwrap()
            .apply_catalog(
                &crate::CatalogBatch {
                    tracks: vec![crate::TrackObservation {
                        object_id: "track".into(),
                        metadata: crate::TrackMetadata {
                            title: "Track".into(),
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
        let copy = root.path().join("download.mp3");
        std::fs::write(&copy, b"download").unwrap();
        let receipt = DownloadFile {
            media_uri: uri.clone(),
            encoding: "mp3".into(),
            revision: "1".into(),
            path: url::Url::from_file_path(&copy).unwrap().to_string(),
            storage_root: root.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        database.commit_download(&receipt).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            receipt.path
        );
        assert_eq!(
            {
                let rows = database
                    .tracks_by_uris(std::slice::from_ref(&uri), &crate::ReadCancellation::new())
                    .await
                    .unwrap();
                rows[0].metadata.title.clone()
            },
            "Track"
        );
        let alternative = root.path().join("download.ogg");
        std::fs::write(&alternative, b"alternative").unwrap();
        let alternate = DownloadFile {
            encoding: media_encoding_key(Some(crate::AudioEncoding {
                codec: crate::AudioCodec::Opus,
                ..crate::AudioEncoding::mp3()
            })),
            path: url::Url::from_file_path(&alternative).unwrap().to_string(),
            completed_size: Some(11),
            storage_root: root.path().to_string_lossy().into_owned(),
            ..receipt.clone()
        };
        database.commit_download(&alternate).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            alternate.path
        );
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().1,
            Some(crate::AudioCodec::Opus)
        );
        database.forget_download_file(&alternate).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap().0,
            receipt.path
        );
        database.forget_download_file(&receipt).await.unwrap();
        assert!(database.playback_access(&uri).await.unwrap().is_none());
        assert_eq!(
            {
                let rows = database
                    .tracks_by_uris(std::slice::from_ref(&uri), &crate::ReadCancellation::new())
                    .await
                    .unwrap();
                rows[0].metadata.title.clone()
            },
            "Track"
        );
    }
}
