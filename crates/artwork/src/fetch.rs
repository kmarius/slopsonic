use std::sync::{Arc, Mutex};

use sources::{ImageSize, SourceError, SourceImageRequest};
use tokio::runtime::Handle;

use crate::SourceResolver;
use crate::selection::Candidate;

#[derive(Debug)]
pub(crate) enum FetchOutcome {
    Ready(Vec<u8>),
    Missing,
}

#[derive(Clone)]
pub(crate) struct FetchContext {
    source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>,
    database: Arc<Mutex<Option<Arc<library::Database>>>>,
}

impl FetchContext {
    pub(crate) fn new(source_resolver: Arc<Mutex<Option<Arc<SourceResolver>>>>) -> Self {
        Self {
            source_resolver,
            database: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn install_database(&self, database: Arc<library::Database>) {
        *self
            .database
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(database);
    }

    pub(crate) fn database(&self) -> Option<Arc<library::Database>> {
        self.database
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn source(&self, source_id: &sources::SourceId) -> Result<Arc<sources::Source>, String> {
        let resolver = self
            .source_resolver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| "artwork source is unavailable".to_string())?;
        resolver(source_id).ok_or_else(|| "artwork source is unavailable".to_string())
    }

    pub(crate) fn fetch(
        &self,
        runtime: &Handle,
        candidate: &Candidate,
        size: ImageSize,
    ) -> Result<FetchOutcome, String> {
        match &candidate.image {
            library::ArtworkImage::Playlist { .. } => {
                let database = self
                    .database()
                    .ok_or_else(|| "Playlist artwork storage is unavailable".to_string())?;
                runtime
                    .block_on(database.playlist_image(&candidate.image))
                    .map(|bytes| {
                        bytes
                            .map(FetchOutcome::Ready)
                            .unwrap_or(FetchOutcome::Missing)
                    })
                    .map_err(|error| error.to_string())
            }
            library::ArtworkImage::Native {
                source_id,
                item_id,
                tag,
            } => {
                let source = self.source(source_id)?;
                runtime
                    .block_on(source.image(SourceImageRequest::Native {
                        image_ref: sources::NativeImageRef::new(item_id, tag.clone()),
                        size,
                    }))
                    .map(|image| {
                        if image.bytes.is_empty() {
                            FetchOutcome::Missing
                        } else {
                            FetchOutcome::Ready(image.bytes)
                        }
                    })
                    .or_else(source_result)
            }
            library::ArtworkImage::Local { .. } => Ok(FetchOutcome::Missing),
        }
    }
}

fn source_result(error: SourceError) -> Result<FetchOutcome, String> {
    match error {
        SourceError::NotFound => Ok(FetchOutcome::Missing),
        error => Err(error.to_string()),
    }
}
