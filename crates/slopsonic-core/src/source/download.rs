use async_channel::Receiver;

use super::SourceOwner;
use crate::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};
use sources::SourceConfiguration;
use std::sync::{Arc, atomic::Ordering};

impl SourceOwner {
    pub(crate) fn request_asset_downloads(&self) {
        self.shared.asset_download_requested.notify_one();
    }

    pub(crate) fn start_asset_downloader(&self) {
        let weak = Arc::downgrade(&self.shared);
        let requested = Arc::clone(&self.shared.asset_download_requested);
        let task = self.shared.runtime.spawn(async move {
            loop {
                requested.notified().await;
                let Some(shared) = weak.upgrade() else { break };
                if !shared.started.load(Ordering::Acquire) {
                    break;
                }
                SourceOwner { shared }.download_assets().await;
            }
        });
        *self
            .shared
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(task);
        self.request_asset_downloads();
    }

    fn asset_download_enabled(&self) -> bool {
        self.shared.settings.download_artwork_automatically()
    }

    async fn download_assets(&self) {
        if self.asset_download_enabled() {
            if let Some(configured) = self.shared.settings.load().sources.configured {
                if let Err(error) = self.download_source_assets(&configured.configuration).await {
                    tracing::warn!(source = %configured.configuration.name, %error, "could not download source assets");
                }
            }
        }
        self.publish_asset_operation(SourceOperation::Idle);
    }

    async fn download_source_assets(
        &self,
        configuration: &SourceConfiguration,
    ) -> Result<(), String> {
        let source_id = &configuration.source_id;
        let database = &self.shared.database;
        let Some(source_key) = database
            .source_identity_key(source_id)
            .await
            .map_err(super::string_error)?
        else {
            return Ok(());
        };
        let cancel_read = library::ReadCancellation::new();
        let mut reported_at = std::time::Instant::now();
        let mut report = |completed| {
            if completed == 0 || reported_at.elapsed() >= std::time::Duration::from_millis(100) {
                self.publish_asset_operation(SourceOperation::Downloading {
                    source_id: source_id.clone(),
                    progress: SourceProgress {
                        stage: SourceProgressStage::Artwork,
                        completed,
                        total: None,
                    },
                });
                reported_at = std::time::Instant::now();
            }
        };
        let mut completed = 0;
        let mut after = None;
        loop {
            let page = database
                .source_artwork_page(source_key, after.as_deref(), 128, &cancel_read)
                .await
                .map_err(super::string_error)?;
            if page.is_empty() {
                break;
            }
            if completed == 0 {
                report(0);
            }
            after = page.last().cloned();
            for binding in page {
                if !self.asset_download_enabled() {
                    return Ok(());
                }
                self.shared
                    .artwork
                    .cache_source(&binding)
                    .await
                    .map_err(super::string_error)?;
                completed += 1;
                report(completed);
            }
        }
        Ok(())
    }

    fn publish_asset_operation(&self, operation: SourceOperation) {
        let finished = !matches!(operation, SourceOperation::Downloading { .. });
        if self.shared.operation.borrow().blocks_library()
            || (finished
                && !matches!(
                    *self.shared.operation.borrow(),
                    SourceOperation::Downloading { .. }
                ))
        {
            return;
        }
        self.shared.operation.send_replace(operation.clone());
        let _ = self
            .shared
            .outputs
            .events
            .try_send(crate::runtime::SourceEvent::Operation(operation));
    }
}

impl SourceOwner {
    pub fn download_selection(
        &self,
        selection: library::MemberSelection,
        context: String,
        title: Option<String>,
    ) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let uris = database
                .selected_track_uris(&selection, None, &library::ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?;
            let subject =
                downloads::DownloadSubject::for_media_uris(&context, title.as_deref(), &uris);
            owner.download_media(subject, uris);
            Ok(())
        })
    }

    pub fn remove_download_media(&self, media_uris: Vec<String>) {
        self.shared.downloads.remove(media_uris, true);
    }
}
