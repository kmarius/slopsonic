//! Provider operations write canonical catalog data directly.
//! Source owns native client lifetimes and publishes committed changes.

use std::collections::BTreeMap;
use std::path::PathBuf;
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
        let cue = library::cue_media_parts(media_uri);
        let backing = cue
            .as_ref()
            .map_or(media_uri, |(_, uri, _, _)| uri.as_str());
        if library::file_media_path(backing).is_some() {
            return Ok(ResolvedDownload::encode(
                ResolvedStream::new(backing),
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

    pub async fn apply_local_mapping(
        &self,
        database: &Database,
        source: library::SourceKey,
        root: &std::path::Path,
        server_prefix: Option<&str>,
        local_prefix: Option<&str>,
        sample_source_path: Option<&str>,
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<(PathBuf, Option<String>)> {
        if cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        let sample = database
            .mapping_track_page(
                source,
                None,
                sample_source_path,
                1,
                &library::ReadCancellation::new(),
            )
            .await?
            .pop()
            .ok_or(SourceError::InvalidRequest(
                "No representative Track matches this mapping",
            ))?;
        let root = root.to_path_buf();
        let server_prefix = server_prefix.map(str::to_owned);
        let local_prefix = local_prefix.map(str::to_owned);
        let (root, server_prefix, access) = tokio::task::spawn_blocking(move || {
            let root = mapping_root(&root)?;
            let server_prefix = match_local_access_sample(
                &root,
                server_prefix.as_deref(),
                local_prefix.as_deref(),
                &sample.source_path,
            )
            .ok_or(SourceError::InvalidRequest(
                "The representative Track does not map to a local file",
            ))?;
            let access = mapped_track_access(
                &root,
                server_prefix.as_deref(),
                local_prefix.as_deref(),
                sample,
            )?
            .ok_or(SourceError::InvalidRequest(
                "The representative Track does not map to a local file",
            ))?;
            Ok::<_, SourceError>((root, server_prefix, access))
        })
        .await
        .map_err(|error| SourceError::Other(error.to_string()))??;
        if cancelled.load(Ordering::Acquire) {
            return Err(SourceError::Cancelled);
        }
        database.clear_mapping_access(source).await?;
        database
            .write_local_access(Some(source), &[&access])
            .await?;
        Ok((root, server_prefix))
    }

    pub async fn complete_local_mapping(
        &self,
        database: &Database,
        source: library::SourceKey,
        root: &std::path::Path,
        server_prefix: Option<&str>,
        local_prefix: Option<&str>,
        cancelled: Arc<AtomicBool>,
    ) -> SourceResult<usize> {
        let root = root.to_path_buf();
        let mut after = None;
        let mut accepted = 0;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(SourceError::Cancelled);
            }
            let tracks = database
                .mapping_track_page(source, after, None, 128, &library::ReadCancellation::new())
                .await?;
            if tracks.is_empty() {
                break;
            }
            after = tracks.last().map(|track| track.track_key);
            let root = root.clone();
            let server_prefix = server_prefix.map(str::to_owned);
            let local_prefix = local_prefix.map(str::to_owned);
            let worker_cancelled = Arc::clone(&cancelled);
            let accesses = tokio::task::spawn_blocking(move || {
                let mut accesses = Vec::new();
                for track in tracks {
                    if worker_cancelled.load(Ordering::Acquire) {
                        return Err(SourceError::Cancelled);
                    }
                    if let Some(access) = mapped_track_access(
                        &root,
                        server_prefix.as_deref(),
                        local_prefix.as_deref(),
                        track,
                    )? {
                        accesses.push(access);
                    }
                }
                Ok(accesses)
            })
            .await
            .map_err(|error| SourceError::Other(error.to_string()))??;
            if cancelled.load(Ordering::Acquire) {
                return Err(SourceError::Cancelled);
            }
            database
                .write_local_access(Some(source), &accesses.iter().collect::<Vec<_>>())
                .await?;
            accepted += accesses.len();
        }
        Ok(accepted)
    }
}

async fn direct_stream(
    database: &Database,
    media_uri: &str,
) -> SourceResult<Option<ResolvedStream>> {
    let cue = library::cue_media_parts(media_uri);
    let backing = cue
        .as_ref()
        .map_or(media_uri, |(_, uri, _, _)| uri.as_str());
    let Some(uri) = library::normalize_direct_media_uri(backing) else {
        return Ok(None);
    };
    let mut access = database.playback_access(media_uri).await?;
    if access.is_none() && cue.is_some() {
        access = database.playback_access(backing).await?;
    }
    let uri = match access {
        Some((uri, _)) => uri,
        None => uri,
    };
    let stream = ResolvedStream::new(uri);
    if cue.is_some()
        && let Some((start, end)) = database.playback_window(media_uri).await?
    {
        return Ok(Some(stream.with_window(start, end)));
    }
    Ok(Some(stream))
}

fn mapping_root(root: &std::path::Path) -> SourceResult<PathBuf> {
    std::fs::canonicalize(root).map_err(|error| SourceError::Other(error.to_string()))
}

fn mapping_path_exists(
    root: &std::path::Path,
    server: Option<&str>,
    local: Option<&str>,
    path: &str,
) -> bool {
    mapped_local_path(root, server, local, path).is_some()
}

fn mapped_track_access(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    track: library::MappingTrackRow,
) -> SourceResult<Option<library::LocalAccessWrite>> {
    let Some(path) = mapped_local_path(root, server_prefix, local_prefix, &track.source_path)
    else {
        return Ok(None);
    };
    let metadata =
        std::fs::metadata(&path).map_err(|error| SourceError::Other(error.to_string()))?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|value| i64::try_from(value.as_nanos()).ok())
        .unwrap_or_default();
    #[cfg(unix)]
    let (device_id, inode) = {
        use std::os::unix::fs::MetadataExt;
        (
            i64::try_from(metadata.dev()).ok(),
            i64::try_from(metadata.ino()).ok(),
        )
    };
    #[cfg(not(unix))]
    let (device_id, inode) = (None, None);
    let access_uri = url::Url::from_file_path(&path)
        .map_err(|()| SourceError::Other("could not create mapped file URI".to_string()))?
        .to_string();
    let mut hash = blake3::Hasher::new();
    hash.update(path.to_string_lossy().as_bytes());
    hash.update(&metadata.len().to_le_bytes());
    hash.update(&mtime_ns.to_le_bytes());
    Ok(Some(library::LocalAccessWrite {
        media_uri: track.media_uri,
        origin: library::LocalAccessOrigin::Mapping,
        path: path.to_string_lossy().into_owned(),
        root: root.to_string_lossy().into_owned(),
        relative_path: path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned(),
        size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        mtime_ns,
        device_id,
        inode,
        parser_version: 1,
        access_uri,
        loudness_analysis_key: Some(*hash.finalize().as_bytes()),
    }))
}

fn mapped_local_path(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<PathBuf> {
    let candidate = project_local_access_path(root, server_prefix, local_prefix, source_path)?;
    mapped_file(root, candidate)
}

fn match_local_access_sample(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<Option<String>> {
    let root = mapping_root(root).ok()?;
    let server_prefix = server_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty());
    if mapping_path_exists(&root, server_prefix, local_prefix, source_path) {
        return Some(server_prefix.map(str::to_string));
    }
    if server_prefix.is_some() {
        return None;
    }
    let inferred = infer_server_prefix(&root, local_prefix, source_path)?;
    mapping_path_exists(&root, Some(&inferred), local_prefix, source_path).then_some(Some(inferred))
}

fn infer_server_prefix(
    root: &std::path::Path,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<String> {
    let starts = std::iter::once(0).chain(
        source_path
            .char_indices()
            .filter(|(_, character)| *character == '/' || *character == '\\')
            .map(|(index, character)| index + character.len_utf8()),
    );
    for start in starts {
        let suffix = source_path.get(start..)?.trim_start_matches(['/', '\\']);
        if suffix.is_empty() {
            continue;
        }
        let prefix = source_path.get(..start)?.trim_end_matches(['/', '\\']);
        let prefix = if prefix.is_empty() {
            source_path
                .chars()
                .next()
                .filter(|character| *character == '/' || *character == '\\')
                .map_or_else(String::new, |character| character.to_string())
        } else {
            prefix.to_string()
        };
        if mapping_path_exists(root, Some(&prefix), local_prefix, source_path) {
            return Some(prefix);
        }
    }
    None
}

/// Applies one saved server-prefix/local-prefix formula to a reported source path.
///
/// File existence and containment within `root` remain the caller's responsibility.
pub fn project_local_access_path(
    root: &std::path::Path,
    server_prefix: Option<&str>,
    local_prefix: Option<&str>,
    source_path: &str,
) -> Option<PathBuf> {
    let mut base = root.to_path_buf();
    if let Some(prefix) = local_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    {
        base.push(prefix);
    }
    if let Some(prefix) = server_prefix
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
    {
        let relative = source_path
            .strip_prefix(prefix)?
            .trim_start_matches(['/', '\\']);
        return Some(base.join(relative.replace('\\', "/")));
    }
    if reported_path_is_absolute(source_path) {
        Some(PathBuf::from(source_path))
    } else {
        Some(base.join(source_path.replace('\\', "/")))
    }
}

pub fn reported_path_is_absolute(value: &str) -> bool {
    std::path::Path::new(value).is_absolute()
        || value.as_bytes().get(1) == Some(&b':')
        || value.starts_with("\\\\")
}

fn mapped_file(root: &std::path::Path, candidate: PathBuf) -> Option<PathBuf> {
    let path = std::fs::canonicalize(candidate).ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
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

    #[test]
    fn one_prefix_formula_projects_navidrome_and_subsonic_paths() {
        let root = tempfile::tempdir().expect("local music root");
        let track = root.path().join("Artist").join("Album").join("Track.flac");
        std::fs::create_dir_all(track.parent().expect("Track parent")).expect("create Album");
        std::fs::write(&track, b"media").expect("write Track");
        let canonical_root = root.path().canonicalize().expect("canonical music root");
        let canonical_track = track.canonicalize().expect("canonical Track");

        assert_eq!(
            mapped_local_path(
                &canonical_root,
                Some("/srv/navidrome/music"),
                None,
                "/srv/navidrome/music/Artist/Album/Track.flac",
            ),
            Some(canonical_track.clone())
        );
        assert_eq!(
            match_local_access_sample(
                &canonical_root,
                None,
                None,
                "/srv/navidrome/music/Artist/Album/Track.flac",
            ),
            Some(Some("/srv/navidrome/music".to_string()))
        );
        assert_eq!(
            project_local_access_path(
                &canonical_root,
                Some("/media/music"),
                None,
                "/media/music/Artist/Album/Track.flac",
            ),
            Some(canonical_track.clone())
        );
        assert_eq!(
            project_local_access_path(&canonical_root, None, None, "Artist/Album/Track.flac",),
            Some(canonical_track)
        );
        assert_eq!(
            match_local_access_sample(&canonical_root, None, None, "Artist/Album/Track.flac",),
            Some(None)
        );
    }

    #[test]
    fn mapping_without_a_prefix_does_not_search_for_a_different_formula_per_track() {
        let root = tempfile::tempdir().expect("local music root");
        let track = root.path().join("Artist").join("Album").join("Track.flac");
        std::fs::create_dir_all(track.parent().expect("Track parent")).expect("create Album");
        std::fs::write(&track, b"media").expect("write Track");
        let canonical_root = root.path().canonicalize().expect("canonical music root");

        assert_eq!(
            mapped_local_path(
                &canonical_root,
                None,
                None,
                "/different/server/root/Artist/Album/Track.flac",
            ),
            None
        );
    }
}
