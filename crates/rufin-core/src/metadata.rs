//! Media details and artwork preparation.
mod details;
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
pub use details::{
    MediaDetailField, MediaDetailSection, MediaDetailText, MediaDetails, MediaDetailsItem,
    media_details,
};
pub use metadata_lookup::{ArtworkQuery, ArtworkResult};
use std::sync::Arc;
pub const MAX_ARTWORK_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MetadataItemId {
    Track(String),
    Album(String),
    Artist(String),
}

impl MetadataItemId {
    pub fn media_uri(&self) -> &str {
        match self {
            Self::Track(uri) | Self::Album(uri) | Self::Artist(uri) => uri,
        }
    }
}

pub fn search_artwork(
    owner: &SourceOwner,
    query: ArtworkQuery,
) -> Receiver<Result<Vec<ArtworkResult>, String>> {
    owner.reply(move |owner, _| async move {
        if !owner
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Ok(Vec::new());
        }
        tokio::task::spawn_blocking(move || metadata_lookup::search_artwork(&query))
            .await
            .map_err(string_error)?
    })
}

pub fn download_artwork(
    owner: &SourceOwner,
    url: String,
) -> Receiver<Result<Arc<sources::ImageBytes>, String>> {
    owner.reply(move |owner, _| async move {
        if !owner
            .shared
            .settings
            .load()
            .ui
            .allows_external_metadata_lookup()
        {
            return Err("External metadata lookup is disabled".into());
        }
        tokio::task::spawn_blocking(move || {
            metadata_lookup::download_artwork(&url).and_then(checked_artwork)
        })
        .await
        .map_err(string_error)?
    })
}

pub fn prepare_artwork(
    owner: &SourceOwner,
    bytes: Vec<u8>,
) -> Receiver<Result<Arc<sources::ImageBytes>, String>> {
    owner.reply(move |_, _| async move {
        tokio::task::spawn_blocking(move || checked_artwork(bytes))
            .await
            .map_err(string_error)?
    })
}

/// Read the original artwork for editing.
pub fn current_artwork(
    owner: &SourceOwner,
    binding: Vec<u8>,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, _| async move {
        let stored = owner.shared.settings.load();
        let request =
            artwork::ArtworkRequest::original(artwork::ArtworkBinding::opaque(&binding), 512)
                .with_external(artwork::ExternalPolicy::new(
                    stored.ui.application.external_metadata_enabled,
                    stored.ui.allows_external_metadata_lookup(),
                    stored.ui.lastfm_api_key,
                ));
        let bytes = owner.shared.artwork.image_bytes(request).await?;
        tokio::task::spawn_blocking(move || bytes.map(checked_artwork).transpose())
            .await
            .map_err(string_error)?
    })
}

fn checked_artwork(bytes: Vec<u8>) -> Result<Arc<sources::ImageBytes>, String> {
    if bytes.len() > MAX_ARTWORK_BYTES {
        return Err("Artwork exceeds 32 MiB".into());
    }
    artwork::decode_rgba(&bytes, 512).map_err(string_error)?;
    let content_type = artwork::image_mime(&bytes).map(str::to_owned);
    Ok(Arc::new(sources::ImageBytes {
        bytes,
        content_type,
    }))
}
