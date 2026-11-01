//! Native music providers and file access.
//!
//! Providers own authentication, HTTP or filesystem work, paging, wire
//! translation, and bounded Library updates. Rufin owns source lifecycle,
//! artwork preparation, and publication of committed catalog changes.

#[cfg(test)]
extern crate self as sources;

mod download;
pub use download::{DownloadQuality, ResolvedDownload, downloaded_codec};
mod config;
mod detail_links;
mod operations;
mod policy;
mod source;

mod remote_http;
mod remote_json;
mod subsonic;

pub use config::{
    CredentialInput, CredentialPreset, EditableSource, SourceConfiguration, SourceSettingsChange,
    SourceSetup,
};
pub use library::SourceId;
pub use operations::{
    ArtworkChange, ArtworkEdit, ArtworkEditing, ArtworkStorage, ImageBytes, MetadataChanges,
    MetadataDraft, MetadataEdit, MetadataField, MetadataFieldKind, SourceMetadataError,
    artwork_file, finish_metadata_save,
};
pub use source::*;

pub use subsonic::{SubsonicAuthentication, SubsonicFlavor};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SourceError {
    #[error(transparent)]
    Library(#[from] library::LibraryError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("source authentication failed: {0}")]
    Auth(String),
    #[error("source TLS validation failed: {0}")]
    Tls(String),
    #[error("source network failed: {0}")]
    Network(String),
    #[error("source server failed with status {status}: {message}")]
    Server { status: u16, message: String },
    #[error("source item was not found")]
    NotFound,
    #[error("source request is invalid: {0}")]
    InvalidRequest(&'static str),
    #[error("source operation was cancelled")]
    Cancelled,
    #[error("saved source configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("source failed: {0}")]
    Other(String),
}

pub type SourceResult<T> = Result<T, SourceError>;
