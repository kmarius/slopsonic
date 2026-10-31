use async_channel::Receiver;

use super::source::DiscoveryUpdate;
use super::{ReleaseUpdate, SourceEvent};

pub struct ProductReceivers {
    pub source: Receiver<SourceEvent>,
    pub source_discovery: Receiver<DiscoveryUpdate>,
    pub downloads: Receiver<downloads::DownloadEvent>,
    pub release_updates: Receiver<ReleaseUpdate>,
}
