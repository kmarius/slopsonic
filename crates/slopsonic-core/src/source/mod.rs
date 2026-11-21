//! Source configuration, browsing selection, and source work.
mod download;
pub mod folders;
mod pins;
pub mod search;
mod selection;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runtime::source::{
    EditableServer, ServerCacheSummary, ServerOperation, ServerProgress, ServerProgressStage,
    ServerStatus, ServerSummary,
};
use crate::runtime::{
    CatalogChange, CatalogPublication, CatalogRevision, FavoriteSettlement, SelectedLibrary,
    ServerEvent, ServerNotice, ServerNoticeKind,
};
use artwork::Artwork;
use async_channel::{Receiver, Sender};
use downloads::Downloads;
use library::{CatalogUpdate, Database, FavoriteTarget, FolderChoice, FolderKey, ReadCancellation};
use sources::{
    LiveFolderPage, SelectedFeed, Source, SourceConfiguration, SourceEntityKind, SourceError,
    SourceReadProgress, SourceReadStage, SourceSetup,
};
use tracing::{info, warn};

use crate::playback::PlaybackOwner;
use crate::settings::{ConfiguredServer, SettingsFile, StoredSettings};

const SOURCE_CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub(crate) struct ServerOutputs {
    pub(crate) events: Sender<ServerEvent>,
}

#[derive(Clone)]
pub struct ServerOwner {
    pub(crate) shared: Arc<Shared>,
}

struct CachedSource {
    configuration: SourceConfiguration,
    credential: Option<String>,
    source: Arc<Source>,
    refresh: Option<Refresh>,
    observer: Option<(Arc<SelectedFeed>, tokio::task::AbortHandle)>,
}

struct Refresh {
    cancelled: Arc<AtomicBool>,
    progress: ServerProgress,
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
    fn matches(&self, configured: &ConfiguredServer) -> bool {
        same_native_input(&self.configuration, &configured.configuration)
            && self.credential == configured.credential
    }
}

fn same_native_input(left: &SourceConfiguration, right: &SourceConfiguration) -> bool {
    left.kind == right.kind && left.provider_payload == right.provider_payload
}

pub(crate) struct Shared {
    home_showcase_variation: i64,
    home_explore_variation: std::sync::atomic::AtomicI64,
    pub(crate) artwork: Artwork,
    pub(crate) database: Arc<Database>,
    downloads: Downloads,
    pub(crate) settings: SettingsFile,
    client: Mutex<Option<CachedSource>>,
    pub(crate) runtime: tokio::runtime::Handle,
    outputs: ServerOutputs,
    operation: tokio::sync::watch::Sender<ServerOperation>,
    catalog_changed: tokio::sync::watch::Sender<CatalogRevision>,
    configuration_changed: tokio::sync::watch::Sender<()>,
    selected: Mutex<Option<SelectedLibrary>>,
    catalog_counts: Mutex<Option<(usize, usize)>>,
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
    pub(crate) fn cached_server_counts(&self) -> Option<(usize, usize)> {
        *self
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    async fn refresh_source_counts(&self) -> Result<(usize, usize), String> {
        let counts = self
            .database
            .server_counts(&ReadCancellation::new())
            .await
            .map_err(string_error)?;
        self.catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(counts);
        Ok(counts)
    }

    async fn load_source_counts(&self) -> Result<(), String> {
        let counts = match self
            .database
            .cached_server(&ReadCancellation::new())
            .await
            .map_err(string_error)?
        {
            Some(_) if self.settings.load().sources.configured.is_some() => Some(
                self.database
                    .server_counts(&ReadCancellation::new())
                    .await
                    .map_err(string_error)?,
            ),
            _ => None,
        };
        *self
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = counts;
        Ok(())
    }

    pub(crate) fn server_status(&self, selected: Option<&SelectedLibrary>) -> Option<ServerStatus> {
        server_status(&self.settings.load(), selected, self.cached_server_counts())
    }

    pub(crate) fn selected(&self) -> Option<SelectedLibrary> {
        self.selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) async fn send(&self, event: ServerEvent) {
        if matches!(
            &event,
            ServerEvent::Configured(_)
                | ServerEvent::Selected { .. }
                | ServerEvent::ReleaseSelected
        ) {
            self.configuration_changed.send_replace(());
        }
        if let ServerEvent::CatalogPublished(publication) = &event
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

    fn publish_operation(&self, operation: ServerOperation) {
        let active = self
            .asset_download
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if matches!(
            operation,
            ServerOperation::Idle | ServerOperation::Refreshing { .. }
        ) && active.is_some()
            && matches!(
                *self.operation.borrow(),
                ServerOperation::Downloading { .. }
            )
        {
            return;
        }
        self.operation.send_replace(operation.clone());
        let _ = self
            .outputs
            .events
            .try_send(ServerEvent::Operation(operation));
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

    /// Runs `install` only while `cancelled` is the current selection. The intent lock is held
    /// throughout, so a concurrent `cancel_selection` lands either before the check or after the install.
    fn install_if_current<T>(
        &self,
        cancelled: &Arc<AtomicBool>,
        install: impl FnOnce() -> T,
    ) -> Option<T> {
        let intent = self
            .selection_intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = !cancelled.load(Ordering::Acquire)
            && intent
                .upgrade()
                .is_some_and(|active| Arc::ptr_eq(&active, cancelled));
        current.then(install)
    }

    fn cancel_selection(&self) {
        let intent = self
            .selection_intent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cancelled) = intent.upgrade() {
            cancelled.store(true, Ordering::Release);
        }
    }

    pub(crate) fn begin_refresh(&self, replace: bool, visible: bool) -> Option<Arc<AtomicBool>> {
        let mut client = self.client.lock().unwrap_or_else(|p| p.into_inner());
        let cached = client.as_mut()?;
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

    fn refresh_is_current(&self, token: &Arc<AtomicBool>) -> bool {
        !token.load(Ordering::Acquire)
            && self
                .client
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .and_then(|cached| cached.refresh.as_ref())
                .is_some_and(|refresh| Arc::ptr_eq(&refresh.cancelled, token))
    }

    fn is_refreshing(&self) -> bool {
        self.client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|cached| cached.refresh.as_ref())
            .is_some_and(|refresh| !refresh.cancelled.load(Ordering::Acquire))
    }

    pub(crate) fn present_refresh(&self) {
        let progress = self
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|cached| cached.refresh.as_ref())
            .filter(|refresh| refresh.visible)
            .map(|refresh| refresh.progress.clone());
        if let Some(progress) = progress
            && !self.operation.borrow().blocks_library()
        {
            self.publish_operation(ServerOperation::Refreshing { progress });
        }
    }

    pub(crate) fn refresh_progress(&self, token: &Arc<AtomicBool>, value: SourceReadProgress) {
        let progress = source_progress(value);
        {
            let mut client = self.client.lock().unwrap_or_else(|p| p.into_inner());
            let Some(refresh) = client.as_mut().and_then(|cached| cached.refresh.as_mut()) else {
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
        if matches!(
            &*self.operation.borrow(),
            ServerOperation::Refreshing { .. }
        ) {
            self.publish_operation(ServerOperation::Refreshing { progress });
        }
    }
}

impl ServerOwner {
    pub(crate) fn open_dormant(
        artwork: Artwork,
        database: Arc<Database>,
        downloads: Downloads,
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        outputs: ServerOutputs,
    ) -> Arc<Self> {
        let stored = settings.load();
        let operation = stored
            .sources
            .configured
            .as_ref()
            .map_or(ServerOperation::Idle, |_| ServerOperation::Switching {
                progress: initial_progress(),
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
            client: Mutex::new(None),
            runtime,
            outputs,
            operation: tokio::sync::watch::channel(operation).0,
            selected: Mutex::new(None),
            catalog_counts: Mutex::new(None),
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
        let clients = weak.clone();
        shared.downloads.install_source_resolver(move || {
            let shared = clients.upgrade()?;
            Self { shared }.client().ok()
        });
        shared.database.observe_catalog(move |update| {
            let shared = weak.upgrade();
            async move {
                if let Some(shared) = shared {
                    ServerOwner { shared }.catalog_updated(update).await;
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
        if stored.sources.configured.is_some() {
            let owner = self.clone();
            self.shared.runtime.spawn(async move {
                let _ = owner.select_server().await;
            });
        }
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            if let Err(error) = owner.shared.load_source_counts().await {
                owner.shared.warn_nonfatal(&error);
            }
            owner
                .shared
                .send(ServerEvent::Configured(owner.server_status()))
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
                let owner = ServerOwner { shared };
                owner.deliver_pending_favorites().await;
                owner.deliver_pending_ratings().await;
            }
        });
        Ok(())
    }

    /// Drops everything kept for the configured server: queue, favorite and rating outboxes,
    /// downloads, cached catalog and artwork, and settings that point at its items.
    pub(crate) async fn reset_server_state(&self) -> Result<(), String> {
        if self.configuration().is_none() {
            return Ok(());
        }
        // Cancel the selection before retiring the client, so a startup connect cannot reopen
        // and observe the old server in between; retiring it stops its refresh and observer.
        self.shared.cancel_selection();
        self.shared
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.release_selected().await;
        // Each step runs even when an earlier one fails, so a partial reset leaves no stale state.
        let mut failure = None;
        let mut note = |result: Result<(), String>| {
            if let Err(error) = result {
                self.shared.warn_nonfatal(&error);
                failure.get_or_insert(error);
            }
        };
        note(self.shared.settings.update(|stored| {
            let ui = &mut stored.ui;
            ui.sidebar.pins.clear();
            ui.sidebar.playlist_pins_imported = false;
            ui.recent_search_results.clear();
            ui.random_play.genre = None;
            ui.downloads.rules = downloads::DownloadRules::default();
            Ok(())
        }));
        let database = &self.shared.database;
        if let Ok(playback) = self.shared.playback() {
            note(playback.forget_server().await);
        }
        // Clearing downloads needs the server attached, even when it never connected.
        note(self.shared.downloads.attach(None).await);
        self.shared.downloads.clear(false);
        self.shared.downloads.detach();
        note(
            database
                .remove_server()
                .await
                .map(|_| ())
                .map_err(string_error),
        );
        self.shared
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.shared
            .catalog_counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        note(
            self.shared
                .artwork
                .invalidate_server_images()
                .map_err(|error| error.to_string()),
        );
        failure.map_or(Ok(()), Err)
    }

    pub(crate) fn spawn_serialized<F, Work>(&self, work: F)
    where
        F: FnOnce(ServerOwner) -> Work + Send + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        self.shared.runtime.spawn(async move {
            let lane_owner = Arc::clone(&shared);
            let _lane = lane_owner.lane.lock().await;
            work(ServerOwner { shared }).await;
        });
    }
}

impl ServerOwner {
    pub fn home_variations(&self) -> (i64, i64) {
        (
            self.shared.home_showcase_variation,
            self.shared.home_explore_variation.load(Ordering::Relaxed),
        )
    }
}

impl ServerOwner {
    pub fn prepare_collection(&self, media_uri: String) -> Receiver<Result<(), String>> {
        self.reply(move |owner, database| async move {
            let source = owner.media_client(&media_uri).await?;
            let result = source.prepare_collection(&database, &media_uri).await;
            result.map_err(string_error)
        })
    }
}

impl ServerOwner {
    pub fn server_status(&self) -> Option<ServerStatus> {
        self.shared.server_status(self.shared.selected().as_ref())
    }
}

impl ServerOwner {
    pub fn operation(&self) -> tokio::sync::watch::Receiver<ServerOperation> {
        self.shared.operation.subscribe()
    }

    pub fn catalog_changes(&self) -> tokio::sync::watch::Receiver<CatalogRevision> {
        self.shared.catalog_changed.subscribe()
    }

    pub fn configuration_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.shared.configuration_changed.subscribe()
    }

    pub fn cached_server_counts(&self) -> Option<(usize, usize)> {
        self.shared.cached_server_counts()
    }

    /// Browse the server without changing the selected library.
    pub async fn browse_folder(
        &self,
        folder_object_id: Option<String>,
        music_folder_object_id: Option<String>,
    ) -> Result<LiveFolderPage, String> {
        self.open_client()
            .await?
            .browse_folder(
                &self.shared.database,
                folder_object_id.as_deref(),
                music_folder_object_id.as_deref(),
            )
            .await
            .map_err(string_error)
    }

    pub fn selected_library(&self) -> Option<SelectedLibrary> {
        self.shared.selected()
    }
}

impl ServerOwner {
    pub fn configure_server(
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
                .publish_operation(ServerOperation::Adding {
                    progress: initial_progress(),
                })
                .await;
            let result = async {
                let connected = Source::connect(input).await.map_err(string_error)?;
                let (configuration, source, credential) = connected.into_parts();
                if !owner.shared.selection_is_current(&cancelled) {
                    return Err(SourceError::Cancelled.to_string());
                }
                let configured = ConfiguredServer {
                    configuration: configuration.clone(),
                    credential: None,
                    music_folder_id: None,
                };
                let configured = owner.persist_connected_server(&configured, credential)?;
                owner
                    .shared
                    .database
                    .reconcile_server(&configuration.name)
                    .await
                    .map_err(string_error)?;
                let source = Arc::new(source);
                owner
                    .install_selected(
                        configured,
                        Some(Arc::clone(&source)),
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
                    let _ = owner.refresh_now(&source, false, true).await;
                }
                Err(error) => {
                    if owner.shared.selected().is_some()
                        && let Some(source) = owner.cached_client()
                    {
                        owner
                            .shared
                            .install_if_current(&cancelled, || owner.start_observer(source, false));
                    }
                    owner
                        .publish_operation(ServerOperation::Failed {
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

    pub fn update_server(&self, input: SourceSetup) -> Receiver<Result<(), String>> {
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let result = owner.edit_configured_server(input).await;
            if let Err(error) = &result {
                owner.shared.warn_nonfatal(&error);
            }
            let _ = sender.try_send(result);
        });
        receiver
    }
}

impl ServerOwner {
    /// Completes when the source's stored catalog is selected. Connection and refresh
    /// continue in the background.
    pub(crate) async fn select_server(&self) -> sources::SourceResult<()> {
        let cancelled = self.shared.begin_selection();
        let owner = self.clone();
        self.shared
            .runtime
            .spawn(async move {
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                owner.shared.publish_operation(ServerOperation::Switching {
                    progress: initial_progress(),
                });
                let result = owner.select_now(Arc::clone(&cancelled), true).await;
                if cancelled.load(Ordering::Acquire) {
                    return Err(SourceError::Cancelled.to_string());
                }
                if let Err(error) = &result {
                    if owner.shared.selected().is_some()
                        && let Some(source) = owner.cached_client()
                    {
                        owner
                            .shared
                            .install_if_current(&cancelled, || owner.start_observer(source, false));
                    }
                    owner.shared.publish_operation(ServerOperation::Failed {
                        message: error.clone(),
                        add_form: false,
                    });
                }
                result?;
                owner
                    .selected_library()
                    .map(|_| ())
                    .ok_or_else(|| SourceError::Cancelled.to_string())
            })
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

impl ServerOwner {
    pub async fn refresh_server(&self, automatic: bool) -> sources::SourceResult<()> {
        let source = self.open_client().await.map_err(SourceError::Other)?;
        self.refresh_now(&source, automatic, true)
            .await
            .map_err(SourceError::Other)
    }
}

impl ServerOwner {
    pub async fn forget_server(&self) -> sources::SourceResult<()> {
        if self.configuration().is_none() {
            return Ok(());
        }
        self.shared.cancel_selection();
        self.shared
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if matches!(
            &*self.shared.operation.borrow(),
            ServerOperation::Refreshing { .. }
        ) {
            self.shared.publish_operation(ServerOperation::Idle);
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.spawn_serialized(move |owner| async move {
            let removed = owner.reset_server_state().await;
            if let Err(error) = &removed {
                owner.shared.warn_nonfatal(error);
            }
            if let Err(error) = owner.shared.settings.update(|stored| {
                stored.sources.configured = None;
                Ok(())
            }) {
                owner.shared.warn_nonfatal(&error);
                let _ = sender.try_send(Err(error));
                return;
            }
            owner
                .shared
                .send(ServerEvent::Configured(owner.shared.server_status(None)))
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

impl ServerOwner {
    pub fn download_media(&self, subject: downloads::DownloadSubject, media_uris: Vec<String>) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            let mut server = false;
            for media_uri in &media_uris {
                if owner.is_server_media(media_uri).await.unwrap_or(false) {
                    server = true;
                    break;
                }
            }
            if server {
                match owner
                    .shared
                    .database
                    .cached_server(&ReadCancellation::new())
                    .await
                {
                    Ok(Some(_)) => {
                        if let Err(error) = owner.shared.downloads.attach(None).await {
                            owner.shared.warn_nonfatal(&error);
                        }
                    }
                    Ok(None) => owner.shared.warn_nonfatal(&source_access_unavailable()),
                    Err(error) => owner.shared.warn_nonfatal(&string_error(error)),
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

impl ServerOwner {
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

impl ServerOwner {
    pub fn set_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        let owner = self.clone();
        self.shared.runtime.spawn(async move {
            owner.apply_rating(target, rating).await;
        });
    }
}

impl ServerOwner {
    pub fn refresh_home(
        &self,
        kind: crate::settings::HomeSectionKind,
    ) -> Receiver<Result<(), String>> {
        use crate::settings::HomeSectionKind;
        if kind == HomeSectionKind::NewlyAdded {
            return self.reply(move |owner, _| async move {
                owner.refresh_server(false).await.map_err(string_error)
            });
        }
        self.reply(move |owner, database| async move {
            if kind == HomeSectionKind::Explore {
                owner
                    .shared
                    .home_explore_variation
                    .fetch_add(1, Ordering::Relaxed);
                if owner.selected_library().is_some() {
                    owner
                        .publish_current_catalog(None, CatalogChange::Home)
                        .await;
                }
                return Ok(());
            }
            let source = owner.open_client().await?;
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

impl ServerOwner {
    /// Whether the target is a server item while a server is configured.
    async fn favorite_source(&self, target: &FavoriteTarget) -> bool {
        let server = match library::entity_parts(target.media_uri()) {
            Some((kind, _)) => kind == target.kind(),
            None => match target {
                FavoriteTarget::Track(media_uri) => self
                    .shared
                    .database
                    .is_server_track(media_uri, &ReadCancellation::new())
                    .await
                    .unwrap_or(false),
                FavoriteTarget::Album(_) | FavoriteTarget::Artist(_) => false,
            },
        };
        server
            && self
                .shared
                .database
                .cached_server(&ReadCancellation::new())
                .await
                .is_ok_and(|cached| cached.is_some())
    }

    async fn apply_favorite(&self, target: FavoriteTarget, favorite: bool) -> Result<(), String> {
        if !self.favorite_source(&target).await {
            return Err("this item does not belong to a server".to_string());
        }
        let changed = self
            .shared
            .database
            .queue_remote_favorite(&target, favorite, unix_seconds())
            .await
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
        self.shared.maintenance.notify_one();
        Ok(())
    }

    async fn apply_rating(&self, target: FavoriteTarget, rating: Option<u8>) {
        let lane = self.shared.lane.lock().await;
        if !self.favorite_source(&target).await
            || !self
                .shared
                .database
                .queue_remote_rating(&target, rating, unix_seconds())
                .await
                .unwrap_or(false)
        {
            return;
        }
        drop(lane);
        self.publish_current_catalog(None, CatalogChange::Broad)
            .await;
        self.shared.maintenance.notify_one();
    }

    pub(crate) fn reply<T, F, Work>(&self, work: F) -> Receiver<T>
    where
        T: Send + 'static,
        F: FnOnce(ServerOwner, Arc<Database>) -> Work + Send + 'static,
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

    pub(crate) async fn is_server_media(&self, media_uri: &str) -> Result<bool, String> {
        self.shared
            .database
            .is_server_media(media_uri)
            .await
            .map_err(string_error)
    }

    pub(crate) async fn media_client(&self, media_uri: &str) -> Result<Arc<Source>, String> {
        if !self.is_server_media(media_uri).await? {
            return Err(source_access_unavailable());
        }
        self.client().map_err(|_| source_access_unavailable())
    }

    pub(crate) fn client(&self) -> Result<Arc<Source>, String> {
        let configured = self
            .shared
            .settings
            .load()
            .sources
            .configured
            .ok_or("The connection no longer exists")?;
        if let Some(cached) = self
            .shared
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && cached.matches(&configured)
        {
            return Ok(cached.source.clone());
        }
        let source = Arc::new(
            Source::open(
                configured.configuration.clone(),
                configured.credential.clone(),
            )
            .map_err(string_error)?,
        );
        let mut client = self.shared.client.lock().unwrap_or_else(|p| p.into_inner());
        let current = self.shared.settings.load();
        if current.sources.configured.iter().any(|item| {
            same_native_input(&item.configuration, &configured.configuration)
                && item.credential == configured.credential
        }) {
            if let Some(cached) = client.as_ref()
                && cached.matches(&configured)
            {
                return Ok(cached.source.clone());
            }
            *client = Some(CachedSource {
                configuration: configured.configuration,
                credential: configured.credential,
                source: Arc::clone(&source),
                refresh: None,
                observer: None,
            });
            return Ok(source);
        }
        drop(client);
        self.client()
    }

    pub(crate) async fn open_client(&self) -> Result<Arc<Source>, String> {
        let owner = self.clone();
        tokio::task::spawn_blocking(move || owner.client())
            .await
            .map_err(string_error)?
    }

    pub(crate) fn cached_client(&self) -> Option<Arc<Source>> {
        self.shared
            .client
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|cached| cached.source.clone())
    }

    pub fn configuration(&self) -> Option<SourceConfiguration> {
        self.shared
            .settings
            .load()
            .sources
            .configured
            .map(|configured| configured.configuration)
    }

    async fn deliver_favorite(&self, target: FavoriteTarget, favorite: bool) -> bool {
        let Ok(source) = self.open_client().await else {
            return true;
        };
        let Some((kind, object_id)) = favorite_object(&target) else {
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
                        .send(ServerEvent::Notice(ServerNotice {
                            kind: ServerNoticeKind::FavoriteRejected,
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
        for (media_uri, favorite) in pending {
            let Some(target) = pending_target(media_uri) else {
                completed = false;
                continue;
            };
            if !self.favorite_source(&target).await {
                completed = false;
                continue;
            }
            if self.deliver_favorite(target, favorite).await {
                let _ = self
                    .shared
                    .database
                    .defer_remote_favorites(now, unix_seconds().saturating_add(30))
                    .await;
                completed = false;
                break;
            }
        }
        if full && completed {
            self.shared.maintenance.notify_one();
        }
    }

    async fn deliver_pending_ratings(&self) {
        let database = &self.shared.database;
        let now = unix_seconds();
        let pending = match database
            .due_remote_ratings(now, 64, &ReadCancellation::new())
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
        let mut rejected = false;
        for (media_uri, rating) in pending {
            let Some(target) = pending_target(media_uri) else {
                completed = false;
                continue;
            };
            let retry = unix_seconds().saturating_add(30);
            let Ok(source) = self.open_client().await else {
                let _ = database.defer_remote_ratings(now, retry).await;
                completed = false;
                break;
            };
            let Some((_, object_id)) = favorite_object(&target) else {
                completed = false;
                continue;
            };
            match source.set_rating(&object_id, rating).await {
                Ok(()) => {
                    if database
                        .acknowledge_remote_rating(&target, rating)
                        .await
                        .is_err()
                    {
                        completed = false;
                    }
                }
                Err(error) if source_error_is_temporary(&error) => {
                    let _ = database.defer_remote_ratings(now, retry).await;
                    completed = false;
                    break;
                }
                Err(error) => {
                    warn!(%error, "server rejected rating");
                    rejected |= matches!(
                        database.reject_remote_rating(&target, rating).await,
                        Ok(true)
                    );
                }
            }
        }
        if rejected {
            self.publish_current_catalog(None, CatalogChange::Broad)
                .await;
        }
        if full && completed {
            self.shared.maintenance.notify_one();
        }
    }
}

fn pending_target(media_uri: String) -> Option<FavoriteTarget> {
    let (kind, _) = library::entity_parts(&media_uri)?;
    let target = match kind.as_str() {
        "track" => FavoriteTarget::Track(media_uri),
        "album" => FavoriteTarget::Album(media_uri),
        "artist" => FavoriteTarget::Artist(media_uri),
        _ => return None,
    };
    Some(target)
}

fn favorite_object(target: &FavoriteTarget) -> Option<(SourceEntityKind, String)> {
    let (kind, object_id) = library::entity_parts(target.media_uri())?;
    if kind != target.kind() {
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
            SourceError::Tls(_)
                | SourceError::Cancelled
                | SourceError::Server {
                    status: 408 | 425 | 429,
                    ..
                }
        )
}

fn configured_source(
    settings: &crate::settings::ServerSettings,
) -> Result<ConfiguredServer, String> {
    settings
        .configured
        .clone()
        .ok_or_else(|| "the configured source no longer exists".to_string())
}

fn server_status(
    stored: &StoredSettings,
    selected: Option<&SelectedLibrary>,
    counts: Option<(usize, usize)>,
) -> Option<ServerStatus> {
    let configured = stored.sources.configured.as_ref()?;
    let (album_count, track_count) = counts.unwrap_or_default();
    Some(ServerStatus {
        source: ServerSummary {
            kind: configured.configuration.kind,
            name: configured.configuration.name.clone(),
        },
        cache: ServerCacheSummary {
            selected_music_folder_name: selected
                .and_then(|selected| {
                    let wanted = selected.music_folder_object_id.as_ref()?;
                    selected
                        .music_folders
                        .iter()
                        .find(|folder| &folder.object_id == wanted)
                })
                .map(|folder| folder.name.clone()),
            album_count,
            track_count,
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

fn editable_source(configuration: &SourceConfiguration) -> Result<EditableServer, String> {
    let credentials = configuration.editable().map_err(string_error)?;
    Ok(EditableServer {
        source: ServerSummary {
            kind: configuration.kind,
            name: configuration.name.clone(),
        },
        credentials,
    })
}

fn source_progress(progress: SourceReadProgress) -> ServerProgress {
    ServerProgress {
        stage: match progress.stage {
            SourceReadStage::Albums => ServerProgressStage::Albums,
            SourceReadStage::Tracks => ServerProgressStage::Tracks,
            SourceReadStage::Artists => ServerProgressStage::Artists,
            SourceReadStage::Genres => ServerProgressStage::Genres,
            SourceReadStage::Playlists => ServerProgressStage::Playlists,
            SourceReadStage::Home => ServerProgressStage::Home,
            SourceReadStage::Finalizing => ServerProgressStage::Finalizing,
        },
        completed: progress.completed,
        total: progress.total,
    }
}

fn initial_progress() -> ServerProgress {
    ServerProgress {
        stage: ServerProgressStage::Connecting,
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

pub(crate) fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

pub struct ServerSubscription {
    source: ServerOwner,
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<()>>,
}
pub struct ServerOperationSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<ServerOperation>>,
}

impl ServerOwner {
    pub fn subscribe(&self) -> Arc<ServerSubscription> {
        let mut changes = self.configuration_changes();
        changes.mark_changed();
        Arc::new(ServerSubscription {
            source: self.clone(),
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub async fn save_subsonic(
        &self,
        edit: bool,
        authentication: sources::SubsonicAuthentication,
        credentials: sources::CredentialInput,
    ) -> sources::SourceResult<()> {
        let setup = SourceSetup {
            authentication,
            credentials,
        };
        if edit {
            self.update_server(setup)
                .recv()
                .await
                .map_err(|error| SourceError::Other(error.to_string()))?
                .map_err(SourceError::Other)
        } else {
            self.configure_server(setup)
                .recv()
                .await
                .map_err(|error| SourceError::Other(error.to_string()))?
                .map(|_| ())
                .map_err(SourceError::Other)
        }
    }
}

impl ServerSubscription {
    pub async fn next(&self) -> sources::SourceResult<Option<ServerStatus>> {
        self.changes
            .lock()
            .await
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(self.source.server_status())
    }
}
impl ServerOperationSubscription {
    pub async fn next(&self) -> sources::SourceResult<ServerOperation> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
impl ServerOwner {
    pub fn editable(&self) -> sources::SourceResult<Option<EditableServer>> {
        self.shared
            .settings
            .load()
            .sources
            .configured
            .as_ref()
            .map(|source| editable_source(&source.configuration).map_err(SourceError::Other))
            .transpose()
    }
}

pub struct ServerCatalogSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<CatalogRevision>>,
}
impl ServerCatalogSubscription {
    pub async fn next(&self) -> sources::SourceResult<CatalogRevision> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
