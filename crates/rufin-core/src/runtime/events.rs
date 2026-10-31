use async_channel::Receiver;

use super::{ReleaseUpdate, SourceEvent};

pub struct ProductReceivers {
    pub source: Receiver<SourceEvent>,
    pub downloads: Receiver<downloads::DownloadEvent>,
    pub release_updates: Receiver<ReleaseUpdate>,
}
