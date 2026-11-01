//! Provider operations write canonical catalog data directly.
//! Source owns native client lifetimes and publishes committed changes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::{DownloadQuality, ResolvedDownload};
use async_channel::{Receiver, Sender};
use audio_processing::ResolvedStream;
use library::{Database, Freshness};
use playback::{SourceReportFact, StreamRequest};
use serde::{Deserialize, Serialize};

use crate::policy::raw_item_id;
use crate::{
    ImageBytes, SourceConfiguration, SourceError, SourceId, SourceResult, SourceSettingsChange,
    SourceSetup,
};

pub(crate) const LIVE_CHANGE_LIMIT: usize = 128;

pub struct SelectedFeed {
    source: Arc<Source>,
    pending: Mutex<Option<SelectedFeedChange>>,
    wake: Sender<()>,
    cancelled: Arc<AtomicBool>,
    remote_task: Mutex<Option<tokio::task::AbortHandle>>,
    receiver: Receiver<()>,
}

enum SelectedFeedChange {
    Remote(RemoteItemChange),
}

impl SelectedFeed {
    fn submit(&self, incoming: SelectedFeedChange) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = Some(match pending.take() {
            Some(current) => current.merge(incoming),
            None => incoming,
        });
        drop(pending);
        let _ = self.wake.try_send(());
        true
    }
}

impl SelectedFeedChange {
    fn merge(self, incoming: Self) -> Self {
        match (self, incoming) {
            (Self::Remote(current), Self::Remote(incoming)) => {
                Self::Remote(current.merge(incoming))
            }
        }
    }
}

impl SelectedFeed {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self
            .remote_task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            task.abort();
        }
        let _ = self.wake.try_send(());
    }

    pub fn wake_pending(&self) {
        let _ = self.wake.try_send(());
    }

    pub fn cancellation(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }

    pub async fn wait_for_change(&self) -> bool {
        loop {
            if self.receiver.recv().await.is_err() {
                return false;
            }
            if self.cancelled.load(Ordering::Acquire) {
                return false;
            }
            if self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some()
            {
                return true;
            }
        }
    }

    pub async fn apply_pending(
        &self,
        database: &Database,
        _source_key: library::SourceKey,
    ) -> SourceResult<bool> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        let change = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .ok_or(SourceError::InvalidRequest(
                "Selected feed has no pending change",
            ))?;
        match change {
            SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost) => Ok(false),
            SelectedFeedChange::Remote(RemoteItemChange::Items(items)) => self
                .source
                .apply_changes(database, items)
                .await
                .map(|()| true),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceReadStage {
    Albums,
    Tracks,
    Artists,
    Genres,
    Playlists,
    Home,
    Finalizing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceReadProgress {
    pub stage: SourceReadStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceInputIdentity {
    pub source_id: SourceId,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeImageRef {
    pub item_id: String,
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blur_hash: Option<String>,
}

impl NativeImageRef {
    pub fn new(item_id: impl Into<String>, tag: impl Into<Option<String>>) -> Self {
        Self {
            item_id: item_id.into(),
            tag: tag.into(),
            blur_hash: None,
        }
    }
}

pub fn native_artwork_binding(
    source_id: &str,
    image: &NativeImageRef,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&[library::ArtworkCandidate::new(
        library::ArtworkImage::Native {
            source_id: SourceId::new(source_id),
            item_id: image.item_id.clone(),
            tag: image.tag.clone(),
        },
        image.blur_hash.clone(),
    )])
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageSize {
    Original,
    Thumbnail(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceImageRequest {
    Native {
        image_ref: NativeImageRef,
        size: ImageSize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveFolder {
    pub object_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveFolderPage {
    pub folders: Vec<LiveFolder>,
    pub tracks: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SourceEntityKind {
    Track,
    Album,
    Artist,
}
impl SourceEntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Album => "album",
            Self::Artist => "artist",
        }
    }
}

pub enum SourceRadioSeed {
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
    Genre(String),
}

pub enum SourceCollection {
    Album(String),
    Artist(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceHomeSection {
    MostPlayed,
    NewlyAdded,
    RecentlyPlayed,
    RecentlyReleased,
}

impl SourceHomeSection {
    pub fn id(self) -> &'static str {
        match self {
            Self::MostPlayed => "most-played",
            Self::NewlyAdded => "newly-added",
            Self::RecentlyPlayed => "recently-played",
            Self::RecentlyReleased => "recently-released",
        }
    }
}

pub(crate) fn optional_collection_error(error: SourceError) -> SourceResult<()> {
    if matches!(error, SourceError::Cancelled | SourceError::Library(_)) {
        return Err(error);
    }
    tracing::warn!(%error, "optional source collection unavailable; retaining its cached observations");
    Ok(())
}

pub(crate) async fn acquire_home<F, Work>(
    changes: &mut library::Catalog<'_>,
    cancelled: &(dyn Fn() -> bool + Send + Sync),
    mut read: F,
) -> SourceResult<()>
where
    F: FnMut(SourceHomeSection) -> Work,
    Work: std::future::Future<Output = SourceResult<Vec<library::HomeEntryInput>>>,
{
    for section in [
        SourceHomeSection::MostPlayed,
        SourceHomeSection::NewlyAdded,
        SourceHomeSection::RecentlyPlayed,
        SourceHomeSection::RecentlyReleased,
    ] {
        match read(section).await {
            Ok(entries) => {
                if cancelled() {
                    return Err(SourceError::Cancelled);
                }
                changes
                    .replace_source_home_section(section.id(), None, &(entries))
                    .await?;
            }
            Err(error) => optional_collection_error(error)?,
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteItemUpdate {
    Metadata,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteItemChange {
    Items(BTreeMap<String, RemoteItemUpdate>),
    BoundaryLost,
}

impl RemoteItemChange {
    pub(crate) fn items(upserts: Vec<String>, removals: Vec<String>) -> Self {
        let mut items: BTreeMap<_, _> = upserts
            .into_iter()
            .map(|id| (id, RemoteItemUpdate::Metadata))
            .collect();
        for id in removals {
            if matches!(
                items.insert(id, RemoteItemUpdate::Remove),
                Some(RemoteItemUpdate::Metadata)
            ) {
                return Self::BoundaryLost;
            }
        }
        if items.len() > LIVE_CHANGE_LIMIT {
            Self::BoundaryLost
        } else {
            Self::Items(items)
        }
    }

    fn merge(self, incoming: Self) -> Self {
        let (Self::Items(mut current), Self::Items(incoming)) = (self, incoming) else {
            return Self::BoundaryLost;
        };
        for (id, change) in incoming {
            if let Some(previous) = current.get_mut(&id) {
                match (previous, change) {
                    (RemoteItemUpdate::Metadata, RemoteItemUpdate::Metadata)
                    | (RemoteItemUpdate::Remove, RemoteItemUpdate::Remove) => {}
                    _ => return Self::BoundaryLost,
                }
            } else {
                current.insert(id, change);
            }
        }
        if current.len() > LIVE_CHANGE_LIMIT {
            Self::BoundaryLost
        } else {
            Self::Items(current)
        }
    }
}

pub struct ConnectedSource {
    configuration: SourceConfiguration,
    source: Source,
    credential: Option<String>,
}

impl ConnectedSource {
    pub fn into_parts(self) -> (SourceConfiguration, Source, Option<String>) {
        (self.configuration, self.source, self.credential)
    }

    pub(crate) fn subsonic(
        configuration: SourceConfiguration,
        source: crate::subsonic::SubsonicSource,
        credential: Option<String>,
    ) -> Self {
        Self {
            source: Source::new(configuration.source_id.clone(), source),
            configuration,
            credential,
        }
    }
}

pub enum SourceEditResult {
    Unchanged,
    ConfigurationOnly(SourceConfiguration),
    Connected(Box<ConnectedSource>),
}

pub struct Source {
    source_id: SourceId,
    retired: AtomicBool,
    pub subsonic: crate::subsonic::SubsonicSource,
}

impl Source {
    pub(crate) async fn catalog_operation<T, E>(
        &self,
        database: &Database,
        work: impl std::ops::AsyncFnOnce(&mut library::Catalog<'_>) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<SourceError>,
    {
        if self.is_retired() {
            return Err(SourceError::Cancelled.into());
        }
        let mut changes = database
            .catalog(self.source_id.as_str(), None)
            .await
            .map_err(SourceError::from)?;
        let result = work(&mut changes).await;
        if !self.is_retired() {
            database
                .publish_catalog_update(self.source_id.as_str(), changes.into_changes())
                .await;
        }
        result
    }

    fn track_object_id(&self, media_uri: &str) -> SourceResult<String> {
        library::source_entity_parts(media_uri)
            .filter(|(source_id, kind, _)| source_id == &self.source_id && kind == "track")
            .map(|(_, _, object_id)| object_id)
            .ok_or(SourceError::InvalidRequest(
                "media URI does not belong to this source",
            ))
    }

    pub(crate) fn new(source_id: SourceId, subsonic: crate::subsonic::SubsonicSource) -> Self {
        Self {
            source_id,
            retired: AtomicBool::new(false),
            subsonic,
        }
    }

    pub async fn connect(source_id: SourceId, input: SourceSetup) -> SourceResult<ConnectedSource> {
        match input {
            SourceSetup::OpenSubsonic {
                kind,
                authentication,
                credentials,
            } => crate::subsonic::connect(source_id, kind, authentication, credentials).await,
        }
    }

    pub async fn edit(
        current: SourceConfiguration,
        current_credential: Option<String>,
        input: SourceSettingsChange,
    ) -> SourceResult<SourceEditResult> {
        match input {
            SourceSettingsChange::OpenSubsonic {
                authentication,
                credentials,
                ..
            } => {
                crate::subsonic::edit(current, current_credential, authentication, credentials)
                    .await
            }
        }
    }

    pub fn open(
        configuration: SourceConfiguration,
        credential: Option<String>,
    ) -> SourceResult<Self> {
        let subsonic = match configuration.kind.as_str() {
            "navidrome" | "subsonic" => crate::subsonic::open(&configuration, credential)?,
            kind => {
                return Err(SourceError::InvalidConfig(format!(
                    "unknown source kind {kind}"
                )));
            }
        };
        Ok(Self::new(configuration.source_id, subsonic))
    }

    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    pub fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    pub fn needs_periodic_refresh(&self) -> bool {
        true
    }

    pub async fn acquire_catalog(
        &self,
        database: &Database,
        progress: &(dyn Fn(SourceReadProgress) + Send + Sync),
        cancelled: Arc<AtomicBool>,
        refresh: bool,
    ) -> SourceResult<(bool, Option<Freshness>)> {
        if cancelled.load(Ordering::Acquire) || self.is_retired() {
            return Err(SourceError::Cancelled);
        }
        if !refresh && !self.needs_periodic_refresh() {
            return Ok((false, None));
        }
        let source_id = self.source_id.as_str();
        let freshness = self.freshness().await.ok().flatten();
        let cancelled_fn = || cancelled.load(Ordering::Acquire) || self.is_retired();
        if self.is_retired() || (&cancelled_fn)() {
            return Err(SourceError::Cancelled);
        }
        self.catalog_operation(database, async |changes| {
            if !refresh {
                let accepted = if let Some(marker) = &freshness {
                    database
                        .accept_freshness(source_id, marker, &library::ReadCancellation::new())
                        .await?
                } else {
                    false
                };
                let full = !accepted;
                if !full {
                    let source = &self.subsonic;
                    source
                        .acquire_user_collections(changes, &|_| {}, &cancelled_fn)
                        .await?;
                    return Ok((false, freshness.clone()));
                }
            }
            let stamp = Some(changes.start_observation().await?);
            let report = |value| progress(value);
            let source = &self.subsonic;
            let complete = source
                .acquire_catalog(changes, &report, &cancelled_fn)
                .await?;
            if cancelled_fn() || self.is_retired() {
                return Err(SourceError::Cancelled);
            }
            changes
                .complete_source_observation(
                    stamp.unwrap(),
                    &[],
                    freshness.as_ref().filter(|_| complete),
                )
                .await?;
            Ok((true, freshness.clone()))
        })
        .await
    }

    async fn freshness(&self) -> SourceResult<Option<Freshness>> {
        let source = &self.subsonic;
        source.freshness().await
    }

    pub async fn stream(
        &self,
        database: &Database,
        request: StreamRequest,
    ) -> SourceResult<ResolvedStream> {
        if let Some(stream) = direct_stream(database, &request.media_uri).await? {
            return Ok(stream);
        }
        let object_id = self.track_object_id(&request.media_uri)?;
        let source = &self.subsonic;
        source.resolve_stream(&object_id, request.quality).await
    }

    pub async fn resolve_download(
        &self,
        database: &Database,
        media_uri: &str,
        quality: DownloadQuality,
    ) -> SourceResult<ResolvedDownload> {
        if library::file_media_path(media_uri).is_some() {
            return Ok(ResolvedDownload::encode(
                ResolvedStream::new(media_uri),
                quality,
            ));
        }
        if let Some(stream) = direct_stream(database, media_uri).await? {
            return Ok(ResolvedDownload::encode(stream, quality));
        }
        let object_id = self.track_object_id(media_uri)?;
        let source = &self.subsonic;
        source.resolve_download(&object_id, quality)
    }

    pub async fn image(&self, request: SourceImageRequest) -> SourceResult<ImageBytes> {
        let source = &self.subsonic;
        match request {
            SourceImageRequest::Native { image_ref, size } => {
                source.image_bytes(&image_ref, size).await
            }
        }
    }

    pub async fn live_search(
        &self,
        database: &Database,
        query: &str,
        limit: usize,
    ) -> SourceResult<library::SearchResults> {
        self.catalog_operation(database, async |changes| {
            let source_id = self.source_id.as_str();
            let source = &self.subsonic;
            let batch = source.live_search(source_id, query, limit).await?;
            let tracks = batch
                .tracks
                .iter()
                .map(|v| v.object_id.clone())
                .collect::<Vec<_>>();
            let albums = batch
                .albums
                .iter()
                .map(|v| v.object_id.clone())
                .collect::<Vec<_>>();
            let artists = batch
                .artists
                .iter()
                .map(|v| v.object_id.clone())
                .collect::<Vec<_>>();
            if self.is_retired() {
                return Err(SourceError::Cancelled);
            }
            changes.apply_catalog(&(batch), true).await?;
            let source = changes.source;
            Ok(database
                .search_rows_by_objects(
                    source,
                    None,
                    false,
                    &tracks,
                    &albums,
                    &artists,
                    &library::ReadCancellation::new(),
                )
                .await?)
        })
        .await
    }

    pub async fn set_favorite(
        &self,
        kind: SourceEntityKind,
        object_id: &str,
        favorite: bool,
    ) -> SourceResult<()> {
        let source = &self.subsonic;
        source.set_favorite(kind, object_id, favorite).await
    }

    pub async fn set_rating(&self, object_id: &str, rating: Option<u8>) -> SourceResult<()> {
        if rating.is_some_and(|rating| rating > 10) {
            return Err(SourceError::InvalidRequest("rating outside 0..=10"));
        }
        let source = &self.subsonic;
        source.set_rating(object_id, rating).await
    }

    pub fn supports_playlist_artwork(&self) -> bool {
        let provider = &self.subsonic;
        provider.supports_playlist_artwork()
    }

    pub async fn browse_folder(
        &self,
        _database: &Database,
        folder_object_id: Option<&str>,
        music_folder_object_id: Option<&str>,
    ) -> SourceResult<LiveFolderPage> {
        let source = &self.subsonic;
        source
            .browse_folder(folder_object_id, music_folder_object_id)
            .await
    }

    pub async fn generated_track_object_ids(
        &self,
        seed: &SourceRadioSeed,
        limit: usize,
    ) -> SourceResult<Vec<String>> {
        let source = &self.subsonic;
        source.generated_track_object_ids(seed, limit).await
    }

    pub async fn apply_items(
        &self,
        database: &Database,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<()> {
        self.catalog_operation(database, async |changes| {
            self.acquire_items(changes, upserts, removals).await?;
            self.acquire_item_relations(changes).await
        })
        .await
    }

    pub(crate) async fn apply_changes(
        &self,
        database: &Database,
        items: BTreeMap<String, RemoteItemUpdate>,
    ) -> SourceResult<()> {
        self.catalog_operation(database, async |changes| {
            let (mut upserts, mut removals) = (Vec::new(), Vec::new());
            for (id, change) in items {
                match change {
                    RemoteItemUpdate::Remove => removals.push(id),
                    RemoteItemUpdate::Metadata => upserts.push(id),
                }
            }
            let metadata = async {
                if !upserts.is_empty() || !removals.is_empty() {
                    self.acquire_items(changes, upserts, removals).await?;
                    self.acquire_item_relations(changes).await?;
                }
                Ok(())
            }
            .await;
            metadata
        })
        .await
    }

    pub(crate) async fn acquire_items(
        &self,
        changes: &mut library::Catalog<'_>,
        upserts: Vec<String>,
        removals: Vec<String>,
    ) -> SourceResult<()> {
        {
            let source = &self.subsonic;
            let result = source
                .acquire_items(changes, upserts, false, &|| self.is_retired())
                .await;
            for object in removals {
                if let Some((kind, id)) = object.split_once(':') {
                    let kind = match kind {
                        "song" => "track",
                        "album" => "album",
                        "artist" => "artist",
                        "playlist" => "playlist",
                        _ => continue,
                    };
                    changes
                        .remove_source_items(kind, &[source.id(kind, id)])
                        .await?;
                }
            }
            result
        }
    }

    fn item_request_id(&self, kind: &str, object: &str) -> String {
        let raw = raw_item_id(object);
        format!("{}:{raw}", if kind == "track" { "song" } else { kind })
    }

    pub(crate) async fn acquire_item_relations(
        &self,
        changes: &mut library::Catalog<'_>,
    ) -> SourceResult<()> {
        let database = changes.database;
        let source_id = self.source_id.as_str();
        let section = SourceHomeSection::NewlyAdded;
        match self.home_section(section).await {
            Ok(entries) => {
                let mut missing = Vec::new();
                for entry in &entries {
                    let kind = match entry.kind {
                        library::HomeEntryKind::Album => "album",
                        library::HomeEntryKind::Track => "track",
                        _ => continue,
                    };
                    if !database
                        .source_contains_entity(source_id, kind, &entry.entity_object_id, true)
                        .await?
                    {
                        missing.push(self.item_request_id(kind, &entry.entity_object_id));
                    }
                }
                if !missing.is_empty() {
                    if let Err(error) = self.acquire_items(changes, missing, Vec::new()).await {
                        optional_collection_error(error)?;
                    }
                }
                if self.is_retired() {
                    return Err(SourceError::Cancelled);
                }
                changes
                    .replace_source_home_section(section.id(), None, &(entries))
                    .await?;
            }
            Err(error) => optional_collection_error(error)?,
        }
        Ok(())
    }

    pub async fn prepare_collection(
        &self,
        database: &Database,
        media_uri: &str,
    ) -> SourceResult<()> {
        let Some((source_id, kind, object_id)) = library::source_entity_parts(media_uri) else {
            return Err(SourceError::InvalidRequest("collection URI is unavailable"));
        };
        if source_id != self.source_id {
            return Err(SourceError::InvalidRequest(
                "collection belongs to another source",
            ));
        }
        let collection = match kind.as_str() {
            "album" => SourceCollection::Album(object_id),
            "artist" => SourceCollection::Artist(object_id),
            _ => return Err(SourceError::InvalidRequest("URI is not a collection")),
        };
        self.catalog_operation(database, async |changes| {
            let source = &self.subsonic;
            source
                .acquire_collection(changes, &collection, false, &|| self.is_retired())
                .await?;
            Ok(())
        })
        .await
    }

    pub async fn refresh_home_section(
        &self,
        database: &Database,
        section: SourceHomeSection,
    ) -> SourceResult<()> {
        self.catalog_operation(database, async |catalog| {
            let entries = self.home_section(section).await?;
            if self.is_retired() {
                return Err(SourceError::Cancelled);
            }
            catalog
                .replace_source_home_section(section.id(), None, &entries)
                .await?;
            Ok(())
        })
        .await
    }

    pub async fn home_section(
        &self,
        section: SourceHomeSection,
    ) -> SourceResult<Vec<library::HomeEntryInput>> {
        let source = &self.subsonic;
        source.home_section(section).await
    }

    pub fn start_selected_feed(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        _initial_scan_pending: bool,
    ) -> Arc<SelectedFeed> {
        let (wake, receiver) = async_channel::bounded(1);
        let feed = Arc::new(SelectedFeed {
            source: Arc::clone(self),
            pending: Mutex::new(None),
            wake,
            cancelled: Arc::new(AtomicBool::new(false)),
            remote_task: Mutex::new(None),
            receiver,
        });
        let source = &self.subsonic;
        if source.has_navidrome_library() {
            let producer = Arc::clone(&feed);
            let task = runtime.spawn(async move {
                let source = &producer.source.subsonic;
                if let Err(error) = source
                    .listen_navidrome_changes(|change| {
                        producer.submit(SelectedFeedChange::Remote(change))
                    })
                    .await
                {
                    tracing::warn!(%error, "Navidrome change feed stopped");
                }
            });
            *feed.remote_task.lock().unwrap_or_else(|p| p.into_inner()) = Some(task.abort_handle());
        }
        feed
    }

    pub async fn report_playback(&self, report: &SourceReportFact) -> SourceResult<()> {
        let object_id = self.track_object_id(&report.media_uri)?;
        let source = &self.subsonic;
        source.report_playback(&object_id, report).await
    }
}

async fn direct_stream(
    database: &Database,
    media_uri: &str,
) -> SourceResult<Option<ResolvedStream>> {
    let Some(uri) = library::normalize_direct_media_uri(media_uri) else {
        return Ok(None);
    };
    let uri = match database.playback_access(media_uri).await? {
        Some((uri, _)) => uri,
        None => uri,
    };
    Ok(Some(ResolvedStream::new(uri)))
}

pub(crate) fn require_source_edit(
    current: &SourceConfiguration,
    expected_kind: &str,
) -> SourceResult<()> {
    if current.kind == expected_kind {
        Ok(())
    } else {
        Err(SourceError::InvalidConfig(format!(
            "expected {expected_kind}, found {}",
            current.kind
        )))
    }
}

pub(crate) fn configured_source_name(configured: Option<String>, provider: String) -> String {
    configured
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.trim().to_string())
        .unwrap_or(provider)
}

pub(crate) fn comparable_address(value: &str) -> &str {
    value.trim().trim_end_matches('/')
}

pub(crate) fn edited_source_name(requested: &str, current: &str) -> String {
    let requested = requested.trim();
    if requested.is_empty() {
        current.to_string()
    } else {
        requested.to_string()
    }
}

#[cfg(test)]
mod refresh_laws {
    use super::*;

    #[test]
    fn selected_feed_accumulates_exact_ids_with_one_bound() {
        let merged = SelectedFeedChange::Remote(RemoteItemChange::items(
            vec!["two".to_string(), "one".to_string()],
            Vec::new(),
        ))
        .merge(SelectedFeedChange::Remote(RemoteItemChange::items(
            vec!["one".to_string(), "three".to_string()],
            vec!["gone".to_string()],
        )));
        let SelectedFeedChange::Remote(RemoteItemChange::Items(items)) = merged else {
            panic!("exact evidence widened unexpectedly");
        };
        assert_eq!(
            items,
            BTreeMap::from([
                ("one".into(), RemoteItemUpdate::Metadata),
                ("three".into(), RemoteItemUpdate::Metadata),
                ("two".into(), RemoteItemUpdate::Metadata),
                ("gone".into(), RemoteItemUpdate::Remove),
            ])
        );
    }

    #[test]
    fn selected_feed_overflow_coalesces_to_one_boundary_operation() {
        let merged = SelectedFeedChange::Remote(RemoteItemChange::items(
            (0..LIVE_CHANGE_LIMIT)
                .map(|index| format!("item-{index}"))
                .collect(),
            Vec::new(),
        ))
        .merge(SelectedFeedChange::Remote(RemoteItemChange::items(
            vec!["overflow".to_string()],
            Vec::new(),
        )));
        assert!(matches!(
            merged,
            SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost)
        ));
    }

    #[test]
    fn repeated_boundary_evidence_admits_one_recovery() {
        let (wake, receiver) = async_channel::bounded(1);
        let mut pending = Some(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost));
        pending = Some(
            pending
                .take()
                .expect("first boundary")
                .merge(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost)),
        );
        assert!(wake.try_send(()).is_ok());
        let _ = wake.try_send(());
        assert!(matches!(
            pending.as_ref(),
            Some(SelectedFeedChange::Remote(RemoteItemChange::BoundaryLost))
        ));
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }
}
