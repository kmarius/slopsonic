use super::ProductHandles;
pub use library::CacheUsage;

#[derive(Clone, Debug)]
pub struct StorageSummary {
    /// The artwork cache, the only cache the app keeps.
    pub artwork: Option<CacheUsage>,
    pub errors: Vec<String>,
}

impl ProductHandles {
    pub async fn storage_summary(&self, clear_artwork: bool) -> StorageSummary {
        let mut errors = Vec::new();
        if clear_artwork && let Err(error) = self.downloads.clear_artwork_cache().await {
            errors.push(error.to_string());
        }
        let artwork = match self.artwork.cache_usage().await {
            Ok(usage) => Some(usage),
            Err(error) => {
                errors.push(error.to_string());
                None
            }
        };
        StorageSummary { artwork, errors }
    }
}
