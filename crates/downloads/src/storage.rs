use std::path::{Path, PathBuf};
#[cfg(feature = "bindings")]
uniffi::custom_type!(PathBuf, String, {
    remote,
    lower: |path| path.to_string_lossy().into_owned(),
    try_lift: |path| Ok(PathBuf::from(path)),
});

use serde::{Deserialize, Serialize};

/// Native paths keep their released JSON representation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DownloadDirectory {
    Native(PathBuf),
}

impl DownloadDirectory {
    pub fn native(&self) -> Option<&Path> {
        match self {
            Self::Native(path) => Some(path),
        }
    }
}

impl std::fmt::Display for DownloadDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native(path) => path.display().fmt(formatter),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum DownloadAudio {
    Native {
        root: Option<PathBuf>,
        path: PathBuf,
    },
}

pub(super) struct DownloadFileMetadata {
    pub size: Option<u64>,
    pub mtime_ns: i64,
}

impl DownloadAudio {
    pub fn native(&self) -> Option<&Path> {
        match self {
            Self::Native { path, .. } => Some(path),
        }
    }

    pub fn location(&self) -> Result<String, String> {
        match self {
            Self::Native { path, .. } => Ok(path.to_string_lossy().into_owned()),
        }
    }

    pub fn access_uri(&self) -> Result<String, String> {
        match self {
            Self::Native { path, .. } => reqwest::Url::from_file_path(path)
                .map(String::from)
                .map_err(|()| "Download path is not absolute".into()),
        }
    }

    pub fn metadata(&self) -> Result<Option<DownloadFileMetadata>, String> {
        match self {
            Self::Native { path, .. } => match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => Ok(Some(DownloadFileMetadata {
                    size: Some(metadata.len()),
                    mtime_ns: metadata
                        .modified()
                        .ok()
                        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                        .map_or(0, |value| {
                            i64::try_from(value.as_nanos()).unwrap_or(i64::MAX)
                        }),
                })),
                Ok(_) => Ok(None),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error.to_string()),
            },
        }
    }

    pub async fn inspect(&self) -> Result<Option<DownloadFileMetadata>, String> {
        let file = self.clone();
        tokio::task::spawn_blocking(move || file.metadata())
            .await
            .map_err(|error| error.to_string())?
    }

    pub fn projection(&self, private_root: &Path) -> Result<(String, String), String> {
        match self {
            Self::Native { root, path } => {
                let root = root.as_deref().unwrap_or(private_root);
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| "The download is outside its managed storage")?;
                Ok((
                    root.to_string_lossy().into_owned(),
                    relative.to_string_lossy().into_owned(),
                ))
            }
        }
    }

    pub async fn publish(&self, partial: &Path) -> Result<Self, String> {
        match self {
            Self::Native { path, .. } => {
                tokio::fs::rename(partial, path)
                    .await
                    .map_err(|error| format!("Could not save the downloaded track: {error}"))?;
                Ok(self.clone())
            }
        }
    }

    pub async fn remove(&self) -> Result<bool, String> {
        match self {
            Self::Native { path, .. } => match tokio::fs::remove_file(path).await {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error.to_string()),
            },
        }
    }
}
