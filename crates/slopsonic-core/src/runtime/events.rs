use async_channel::Receiver;

use super::ServerEvent;

pub struct ProductReceivers {
    pub source: Receiver<ServerEvent>,
    pub downloads: Receiver<downloads::DownloadEvent>,
}
