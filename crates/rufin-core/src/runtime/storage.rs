use super::ProductHandles;
pub use library::CacheUsage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheKind {
    Loudness,
    Artwork,
}

#[derive(Clone, Debug)]
pub struct CacheSummary {
    pub kind: CacheKind,
    pub usage: Option<CacheUsage>,
}

#[derive(Clone, Debug)]
pub struct StorageSummary {
    pub caches: Vec<CacheSummary>,
    pub total_bytes: Option<u64>,
    pub errors: Vec<String>,
}

impl ProductHandles {
    pub async fn storage_summary(&self, clear: Option<CacheKind>) -> StorageSummary {
        let mut summary = StorageSummary {
            caches: Vec::new(),
            total_bytes: Some(0),
            errors: Vec::new(),
        };
        if let Some(kind) = clear
            && let Err(error) = self.clear_cached_data(kind).await
        {
            summary.errors.push(error);
        }
        for kind in [CacheKind::Loudness, CacheKind::Artwork] {
            let usage = match self.cache_usage(kind).await {
                Ok(usage) => {
                    summary.total_bytes = summary.total_bytes.map(|total| total + usage.bytes);
                    Some(usage)
                }
                Err(error) => {
                    summary.total_bytes = None;
                    summary.errors.push(error);
                    None
                }
            };
            summary.caches.push(CacheSummary { kind, usage });
        }
        summary
    }

    pub fn prepare_storage_reset(
        &self,
        reset: crate::settings::StorageReset,
    ) -> Result<crate::settings::StorageResetRequest, String> {
        self.settings.prepare_storage_reset(reset)
    }

    pub async fn close_storage(&self) -> Result<(), String> {
        self.source.close_storage().await;
        self.downloads.shutdown().await?;
        let settings = self.settings.clone();
        self.runtime
            .spawn_blocking(move || settings.flush())
            .await
            .map_err(|e| e.to_string())??;
        self.library
            .as_ref()
            .clone()
            .close()
            .await
            .map_err(|error| error.to_string())
    }

    async fn cache_usage(&self, kind: CacheKind) -> Result<CacheUsage, String> {
        match kind {
            CacheKind::Loudness => self
                .library
                .loudness_cache_usage()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Artwork => self
                .artwork
                .cache_usage()
                .await
                .map_err(|error| error.to_string()),
        }
    }

    pub async fn clear_cached_data(&self, kind: CacheKind) -> Result<(), String> {
        match kind {
            CacheKind::Loudness => self
                .library
                .clear_loudness_cache()
                .await
                .map_err(|error| error.to_string()),
            CacheKind::Artwork => self
                .downloads
                .clear_artwork_cache()
                .await
                .map_err(|error| error.to_string()),
        }
    }
}
