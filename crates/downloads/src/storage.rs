use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct DownloadAudio {
    pub root: Option<PathBuf>,
    pub path: PathBuf,
}

pub(super) struct DownloadFileMetadata {
    pub size: Option<u64>,
}

impl DownloadAudio {
    pub fn access_uri(&self) -> Result<String, String> {
        reqwest::Url::from_file_path(&self.path)
            .map(String::from)
            .map_err(|()| "Download path is not absolute".into())
    }

    pub fn metadata(&self) -> Result<Option<DownloadFileMetadata>, String> {
        match std::fs::metadata(&self.path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(DownloadFileMetadata {
                size: Some(metadata.len()),
            })),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub async fn inspect(&self) -> Result<Option<DownloadFileMetadata>, String> {
        let file = self.clone();
        tokio::task::spawn_blocking(move || file.metadata())
            .await
            .map_err(|error| error.to_string())?
    }

    pub fn projection(&self, private_root: &Path) -> Result<(String, String), String> {
        let root = self.root.as_deref().unwrap_or(private_root);
        let relative = self
            .path
            .strip_prefix(root)
            .map_err(|_| "The download is outside its managed storage")?;
        Ok((
            root.to_string_lossy().into_owned(),
            relative.to_string_lossy().into_owned(),
        ))
    }

    /// Errs when a folder holds the copy's path, where the finished file could never land.
    pub async fn check_place(&self) -> Result<(), String> {
        if tokio::fs::symlink_metadata(&self.path)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            return Err(format!(
                "A folder is in the download's place: {}",
                self.path.display()
            ));
        }
        Ok(())
    }

    pub async fn publish(&self, partial: &Path) -> Result<Self, String> {
        tokio::fs::rename(partial, &self.path)
            .await
            .map_err(|error| format!("Could not save the downloaded track: {error}"))?;
        Ok(self.clone())
    }

    pub async fn remove(&self) -> Result<bool, String> {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            // A folder in the copy's place is not the copy, so there is nothing to remove.
            Err(_) if self.path.is_dir() => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
}
