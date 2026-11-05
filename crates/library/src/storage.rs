//! Usage reporting shared by disposable caches.

#[derive(Clone, Copy, Debug, Default)]
pub struct CacheUsage {
    pub entries: u64,
    pub bytes: u64,
}
