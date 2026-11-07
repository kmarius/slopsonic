use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum SourceMetadataError {
    #[error("metadata editing is unavailable")]
    Unavailable,
    #[error("metadata was saved but its source refresh failed: {0}")]
    SavedRefreshFailed(String),
    #[error("some changes were saved: {reason}")]
    PartiallySaved { reason: String },
    #[error("metadata failed: {0}")]
    Write(String),
}

impl From<crate::SourceError> for SourceMetadataError {
    fn from(error: crate::SourceError) -> Self {
        Self::Write(error.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageBytes {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArtworkChange {
    Replace(std::sync::Arc<ImageBytes>),
    Remove,
}

pub fn finish_metadata_save(
    result: Result<(), SourceMetadataError>,
    refresh: Result<(), SourceMetadataError>,
    saved: bool,
) -> Result<(), SourceMetadataError> {
    let message = match result {
        Ok(()) => {
            return refresh.map_err(|error| match error {
                SourceMetadataError::SavedRefreshFailed(_)
                | SourceMetadataError::PartiallySaved { .. } => error,
                error => SourceMetadataError::SavedRefreshFailed(error.to_string()),
            });
        }
        Err(SourceMetadataError::PartiallySaved { reason }) => reason,
        Err(error) if saved => error.to_string(),
        Err(error) => return Err(error),
    };
    let message = match refresh {
        Ok(()) => message,
        Err(error) => format!("{message}. Source refresh also failed: {error}"),
    };
    Err(SourceMetadataError::PartiallySaved { reason: message })
}
