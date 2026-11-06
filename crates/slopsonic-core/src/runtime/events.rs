use async_channel::Receiver;

use super::SourceEvent;

pub struct ProductReceivers {
    pub source: Receiver<SourceEvent>,
    pub downloads: Receiver<downloads::DownloadEvent>,
}
