//! Commands, updates, and startup data exchanged between application clients and Rufin.
//!
//! `app` constructs these handles; the crates behind them implement the behavior.

use std::sync::Arc;

mod diagnostics;
mod events;
mod inputs;
pub mod scrobbling;
pub mod source;
mod storage;
pub use diagnostics::DiagnosticsHandle;
pub use downloads::{DownloadEvent, DownloadQueueItem, DownloadQueueState, DownloadSubject};
pub use events::ProductReceivers;
pub use inputs::{
    CatalogChange, CatalogPublication, CatalogRevision, FavoriteSettlement, RuntimeInputs,
    SelectedLibrary, SourceEvent, SourceNotice, SourceNoticeKind,
};
pub use scrobbling::{
    LastFmPreferences, LibreFmPreferences, ListenBrainzPreferences, ScrobblingConnection,
    ScrobblingConnectionEvent, ScrobblingHandle, ScrobblingPreferences,
};
pub use source::SourceHandle;
pub use storage::{CacheKind, CacheSummary, CacheUsage, StorageSummary};

#[derive(Clone)]
pub struct ProductHandles {
    pub settings: crate::SettingsHandle,
    pub library: Arc<library::Database>,
    pub runtime: tokio::runtime::Handle,
    pub source: SourceHandle,
    pub downloads: downloads::Downloads,
    pub playback: std::sync::Arc<crate::playback::PlaybackOwner>,
    pub artwork: artwork::Artwork,
    pub scrobbling: ScrobblingHandle,
}
