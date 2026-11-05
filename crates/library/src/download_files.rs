use crate::{Database, LibraryError, LibraryResult};
use sqlx::Connection;

#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct DownloadFile {
    pub media_uri: String,
    pub encoding: String,
    pub revision: String,
    pub path: String,
    pub managed: bool,
    pub source_id: Option<String>,
    pub owners: String,
    pub storage_root: String,
    pub relative_path: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn download_receipt_removal_preserves_original_and_borrowed_access() {
        let root = tempfile::tempdir().unwrap();
        let database = Database::open(root.path().join("library.sqlite"))
            .await
            .unwrap();
        let original = root.path().join("original.flac");
        std::fs::write(&original, b"original").unwrap();
        let uri = url::Url::from_file_path(&original).unwrap().to_string();
        let item = crate::TrackReference::direct(&uri, "Track", "Artist", "Album", 180_000);
        database
            .write_direct_track_metadata(&uri, &item.metadata)
            .await
            .unwrap();
        let copy = root.path().join("download.mp3");
        std::fs::write(&copy, b"download").unwrap();
        let access = crate::DownloadAccessWrite {
            media_uri: item.media_uri.clone(),
            path: copy.to_string_lossy().into_owned(),
            root: root.path().to_string_lossy().into_owned(),
            relative_path: "download.mp3".into(),
            size_bytes: 8,
            mtime_ns: 0,
            device_id: None,
            inode: None,
            parser_version: 1,
            access_uri: url::Url::from_file_path(&copy).unwrap().to_string(),
            loudness_analysis_key: None,
        };
        database
            .write_download_access(None, &[&access])
            .await
            .unwrap();
        let receipt = DownloadFile {
            media_uri: uri.clone(),
            encoding: "mp3".into(),
            revision: "1".into(),
            path: url::Url::from_file_path(&copy).unwrap().to_string(),
            managed: true,
            ..Default::default()
        };
        database.save_download_file(&receipt).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap(),
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
        let alternative = root.path().join("download.opus");
        std::fs::write(&alternative, b"alternative").unwrap();
        let alternate = DownloadFile {
            encoding: media_encoding_key(Some(crate::AudioEncoding {
                codec: crate::AudioCodec::Opus,
                ..crate::AudioEncoding::mp3()
            })),
            path: url::Url::from_file_path(&alternative).unwrap().to_string(),
            completed_size: Some(11),
            storage_root: root.path().to_string_lossy().into_owned(),
            relative_path: "download.opus".into(),
            ..receipt.clone()
        };
        let mut alternate_access = access.clone();
        alternate_access.path = alternative.to_string_lossy().into_owned();
        alternate_access.access_uri = alternate.path.clone();
        alternate_access.relative_path = "download.opus".into();
        alternate_access.size_bytes = 11;
        database
            .commit_download(&alternate, None, &alternate_access)
            .await
            .unwrap();
        database.forget_download_file(&alternate).await.unwrap();
        assert_eq!(
            database.playback_access(&uri).await.unwrap().unwrap(),
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
        let removed = sqlx::query("DELETE FROM download_files WHERE media_uri=?1 AND encoding=?2 AND path=?3 AND revision=?4")
            .bind(&file.media_uri).bind(&file.encoding).bind(&file.path).bind(&file.revision).execute(&mut *transaction).await?.rows_affected()>0;
        if removed {
            let access = sqlx::query("DELETE FROM download_locators WHERE media_uri=?1 AND access_uri=?2 AND origin='download' AND NOT EXISTS(SELECT 1 FROM download_files WHERE media_uri=?1 AND path=?2)")
                .bind(&file.media_uri).bind(&file.path).execute(&mut *transaction).await?.rows_affected()>0;
            if access
                && let Some(next) = sqlx::query_as::<_, DownloadFile>(
                    "SELECT * FROM download_files WHERE media_uri=?1 ORDER BY rowid DESC LIMIT 1",
                )
                .bind(&file.media_uri)
                .fetch_optional(&mut *transaction)
                .await?
            {
                let path = crate::file_media_path(&next.path);
                let root = if next.storage_root.is_empty() {
                    path.as_deref()
                        .and_then(std::path::Path::parent)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|| next.path.clone())
                } else {
                    next.storage_root.clone()
                };
                crate::download_access::write_download_locator(
                    &mut transaction,
                    &crate::download_access::DownloadLocatorWrite {
                        source_id: next.source_id.clone(),
                        media_uri: next.media_uri.clone(),
                        origin: "download".into(),
                        path: path
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_else(|| next.path.clone()),
                        root,
                        relative_path: next.relative_path.clone(),
                        access_uri: next.path.clone(),
                    },
                )
                .await?;
                sqlx::query("INSERT INTO download_access_metadata SELECT ?1,coalesce(?2,size_bytes),0,NULL,NULL,parser_version,loudness_analysis_key FROM download_access_metadata WHERE access_uri=?3 ON CONFLICT(access_uri) DO NOTHING")
                    .bind(&next.path).bind(next.completed_size).bind(&file.path).execute(&mut *transaction).await?;
            }
        }
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

    pub async fn commit_download(
        &self,
        file: &DownloadFile,
        source: Option<crate::SourceKey>,
        access: &crate::DownloadAccessWrite,
    ) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        crate::download_access::write_download_access_on(&mut transaction, source, &[access])
            .await?;
        save_download_on(&mut transaction, file).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn download_files_page(
        &self,
        source: Option<&str>,
        after: &(String, String),
        owner: Option<&str>,
    ) -> LibraryResult<Vec<DownloadFile>> {
        let mut reader = self.acquire_reader().await?;
        Ok(sqlx::query_as("SELECT * FROM download_files WHERE source_id IS ?1 AND (media_uri,encoding)>(?2,?3) AND (?4 IS NULL OR EXISTS(SELECT 1 FROM json_each(owners) WHERE value=json_extract(?4,'$'))) ORDER BY media_uri,encoding LIMIT 128")
            .bind(source).bind(&after.0).bind(&after.1).bind(owner).fetch_all(&mut *reader).await?)
    }
}

async fn save_download_on(
    connection: &mut sqlx::SqliteConnection,
    file: &DownloadFile,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO download_files(media_uri,encoding,revision,path,managed,source_id,owners,storage_root,relative_path,completed_size,retired_documents) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(media_uri,encoding) DO UPDATE SET revision=excluded.revision,path=excluded.path,managed=excluded.managed,source_id=coalesce(excluded.source_id,download_files.source_id),owners=excluded.owners,storage_root=excluded.storage_root,relative_path=excluded.relative_path,completed_size=excluded.completed_size,retired_documents=excluded.retired_documents")
        .bind(&file.media_uri).bind(&file.encoding).bind(&file.revision).bind(&file.path).bind(file.managed).bind(&file.source_id)
        .bind(if file.owners.is_empty() { "[]" } else { &file.owners }).bind(&file.storage_root).bind(&file.relative_path).bind(file.completed_size)
        .bind(if file.retired_documents.is_empty() { "[]" } else { &file.retired_documents }).execute(connection).await?;
    Ok(())
}
