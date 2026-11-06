//! Source configuration, browsing selection, and source work.
mod availability;
mod download;
pub mod folders;
mod pins;
pub mod search;
mod selection;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runtime::source::{
    EditableSource, SourceCacheSummary, SourceOperation, SourceProgress, SourceProgressStage,
    SourceStatus, SourceSummary,
};
use crate::runtime::{
    CatalogChange, CatalogPublication, CatalogRevision, FavoriteSettlement, SelectedLibrary,
    SourceEvent, SourceNotice, SourceNoticeKind,
};
use artwork::Artwork;
use async_channel::{Receiver, Sender};
use downloads::Downloads;
use library::{
    CatalogUpdate, Database, FavoriteTarget, FolderChoice, FolderKey, ReadCancellation, SourceKey,
};
use secrets::SecretStore;
use sources::{
    LiveFolderPage, SelectedFeed, Source, SourceConfiguration, SourceEntityKind, SourceError,
    SourceId, SourceReadProgress, SourceReadStage, SourceSetup,
};
use tracing::{info, warn};

use crate::playback::PlaybackOwner;
use crate::settings::{
    ConfiguredSource, SettingsFile, StoredSettings, fresh_source_id, load_provider_secret,
    load_provider_secret_or_none, seal_credential,
};

const SOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub(crate) struct SourceOutputs {
    pub(crate) events: Sender<SourceEvent>,
}

#[derive(Clone)]
pub struct SourceOwner {
    pub(crate) shared: Arc<Shared>,
}

struct CachedSource {
    configuration: SourceConfiguration,
    credential: Option<crate::settings::Credential>,
    source: Arc<Source>,
    refresh: Option<Refresh>,
    observer: Option<(Arc<SelectedFeed>, tokio::task::AbortHandle)>,
}

struct Refresh {
    cancelled: Arc<AtomicBool>,
    progress: SourceProgress,
    visible: bool,
}

impl Drop for CachedSource {
    fn drop(&mut self) {
        self.source.retire();
        if let Some((observer, task)) = &self.observer {
            observer.cancel();
            task.abort();
        }
        if let Some(refresh) = &self.refresh {
            refresh.cancelled.store(true, Ordering::Release);
        }
    }
}

impl CachedSource {
    fn matches(&self, configured: &ConfiguredSource) -> bool {
        same_native_input(&self.configuration, &configured.configuration)
            && self.credential == configured.credential
    }
}

fn same_native_input(left: &SourceConfiguration, right: &SourceConfiguration) -> bool {
    left.source_id == right.source_id
        && left.kind == right.kind
        && left.provider_payload == right.provider_payload
}

pub(crate) struct Shared {
    home_showcase_variation: i64,
    home_explore_variation: std::sync::atomic::AtomicI64,
    pub(crate) artwork: Artwork,
    pub(crate) database: Arc<Database>,
    downloads: Downloads,
    pub(crate) settings: SettingsFile,
    pub(crate) secrets: Arc<dyn SecretStore>,
    clients: Mutex<HashMap<SourceId, CachedSource>>,
    pub(crate) runtime: tokio::runtime::Handle,
    outputs: SourceOutputs,
    operation: tokio::sync::watch::Sender<SourceOperation>,
    catalog_changed: tokio::sync::watch::Sender<CatalogRevision>,
    configuration_changed: tokio::sync::watch::Sender<()>,
    selected: Mutex<Option<SelectedLibrary>>,
    catalog_counts: Mutex<HashMap<SourceId, (usize, usize)>>,
    pub(crate) prepared_albums: tokio::sync::Mutex<Option<crate::browse::PreparedAlbums>>,
    selection_intent: Mutex<Weak<AtomicBool>>,
    connection: Mutex<Option<tokio::task::AbortHandle>>,
    asset_download: Mutex<Option<tokio::task::JoinHandle<()>>>,
    asset_download_requested: Arc<tokio::sync::Notify>,
    pub(crate) lane: tokio::sync::Mutex<()>,
    playback: Mutex<Weak<PlaybackOwner>>,
    maintenance: Arc<tokio::sync::Notify>,
    started: AtomicBool,
}

impl Shared {
    pub(crate) fn cached_source_counts(&self, source: &SourceId) -> Option<(usize, usize)> {
        self.catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(source)
            .copied()
    }

    async fn refresh_source_counts(
        &self,
        source_id: &SourceId,
        source: SourceKey,
    ) -> Result<(usize, usize), String> {
        let counts = self
            .database
            .source_counts(source, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        self.catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(source_id.clone(), counts);
        Ok(counts)
    }

    async fn load_source_counts(&self) -> Result<(), String> {
        let counts = self
            .database
            .all_source_counts()
            .await
            .map_err(string_error)?;
        *self
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = counts;
        Ok(())
    }

    pub(crate) fn source_status(&self, selected: Option<&SelectedLibrary>) -> Option<SourceStatus> {
        source_status(
            &self.settings.load(),
            selected,
            &self
                .catalog_counts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    pub(crate) fn selected(&self) -> Option<SelectedLibrary> {
        self.selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) async fn send(&self, event: SourceEvent) {
        if matches!(
            &event,
            SourceEvent::Configured(_)
                | SourceEvent::Selected { .. }
                | SourceEvent::ReleaseSelected
        ) {
            self.configuration_changed.send_replace(());
        }
        if let SourceEvent::CatalogPublished(publication) = &event
            && publication.change != CatalogChange::Home
        {
            self.catalog_changed.send_modify(|revision| {
                if publication.change == CatalogChange::Activity {
                    revision.activity += 1;
                } else {
                    revision.catalog += 1;
                    revision.change = Some(publication.change.clone());
                }
            });
            if matches!(
                publication.change,
                CatalogChange::Broad | CatalogChange::Album(_)
            ) && let Ok(playback) = self.playback()
            {
                playback.catalog_changed();
            }
        }
        let _ = self.outputs.events.send(event).await;
    }

    fn publish_operation(&self, operation: SourceOperation) {
        let active = self
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if matches!(
            operation,
            SourceOperation::Idle | SourceOperation::Refreshing { .. }
        ) && active.is_some()
            && matches!(
                *self.operation.borrow(),
                SourceOperation::Downloading { .. }
            )
        {
            return;
        }
        self.operation.send_replace(operation.clone());
        let _ = self
            .outputs
            .events
            .try_send(SourceEvent::Operation(operation));
    }

    pub(crate) fn warn_nonfatal(&self, message: &str) {
        warn!(message, "source operation was ignored");
    }

    pub(crate) fn playback(&self) -> Result<Arc<PlaybackOwner>, String> {
        self.playback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
            .ok_or_else(|| "Playback is unavailable".to_string())
    }

    fn begin_selection(&self) -> Arc<AtomicBool> {
        self.cancel_selection();
        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .selection_intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(&cancelled);
        cancelled
    }

    fn selection_is_current(&self, cancelled: &Arc<AtomicBool>) -> bool {
        !cancelled.load(Ordering::Acquire)
            && self
                .selection_intent
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .upgrade()
                .is_some_and(|active| Arc::ptr_eq(&active, cancelled))
    }

    fn cancel_selection(&self) {
        if let Some(cancelled) = self
            .selection_intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
        {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(crate) fn begin_refresh(
        &self,
        source_id: &SourceId,
        replace: bool,
        visible: bool,
    ) -> Option<Arc<AtomicBool>> {
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        let cached = clients.get_mut(source_id)?;
        if let Some(previous) = &cached.refresh {
            if !replace && !previous.cancelled.load(Ordering::Acquire) {
                return None;
            }
            previous.cancelled.store(true, Ordering::Release);
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        cached.refresh = Some(Refresh {
            cancelled: cancelled.clone(),
            progress: initial_progress(),
            visible,
        });
        Some(cancelled)
    }

    fn refresh_is_current(&self, source_id: &SourceId, token: &Arc<AtomicBool>) -> bool {
        !token.load(Ordering::Acquire)
            && self
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(source_id)
                .and_then(|cached| cached.refresh.as_ref())
                .is_some_and(|refresh| Arc::ptr_eq(&refresh.cancelled, token))
    }

    fn is_refreshing(&self, source_id: &SourceId) -> bool {
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .and_then(|cached| cached.refresh.as_ref())
            .is_some_and(|refresh| !refresh.cancelled.load(Ordering::Acquire))
    }

    pub(crate) fn present_refresh(&self, source_id: &SourceId) {
        let progress = self
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .and_then(|cached| cached.refresh.as_ref())
            .filter(|refresh| refresh.visible)
            .map(|refresh| refresh.progress.clone());
        if let Some(progress) = progress
            && !self.operation.borrow().blocks_library()
        {
            self.publish_operation(SourceOperation::Refreshing {
                source_id: source_id.clone(),
                progress,
            });
        }
    }

    pub(crate) fn refresh_progress(
        &self,
        source_id: &SourceId,
        token: &Arc<AtomicBool>,
        value: SourceReadProgress,
    ) {
        let progress = source_progress(value);
        {
            let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
            let Some(refresh) = clients
                .get_mut(source_id)
                .and_then(|cached| cached.refresh.as_mut())
            else {
                return;
            };
            if !Arc::ptr_eq(&refresh.cancelled, token) || token.load(Ordering::Acquire) {
                return;
            }
            if refresh.progress == progress {
                return;
            }
            refresh.progress = progress.clone();
            if !refresh.visible {
                return;
            }
        }
        if matches!(&*self.operation.borrow(), SourceOperation::Refreshing { source_id: current, .. } if current == source_id)
        {
            self.publish_operation(SourceOperation::Refreshing {
                source_id: source_id.clone(),
                progress,
            });
        }
    }
}

impl SourceOwner {
    pub(crate) fn open_dormant(
        artwork: Artwork,
        database: Arc<Database>,
        downloads: Downloads,
        settings: SettingsFile,
        secrets: Arc<dyn SecretStore>,
        runtime: tokio::runtime::Handle,
        outputs: SourceOutputs,
    ) -> Arc<Self> {
        let stored = settings.load();
        let operation =
            stored
                .sources
                .configured
                .as_ref()
                .map_or(SourceOperation::Idle, |configured| {
                    SourceOperation::Switching {
                        target: configured.configuration.source_id.clone(),
                        progress: initial_progress(),
                    }
                });
        let home_variation = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos() as i64);
        let shared = Arc::new(Shared {
            catalog_changed: tokio::sync::watch::channel(CatalogRevision::default()).0,
            configuration_changed: tokio::sync::watch::channel(()).0,
            home_showcase_variation: home_variation,
            home_explore_variation: std::sync::atomic::AtomicI64::new(home_variation),
            artwork,
            database,
            downloads,
            settings,
            secrets,
            clients: Mutex::new(HashMap::new()),
            runtime,
            outputs,
            operation: tokio::sync::watch::channel(operation).0,
            selected: Mutex::new(None),
            catalog_counts: Mutex::new(HashMap::new()),
            prepared_albums: tokio::sync::Mutex::new(None),
            selection_intent: Mutex::new(Weak::new()),
            connection: Mutex::new(None),
            asset_download: Mutex::new(None),
            asset_download_requested: Arc::new(tokio::sync::Notify::new()),
            lane: tokio::sync::Mutex::new(()),
            playback: Mutex::new(Weak::new()),
            maintenance: Arc::new(tokio::sync::Notify::new()),
            started: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&shared);
        shared.database.observe_catalog(move |source, update| {
            let shared = weak.upgrade();
            async move {
                if let Some(shared) = shared {
                    if let Some((id, key)) = source {
                        SourceOwner { shared }
                            .catalog_updated(id, key, update)
                            .await;
                    } else if update.activity {
                        shared
                            .send(SourceEvent::CatalogPublished(CatalogPublication {
                                source_key: None,
                                favorite: None,
                                change: CatalogChange::Activity,
                            }))
                            .await;
                    }
                }
            }
        });
        Arc::new(Self { shared })
    }

    pub(crate) fn attach_playback(&self, playback: &Arc<PlaybackOwner>) {
        *self
            .shared
            .playback
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(playback);
    }

    pub(crate) fn start(&self) -> Result<(), String> {
        if self.shared.started.swap(true, Ordering::AcqRel) {
            return Err("the source owner is already running".to_string());
        }
        self.start_asset_downloader();
        let stored = self.shared.settings.load();
        let keep = stored
            .sources
            .configured
            .as_ref()
            .map(|configured| configured.configuration.source_id.clone());
        if let Some(source_id) = keep {
            let owner = self.clone();
            self.shared.runtime.spawn(async move {
                let _ = owner.select_source(source_id).await;
            });
        }
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if let Err(error) = owner.shared.load_source_counts().await {
                owner.shared.warn_nonfatal(&error);
            }
            owner
                .shared
                .send(SourceEvent::Configured(owner.source_status()))
                .await;
        });
        let weak = Arc::downgrade(&self.shared);
        let maintenance = Arc::clone(&self.shared.maintenance);
        self.shared.runtime.spawn(async move {
            let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {},
                    _ = maintenance.notified() => {},
                };
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                let owner = SourceOwner { shared };
                owner.deliver_pending_favorites().await;
            }
        });
        Ok(())
    }

    async fn remove_source_resources(&self, source_id: &SourceId) {
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(source_id);
        self.shared
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(source_id);
        self.shared.downloads.clear(source_id.clone(), false);
        if let Err(error) = self.shared.artwork.invalidate_source(source_id) {
            self.shared.warn_nonfatal(&error.to_string());
        }
    }

    pub(crate) fn spawn_serialized<F, Work>(&self, work: F)
    where
        F: FnOnce(SourceOwner) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        self.shared.runtime.spawn(async move {
            let lane_owner = Arc::clone(&shared);
            let _lane = lane_owner.lane.lock().await;
            work(SourceOwner { shared }).await;
        });
    }
}

impl SourceOwner {
    pub fn home_variations(&self) -> (i64, i64) {
        (
            self.shared.home_showcase_variation,
            self.shared.home_explore_variation.load(Ordering::Relaxed),
        )
    }
}

impl SourceOwner {
    pub fn prepare_collection(&self, media_uri: String) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let source = owner.media_client(&media_uri).await?;
            let result = source.prepare_collection(&database, &media_uri).await;
            result.map_err(string_error)
        })
    }
}

impl SourceOwner {
    pub fn source_status(&self) -> Option<SourceStatus> {
        self.shared.source_status(self.shared.selected().as_ref())
    }
}

impl SourceOwner {
    pub fn operation(&self) -> tokio::sync::watch::Receiver<SourceOperation> {
        self.shared.operation.subscribe()
    }

    pub fn catalog_changes(&self) -> tokio::sync::watch::Receiver<CatalogRevision> {
        self.shared.catalog_changed.subscribe()
    }

    pub fn configuration_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.shared.configuration_changed.subscribe()
    }

    pub fn cached_source_counts(&self, source: &SourceId) -> Option<(usize, usize)> {
        self.shared.cached_source_counts(source)
    }

    /// Browse a configured source without changing the selected library.
    pub async fn browse_folder(
        &self,
        source_id: SourceId,
        folder_object_id: Option<String>,
        music_folder_object_id: Option<String>,
    ) -> Result<LiveFolderPage, String> {
        self.open_client(source_id)
            .await?
            .browse_folder(
                &self.shared.database,
                folder_object_id.as_deref(),
                music_folder_object_id.as_deref(),
            )
            .await
            .map_err(string_error)
    }

    pub fn selected_source_summary(&self) -> Option<(SourceId, String)> {
        let source_id = self
            .shared
            .selected
            .lock()
            .unwrap()
            .as_ref()?
            .source_id
            .clone();
        let name = self.shared.settings.source_name(&source_id);
        Some((source_id, name))
    }

    pub fn selected_library(&self) -> Option<SelectedLibrary> {
        self.shared.selected()
    }
}

impl SourceOwner {
    pub fn configure_source(
        &self,
        input: SourceSetup,
    ) -> Receiver<Result<SelectedLibrary, String>> {
        let (sender, receiver) = async_channel::bounded(1);
        if self.shared.settings.load().sources.configured.is_some() {
            let _ = sender.try_send(Err("A server is already configured".to_string()));
            return receiver;
        }
        let cancelled = self.shared.begin_selection();
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if cancelled.load(Ordering::Acquire) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            owner
                .publish_operation(SourceOperation::Adding {
                    progress: initial_progress(),
                })
                .await;
            let mut persisted_source_id = None;
            let result = async {
                let connected = Source::connect(fresh_source_id()?, input)
                    .await
                    .map_err(string_error)?;
                let (configuration, source, credential) = connected.into_parts();
                if !owner.shared.selection_is_current(&cancelled) {
                    return Err(SourceError::Cancelled.to_string());
                }
                let configured = ConfiguredSource {
                    configuration: configuration.clone(),
                    credential: None,
                    music_folder_id: None,
                };
                let configured = owner.persist_connected_source(&configured, credential)?;
                persisted_source_id = Some(configuration.source_id.clone());
                let source_state = owner
                    .shared
                    .database
                    .reconcile_source(
                        &configuration.source_id,
                        configuration.kind.source_id(),
                        &configuration.name,
                    )
                    .await
                    .map_err(string_error)?;
                let source = Arc::new(source);
                owner
                    .install_selected(
                        configured,
                        Some(Arc::clone(&source)),
                        source_state,
                        Arc::clone(&cancelled),
                    )
                    .await?;
                Ok(source)
            }
            .await;
            if !owner.shared.selection_is_current(&cancelled) {
                let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                return;
            }
            match result {
                Ok(source) => {
                    let Some(selected) = owner.selected_library() else {
                        let _ = sender.try_send(Err(SourceError::Cancelled.to_string()));
                        return;
                    };
                    let _ = sender.try_send(Ok(selected));
                    let _ = owner
                        .refresh_now(source.source_id(), &source, false, true)
                        .await;
                }
                Err(error) => {
                    if let Some(selected) = owner.shared.selected()
                        && let Some(source) = owner.cached_client(&selected.source_id)
                    {
                        owner.start_observer(selected, source, false);
                    }
                    owner
                        .publish_operation(SourceOperation::Failed {
                            source_id: persisted_source_id,
                            message: error.clone(),
                            add_form: true,
                        })
                        .await;
                    let _ = sender.try_send(Err(error));
                }
            }
        });
        receiver
    }

    pub fn update_source(
        &self,
        source_id: SourceId,
        input: SourceSetup,
    ) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let result = owner.edit_configured_source(source_id, input).await;
            if let Err(error) = &result {
                owner.shared.warn_nonfatal(&error);
            }
            let _ = sender.try_send(result);
        });
        receiver
    }
}

impl SourceOwner {
    /// Completes when the source's stored catalog is selected. Connection and refresh
    /// continue in the background, including while credentials are being unlocked.
    pub(crate) async fn select_source(
        &self,
        source_id: SourceId,
    ) -> sources::SourceResult<SourceId> {
        let cancelled = self.shared.begin_selection();
        let owner = self.clone();
        self.shared
            .runtime
            .spawn(async move {
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                owner.shared.publish_operation(SourceOperation::Switching {
                    target: source_id.clone(),
                    progress: initial_progress(),
                });
                let result = owner
                    .select_now(source_id.clone(), Arc::clone(&cancelled), true)
                    .await;
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                if let Err(error) = &result {
                    if let Some(selected) = owner.shared.selected()
                        && let Some(source) = owner.cached_client(&selected.source_id)
                    {
                        owner.start_observer(selected, source, false);
                    }
                    owner.shared.publish_operation(SourceOperation::Failed {
                        source_id: Some(source_id),
                        message: error.clone(),
                        add_form: false,
                    });
                }
                result?;
                owner
                    .selected_library()
                    .map(|selected| selected.source_id)
                    .ok_or_else(|| SourceError::Cancelled.to_string())
            })
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

impl SourceOwner {
    pub async fn refresh_source(
        &self,
        source_id: SourceId,
        automatic: bool,
    ) -> sources::SourceResult<()> {
        let source = self
            .open_client(source_id.clone())
            .await
            .map_err(SourceError::Other)?;
        self.refresh_now(&source_id, &source, automatic, true)
            .await
            .map_err(SourceError::Other)
    }
}

impl SourceOwner {
    pub async fn forget_source(&self, source_id: SourceId) -> sources::SourceResult<()> {
        let selected = self
            .shared
            .selected()
            .is_some_and(|selected| selected.source_id == source_id);
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&source_id);
        if selected {
            self.shared.cancel_selection();
        }
        if matches!(&*self.shared.operation.borrow(), SourceOperation::Refreshing { source_id: active, .. } if active == &source_id)
        {
            self.shared.publish_operation(SourceOperation::Idle);
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let selected = owner
                .shared
                .selected()
                .filter(|selected| selected.source_id == source_id);
            if selected.is_some() {
                owner.release_selected().await;
            }
            let playback = owner.shared.playback().ok();
            let identity = owner
                .shared
                .database
                .source_identity_key(&source_id)
                .await
                .ok()
                .flatten();
            if let Some(source_key) = identity {
                if let Some(playback) = playback.as_ref() {
                    if let Err(error) = playback.forget_source(source_key).await {
                        owner.shared.warn_nonfatal(&error);
                        let _ = sender.try_send(Err(error));
                        return;
                    }
                }
            }
            let removed = owner
                .shared
                .database
                .remove_source(&source_id)
                .await
                .map(|_| ())
                .map_err(string_error);
            if let Err(error) = &removed {
                owner.shared.warn_nonfatal(error);
            }
            if let Err(error) = owner.shared.settings.update(|stored| {
                if stored
                    .sources
                    .configured
                    .as_ref()
                    .is_some_and(|item| item.configuration.source_id == source_id)
                {
                    stored.sources.configured = None;
                }
                Ok(())
            }) {
                owner.shared.warn_nonfatal(&error);
                let _ = sender.try_send(Err(error));
                return;
            }
            owner.remove_source_resources(&source_id).await;
            owner
                .shared
                .send(SourceEvent::Configured(owner.shared.source_status(None)))
                .await;
            let _ = sender.try_send(removed);
        });
        receiver
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

impl SourceOwner {
    pub fn download_media(&self, subject: downloads::DownloadSubject, media_uris: Vec<String>) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let mut source_ids = Vec::new();
            for media_uri in &media_uris {
                let source = match library::source_entity_parts(media_uri) {
                    Some((source, kind, _)) if kind == "track" => Some(source),
                    _ => owner
                        .shared
                        .database
                        .track_source_by_uri(media_uri, &ReadCancellation::new())
                        .await
                        .ok()
                        .flatten(),
                };
                if let Some(source) = source {
                    source_ids.push(source);
                }
            }
            source_ids.sort();
            source_ids.dedup();
            for source_id in source_ids {
                let (source, source_key) = match owner.source_target(&source_id).await {
                    Ok(target) => target,
                    Err(error) => {
                        owner.shared.warn_nonfatal(&error);
                        continue;
                    }
                };
                if let Err(error) = owner
                    .shared
                    .downloads
                    .attach(source_id.clone(), source_key, Some(source), None)
                    .await
                {
                    owner.shared.warn_nonfatal(&error);
                }
            }
            owner.shared.downloads.download(subject, media_uris);
        });
    }

    pub fn set_favorite(&self, target: FavoriteTarget, favorite: bool) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let _ = owner.set_favorite_with_result(target, favorite).await;
        });
    }
}

impl SourceOwner {
    pub async fn set_favorite_with_result(
        &self,
        target: FavoriteTarget,
        favorite: bool,
    ) -> sources::SourceResult<bool> {
        info!(kind = target.kind(), favorite, "Favorite requested");
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let result = match owner.apply_favorite(target, favorite).await {
                Ok(()) => Ok(favorite),
                Err(error) => Err(error),
            };
            let _ = sender.try_send(result);
        });
        receiver
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

impl SourceOwner {
    pub fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            owner.apply_rating(target, rating).await;
        });
    }
}

impl SourceOwner {
    pub fn refresh_home(
        &self,
        source_id: SourceId,
        kind: crate::settings::HomeSectionKind,
    ) -> Receiver<Result<(), String>> {
        use crate::settings::HomeSectionKind;
        if kind == HomeSectionKind::NewlyAdded {
            return self.reply(move |owner, _| async move {
                owner
                    .refresh_source(source_id, false)
                    .await
                    .map_err(string_error)
            });
        }
        self.reply(move |owner, database| async move {
            if kind == HomeSectionKind::Explore {
                owner
                    .shared
                    .home_explore_variation
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(selected) = owner
                    .selected_library()
                    .filter(|selected| selected.source_id == source_id)
                {
                    owner
                        .publish_catalog(&selected, None, CatalogChange::Home)
                        .await;
                }
                return Ok(());
            }
            let source = owner.open_client(source_id.clone()).await?;
            let section = match kind {
                HomeSectionKind::MostPlayed => sources::SourceHomeSection::MostPlayed,
                HomeSectionKind::RecentlyPlayed => sources::SourceHomeSection::RecentlyPlayed,
                HomeSectionKind::RecentlyReleased => sources::SourceHomeSection::RecentlyReleased,
                HomeSectionKind::Explore | HomeSectionKind::NewlyAdded => unreachable!(),
            };
            if let Err(error) = source.refresh_home_section(&database, section).await {
                warn!(%error,"could not refresh Home section");
            }
            Ok(())
        })
    }
}

impl SourceOwner {
    pub(crate) async fn source_target(
        &self,
        source_id: &SourceId,
    ) -> Result<(Arc<Source>, SourceKey), String> {
        let source = self.client(source_id)?;
        let publication = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .ok_or_else(source_access_unavailable)?;
        Ok((source, publication.source))
    }

    async fn favorite_source(&self, target: &FavoriteTarget) -> Option<(SourceId, SourceKey)> {
        let source_id = match library::source_entity_parts(target.media_uri()) {
            Some((source_id, kind, _)) if kind == target.kind() => source_id,
            _ => match target {
                FavoriteTarget::Track(media_uri) => self
                    .shared
                    .database
                    .track_source_by_uri(media_uri, &ReadCancellation::new())
                    .await
                    .ok()
                    .flatten()?,
                FavoriteTarget::Album(_) | FavoriteTarget::Artist(_) => return None,
            },
        };
        let source_key = self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .ok()
            .flatten()?
            .source;
        Some((source_id, source_key))
    }

    async fn apply_favorite(&self, target: FavoriteTarget, favorite: bool) -> Result<(), String> {
        let source = self.favorite_source(&target).await;
        let changed = if source.is_some() {
            self.shared
                .database
                .queue_remote_favorite(&target, favorite, unix_seconds())
                .await
        } else {
            self.shared.database.set_favorite(&target, favorite).await
        }
        .map_err(string_error)?;
        if !changed {
            return Ok(());
        }
        self.publish_current_catalog(
            Some(FavoriteSettlement {
                target: target.clone(),
                requested: favorite,
                effective: favorite,
            }),
            CatalogChange::Broad,
        )
        .await;
        if source.is_some() {
            self.shared.maintenance.notify_one();
        }
        Ok(())
    }

    async fn apply_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        let lane = self.shared.lane.lock().await;
        if !self
            .shared
            .database
            .set_rating(&target, rating)
            .await
            .unwrap_or(false)
        {
            return;
        }
        let source = self
            .favorite_source(&target)
            .await
            .and_then(|(id, _)| self.client(&id).ok().map(|source| (source, id)));
        drop(lane);
        self.publish_current_catalog(None, CatalogChange::Broad)
            .await;
        if let Some((source, source_id)) = source
            && let Some((_, object_id)) = favorite_object(&source_id, &target)
            && let Err(error) = source.set_rating(&object_id, rating).await
        {
            warn!(%error, "could not write source rating");
        }
    }

    pub(crate) fn reply<T, F, Work>(&self, work: F) -> Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(SourceOwner, Arc<Database>) -> Work + Send + 'static,
        Work: Future<Output = T> + Send + 'static,
    {
        let (sender, receiver) = async_channel::bounded(1);
        let owner = self.clone();
        let database = Arc::clone(&self.shared.database);
        self.shared.runtime.spawn(async move {
            let _ = sender.send(work(owner, database).await).await;
        });
        receiver
    }

    pub(crate) async fn media_source_id(
        &self,
        media_uri: &str,
    ) -> Result<Option<SourceId>, String> {
        match library::source_entity_parts(media_uri) {
            Some((source_id, _, _)) => Ok(Some(source_id)),
            None => self
                .shared
                .database
                .track_source_by_uri(media_uri, &ReadCancellation::new())
                .await
                .map_err(string_error),
        }
    }

    pub(crate) async fn media_client(&self, media_uri: &str) -> Result<Arc<Source>, String> {
        let source_id = self
            .media_source_id(media_uri)
            .await?
            .ok_or_else(source_access_unavailable)?;
        self.client(&source_id)
    }

    pub(crate) fn client(&self, source_id: &SourceId) -> Result<Arc<Source>, String> {
        let stored = self.shared.settings.load();
        let configured = stored
            .sources
            .configured
            .iter()
            .find(|s| &s.configuration.source_id == source_id)
            .cloned()
            .ok_or("The connection no longer exists")?;
        if let Some(cached) = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            && cached.matches(&configured)
        {
            return Ok(cached.source.clone());
        }
        let credential = configured
            .credential
            .as_ref()
            .map(|reference| load_provider_secret(self.shared.secrets.as_ref(), reference))
            .transpose()?
            .flatten();

        let source = Arc::new(
            Source::open(configured.configuration.clone(), credential.clone())
                .map_err(string_error)?,
        );
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let current = self.shared.settings.load();
        if current.sources.configured.iter().any(|item| {
            same_native_input(&item.configuration, &configured.configuration)
                && item.credential == configured.credential
        }) {
            if let Some(cached) = clients.get(source_id)
                && cached.matches(&configured)
            {
                return Ok(cached.source.clone());
            }
            clients.insert(
                source_id.clone(),
                CachedSource {
                    configuration: configured.configuration,
                    credential: configured.credential,
                    source: Arc::clone(&source),
                    refresh: None,
                    observer: None,
                },
            );
            return Ok(source);
        }
        Err(SourceError::Cancelled.to_string())
    }

    pub(crate) async fn open_client(&self, source_id: SourceId) -> Result<Arc<Source>, String> {
        let owner = self.clone();
        tokio::task::spawn_blocking(move || owner.client(&source_id))
            .await
            .map_err(string_error)?
    }

    pub(crate) fn cached_client(&self, source_id: &SourceId) -> Option<Arc<Source>> {
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .map(|cached| cached.source.clone())
    }

    pub fn configuration(&self, source_id: &SourceId) -> Option<SourceConfiguration> {
        configured_source(&self.shared.settings.load().sources, source_id)
            .ok()
            .map(|configured| configured.configuration)
    }

    async fn deliver_favorite(
        &self,
        source_id: SourceId,
        source_key: SourceKey,
        target: FavoriteTarget,
        favorite: bool,
    ) -> bool {
        let Ok(source) = self.open_client(source_id.clone()).await else {
            return true;
        };
        let Some((kind, object_id)) = favorite_object(&source_id, &target) else {
            return false;
        };
        match source.set_favorite(kind, &object_id, favorite).await {
            Ok(()) => {
                let acknowledgement = self
                    .shared
                    .database
                    .acknowledge_remote_favorite(&target, favorite)
                    .await;
                info!(
                    favorite,
                    acknowledged = matches!(acknowledgement, Ok(true)),
                    "Favorite delivered"
                );
                if acknowledgement.is_err() {
                    return true;
                }
            }
            Err(error) if source_error_is_temporary(&error) => {
                info!(favorite, "Favorite delivery deferred");
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorite(&target, favorite, unix_seconds().saturating_add(30))
                    .await;
                return true;
            }
            Err(_) => {
                info!(favorite, "Favorite delivery rejected");
                let previous = match self
                    .shared
                    .database
                    .reject_remote_favorite(&target, favorite)
                    .await
                {
                    Ok(previous) => previous,
                    Err(_) => return true,
                };
                if let Some(previous) = previous {
                    self.publish_current_catalog(
                        Some(FavoriteSettlement {
                            target,
                            requested: favorite,
                            effective: previous,
                        }),
                        CatalogChange::Broad,
                    )
                    .await;
                    self.shared
                        .send(SourceEvent::Notice(SourceNotice {
                            source_key: self
                                .shared
                                .selected()
                                .map_or(source_key, |selected| selected.source_key),
                            kind: SourceNoticeKind::FavoriteRejected,
                        }))
                        .await;
                }
            }
        }
        false
    }

    async fn deliver_pending_favorites(&self) {
        let now = unix_seconds();
        let pending = match self
            .shared
            .database
            .due_remote_favorites(now, 64, &ReadCancellation::new())
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                self.shared.warn_nonfatal(&error.to_string());
                return;
            }
        };
        let full = pending.len() == 64;
        let mut completed = true;
        let mut unavailable = HashSet::new();
        for (media_uri, favorite) in pending {
            let Some((source_id, kind, _)) = library::source_entity_parts(&media_uri) else {
                completed = false;
                continue;
            };
            let target = match kind.as_str() {
                "track" => FavoriteTarget::Track(media_uri),
                "album" => FavoriteTarget::Album(media_uri),
                "artist" => FavoriteTarget::Artist(media_uri),
                _ => {
                    completed = false;
                    continue;
                }
            };
            if unavailable.contains(&source_id) {
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorite(&target, favorite, unix_seconds().saturating_add(30))
                    .await;
                completed = false;
                continue;
            }
            let Some((_, source_key)) = self.favorite_source(&target).await else {
                completed = false;
                continue;
            };
            if self
                .deliver_favorite(source_id.clone(), source_key, target, favorite)
                .await
            {
                unavailable.insert(source_id);
                completed = false;
            }
        }
        if full && completed {
            self.shared.maintenance.notify_one();
        }
    }
}

fn favorite_object(
    source: &SourceId,
    target: &FavoriteTarget,
) -> Option<(SourceEntityKind, String)> {
    let (source_id, kind, object_id) = library::source_entity_parts(target.media_uri())?;
    if &source_id != source || kind != target.kind() {
        return None;
    }
    let kind = match target {
        FavoriteTarget::Track(_) => SourceEntityKind::Track,
        FavoriteTarget::Album(_) => SourceEntityKind::Album,
        FavoriteTarget::Artist(_) => SourceEntityKind::Artist,
    };
    Some((kind, object_id))
}

fn source_error_is_cancelled(error: &SourceError) -> bool {
    matches!(
        error,
        SourceError::Cancelled | SourceError::Library(library::LibraryError::ReadCancelled)
    )
}

fn source_error_is_temporary(error: &SourceError) -> bool {
    source_error_allows_cache(error)
        || matches!(
            error,
            SourceError::Server {
                status: 408 | 425 | 429,
                ..
            }
        )
}

fn configured_source(
    settings: &crate::settings::SourceSettings,
    source_id: &SourceId,
) -> Result<ConfiguredSource, String> {
    settings
        .configured
        .iter()
        .find(|item| &item.configuration.source_id == source_id)
        .cloned()
        .ok_or_else(|| "the configured source no longer exists".to_string())
}

fn source_status(
    stored: &StoredSettings,
    selected: Option<&SelectedLibrary>,
    counts: &HashMap<SourceId, (usize, usize)>,
) -> Option<SourceStatus> {
    let configured = stored.sources.configured.as_ref()?;
    Some(SourceStatus {
        source: SourceSummary {
            id: configured.configuration.source_id.clone(),
            kind: configured.configuration.kind,
            name: configured.configuration.name.clone(),
        },
        cache: SourceCacheSummary {
            selected_music_folder_name: selected
                .and_then(|selected| {
                    let wanted = selected.music_folder_object_id.as_ref()?;
                    selected
                        .music_folders
                        .iter()
                        .find(|folder| &folder.object_id == wanted)
                })
                .map(|folder| folder.name.clone()),
            album_count: counts
                .get(&configured.configuration.source_id)
                .map_or(0, |counts| counts.0),
            track_count: counts
                .get(&configured.configuration.source_id)
                .map_or(0, |counts| counts.1),
        },
    })
}

pub(crate) fn source_error_allows_cache(error: &SourceError) -> bool {
    matches!(
        error,
        SourceError::Network(_)
            | SourceError::Server {
                status: 500..=599,
                ..
            }
    )
}

fn source_refresh_error_text(error: &SourceError) -> String {
    if source_error_allows_cache(error) {
        "Server is unreachable".to_string()
    } else {
        error.to_string()
    }
}

fn editable_source(configuration: &SourceConfiguration) -> Result<EditableSource, String> {
    let credentials = configuration.editable().map_err(string_error)?;
    Ok(EditableSource {
        source: SourceSummary {
            id: configuration.source_id.clone(),
            kind: configuration.kind,
            name: configuration.name.clone(),
        },
        credentials,
    })
}

fn source_progress(progress: SourceReadProgress) -> SourceProgress {
    SourceProgress {
        stage: match progress.stage {
            SourceReadStage::Albums => SourceProgressStage::Albums,
            SourceReadStage::Tracks => SourceProgressStage::Tracks,
            SourceReadStage::Artists => SourceProgressStage::Artists,
            SourceReadStage::Genres => SourceProgressStage::Genres,
            SourceReadStage::Playlists => SourceProgressStage::Playlists,
            SourceReadStage::Home => SourceProgressStage::Home,
            SourceReadStage::Finalizing => SourceProgressStage::Finalizing,
        },
        completed: progress.completed,
        total: progress.total,
    }
}

fn initial_progress() -> SourceProgress {
    SourceProgress {
        stage: SourceProgressStage::Connecting,
        completed: 0,
        total: None,
    }
}

pub(crate) fn source_access_unavailable() -> String {
    "Live source access is unavailable. Check the saved credentials and refresh.".to_string()
}

pub(crate) fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

pub struct SourceSubscription {
    source: SourceOwner,
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
}
pub struct SourceOperationSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<SourceOperation>>,
}

impl SourceOwner {
    pub fn subscribe(&self) -> Arc<SourceSubscription> {
        let mut changes = self.configuration_changes();
        changes.mark_changed();
        Arc::new(SourceSubscription {
            source: self.clone(),
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub fn subscribe_operation(&self) -> Arc<SourceOperationSubscription> {
        let mut changes = self.operation();
        changes.mark_changed();
        Arc::new(SourceOperationSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub async fn save_subsonic(
        &self,
        source_id: Option<SourceId>,
        kind: sources::SubsonicFlavor,
        authentication: sources::SubsonicAuthentication,
        credentials: sources::CredentialInput,
    ) -> sources::SourceResult<()> {
        if let Some(source_id) = source_id {
            self.update_source(
                source_id,
                SourceSetup {
                    kind,
                    authentication,
                    credentials,
                },
            )
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
        } else {
            self.configure_source(SourceSetup {
                kind,
                authentication,
                credentials,
            })
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map(|_| ())
            .map_err(SourceError::Other)
        }
    }
}

impl SourceSubscription {
    pub async fn next(&self) -> sources::SourceResult<Option<SourceStatus>> {
        self.changes
            .lock()
            .await
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(self.source.source_status())
    }
}
impl SourceOperationSubscription {
    pub async fn next(&self) -> sources::SourceResult<SourceOperation> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
impl SourceOwner {
    pub fn editable(&self, source_id: SourceId) -> sources::SourceResult<Option<EditableSource>> {
        let stored = self.shared.settings.load();
        stored
            .sources
            .configured
            .iter()
            .find(|source| source.configuration.source_id == source_id)
            .map(|source| editable_source(&source.configuration).map_err(SourceError::Other))
            .transpose()
    }
}

pub struct SourceCatalogSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<CatalogRevision>>,
}
impl SourceOwner {
    pub fn subscribe_catalog(&self) -> Arc<SourceCatalogSubscription> {
        let mut changes = self.catalog_changes();
        changes.mark_changed();
        Arc::new(SourceCatalogSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
}
impl SourceCatalogSubscription {
    pub async fn next(&self) -> sources::SourceResult<CatalogRevision> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
