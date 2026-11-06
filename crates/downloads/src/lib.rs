//! Download queue ownership, command handling, and bounded transfer execution.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use audio_processing::ResolvedStream;
use library::{Database, FolderKey, SourceKey};
use serde::{Deserialize, Serialize};
use sources::{DownloadQuality, ResolvedDownload};
use sources::{Source, SourceError, SourceId};
use tracing::warn;

mod actor;
mod enqueue;
mod jobs;
mod quality;
mod queue;
mod removal;
mod rules;
mod transfer;
pub use quality::{DOWNLOAD_BITRATES, download_bitrates};
mod storage;
#[cfg(test)]
mod tests;
mod track_download;
pub use storage::DownloadDirectory;

use actor::*;
use queue::*;
use track_download::*;

pub type DownloadCount = usize;
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
const MAX_ACTIVE_DOWNLOADS: usize = 3;

#[derive(Clone)]
pub struct Downloads {
    root: Arc<PathBuf>,
    commands: Sender<Command>,
    status: tokio::sync::watch::Receiver<DownloadStatus>,
}

enum Command {
    ClearArtworkCache(Sender<Result<(), String>>),
    Page {
        scope: DownloadQueueScope,
        offset: u64,
        limit: u32,
        response: Sender<Result<DownloadPage, String>>,
    },
    Attach {
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Weak<Source>>,
        folder: Option<FolderKey>,
        response: Sender<Result<(), String>>,
    },
    Download {
        subject: DownloadSubject,
        media_uris: Vec<String>,
    },
    ConnectChanged,
    Remove {
        media_uris: Vec<String>,
        notify: bool,
    },
    LibraryChanged {
        source_id: SourceId,
    },
    SettingsChanged(Vec<SourceDownloadSettings>),
    RemoveRule {
        source_id: SourceId,
        rule: DownloadRule,
        delete_downloads: bool,
    },
    Cancel {
        source_id: Option<SourceId>,
        job_id: String,
    },
    ClearJob {
        source_id: SourceId,
        job_id: String,
    },
    SetPaused(bool),
    Move {
        source_id: SourceId,
        job_id: String,
        movement: DownloadQueueMove,
    },
    Clear {
        source_id: SourceId,
        notify: bool,
    },
}

#[derive(Clone)]
struct AttachedSource {
    source_key: SourceKey,
    source: Option<Weak<Source>>,
    folder: Option<FolderKey>,
    directory: Option<DownloadDirectory>,
}

#[derive(Clone)]
struct RuleIntent {
    database: Database,
    source_key: SourceKey,
    folder: Option<FolderKey>,
    rules: DownloadRules,
}

impl RuleIntent {
    fn same_context(&self, other: &Self) -> bool {
        self.source_key == other.source_key && self.folder == other.folder
    }
}

fn same_weak_target<T>(left: &Option<Weak<T>>, right: &Option<Weak<T>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Weak::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
enum DownloadOwner {
    Subject(DownloadSubject),
    Retained,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DownloadJob {
    id: String,
    subject: DownloadSubject,
    #[serde(deserialize_with = "quality::deserialize")]
    quality: DownloadQuality,
    #[serde(default)]
    replace_unknown: bool,
    #[serde(default)]
    completed: Vec<String>,
    #[serde(default)]
    failed: bool,
    remaining: VecDeque<String>,
    state: DownloadQueueState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DownloadRule {
    EntireLibrary,
    Favorites,
    AllPlaylists,
    LatestFiveAlbums,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DownloadRules {
    #[serde(default)]
    pub entire_library: bool,
    #[serde(default)]
    pub favorites: bool,
    #[serde(default)]
    pub all_playlists: bool,
    #[serde(default)]
    pub latest_five_albums: bool,
}

impl DownloadRules {
    pub fn is_empty(self) -> bool {
        !self.entire_library && !self.favorites && !self.all_playlists && !self.latest_five_albums
    }

    pub fn contains(self, rule: DownloadRule) -> bool {
        match rule {
            DownloadRule::EntireLibrary => self.entire_library,
            DownloadRule::Favorites => self.favorites,
            DownloadRule::AllPlaylists => self.all_playlists,
            DownloadRule::LatestFiveAlbums => self.latest_five_albums,
        }
    }

    pub fn set(&mut self, rule: DownloadRule, active: bool) {
        match rule {
            DownloadRule::EntireLibrary => self.entire_library = active,
            DownloadRule::Favorites => self.favorites = active,
            DownloadRule::AllPlaylists => self.all_playlists = active,
            DownloadRule::LatestFiveAlbums => self.latest_five_albums = active,
        }
    }

    pub fn active(self) -> impl Iterator<Item = DownloadRule> {
        DownloadRule::ALL
            .into_iter()
            .filter(move |rule| self.contains(*rule))
    }
}

impl DownloadRule {
    pub const ALL: [Self; 4] = [
        Self::EntireLibrary,
        Self::Favorites,
        Self::AllPlaylists,
        Self::LatestFiveAlbums,
    ];
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceDownloadSettings {
    pub source_id: SourceId,
    #[serde(flatten)]
    pub rules: DownloadRules,
    #[serde(default)]
    #[serde(deserialize_with = "quality::deserialize")]
    pub quality: DownloadQuality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<DownloadDirectory>,
}

impl SourceDownloadSettings {
    pub fn for_source(source_id: SourceId) -> Self {
        Self {
            source_id,
            rules: DownloadRules::default(),
            quality: DownloadQuality::Original,
            directory: None,
        }
    }

    pub fn is_default(&self) -> bool {
        self.rules.is_empty()
            && self.quality == DownloadQuality::Original
            && self.directory.is_none()
    }
}

async fn rule_media_uris(
    database: &Database,
    source: SourceKey,
    folder: Option<FolderKey>,
    rule: DownloadRule,
) -> library::LibraryResult<Vec<String>> {
    let cancellation = library::ReadCancellation::new();
    match rule {
        DownloadRule::EntireLibrary | DownloadRule::Favorites => {
            let mut query = library::MemberRead::new(library::Collection::Source(source));
            query.folder = folder;
            query.favorites_only = matches!(rule, DownloadRule::Favorites);
            database
                .selected_track_uris(&query.selection(), None, &cancellation)
                .await
        }
        DownloadRule::AllPlaylists => {
            database
                .all_playlist_track_order(source, folder, &cancellation)
                .await
        }
        DownloadRule::LatestFiveAlbums => {
            database
                .latest_album_track_order(source, folder, 5, &cancellation)
                .await
        }
    }
}

type PreparedRules = Result<Vec<(DownloadRule, Vec<String>)>, String>;

fn prepare_rules(intent: RuleIntent, prepared: Sender<PreparedRules>) {
    tokio::spawn(async move {
        let RuleIntent {
            database,
            source_key,
            folder,
            rules,
        } = intent;
        let mut result = Vec::new();
        for rule in rules.active() {
            match rule_media_uris(&database, source_key, folder, rule).await {
                Ok(media_uris) => result.push((rule, media_uris)),
                Err(error) => {
                    drop(prepared.try_send(Err(error.to_string())));
                    return;
                }
            }
        }
        drop(prepared.try_send(Ok(result)));
    });
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum DownloadSubject {
    Rule(DownloadRule),
    Prepared {
        context_id: String,
        title: Option<String>,
    },
}

impl DownloadSubject {
    pub fn for_media_uris(context: &str, title: Option<&str>, media_uris: &[String]) -> Self {
        let encoded = serde_json::to_vec(media_uris).unwrap_or_default();
        Self::Prepared {
            context_id: format!("{context}:{}", hash_id_bytes(&encoded)),
            title: title.map(str::to_string),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DownloadQueueState {
    Queued,
    Downloading,
    WaitingForConnection,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadQueueItem {
    pub id: String,
    pub source_id: Option<SourceId>,
    pub subject: DownloadSubject,
    pub preview_uris: Vec<String>,
    pub quality: DownloadQuality,
    pub completed_tracks: DownloadCount,
    pub total_tracks: DownloadCount,
    pub state: DownloadQueueState,
}

#[derive(Clone, Debug)]
pub enum DownloadQueueMove {
    Up,
    Down,
    Before { job_id: String },
    After { job_id: String },
}

#[derive(Clone, Debug)]
pub enum DownloadQueueScope {
    All,
    Source { source_id: SourceId },
    Direct,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadFeedbackKind {
    Started,
    Queued,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadFeedback {
    pub subject: DownloadSubject,
    pub preview_uris: Vec<String>,
    pub item_count: usize,
    pub kind: DownloadFeedbackKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadEvent {
    SubjectChanged {
        subject: DownloadSubject,
        downloaded: bool,
    },
    Changed {
        media_uri: String,
        downloaded: bool,
    },
    Queue {
        summary: DownloadQueueSummary,
        paused: bool,
    },
    Feedback(DownloadFeedback),
    Notice(String),
}

struct Actor {
    artwork: artwork::Artwork,
    root: Arc<PathBuf>,
    database: Database,
    events: DownloadEvents,
    transfers: Arc<TransferClients>,
    prepared_rules: Sender<PreparedRules>,
    attached: HashMap<SourceId, AttachedSource>,
    settings: HashMap<SourceId, SourceDownloadSettings>,
    running_rules: Option<RuleIntent>,
    pending_rules: Option<RuleIntent>,
    jobs: HashMap<Option<SourceId>, Vec<DownloadJob>>,
    paused: bool,
    next_job: u64,
}

impl Downloads {
    pub fn default_directory(&self) -> &Path {
        &self.root
    }

    pub fn new(
        artwork: artwork::Artwork,
        root: PathBuf,
        database: Database,
        runtime: tokio::runtime::Handle,
        events: Sender<DownloadEvent>,
        settings: Vec<SourceDownloadSettings>,
    ) -> Self {
        let (commands, receiver) = async_channel::unbounded();
        let (prepared_rules, rule_results) = async_channel::unbounded();
        let (status, changes) = tokio::sync::watch::channel(DownloadStatus::default());
        let downloads = Self {
            root: Arc::new(root),
            commands,
            status: changes,
        };
        runtime.spawn(run(
            Actor {
                artwork,
                root: Arc::clone(&downloads.root),
                database,
                events: DownloadEvents {
                    sender: events,
                    status,
                    counts_dirty: Mutex::new(HashSet::new()),
                },
                transfers: Arc::new(TransferClients::default()),
                prepared_rules,
                attached: HashMap::new(),
                settings: settings
                    .into_iter()
                    .map(|settings| (settings.source_id.clone(), settings))
                    .collect(),
                running_rules: None,
                pending_rules: None,
                jobs: HashMap::new(),
                paused: false,
                next_job: 0,
            },
            receiver,
            rule_results,
        ));
        downloads
    }

    pub async fn attach(
        &self,
        source_id: SourceId,
        source_key: SourceKey,
        source: Option<Arc<Source>>,
        folder: Option<FolderKey>,
    ) -> Result<(), String> {
        let (response, result) = async_channel::bounded(1);
        self.commands
            .send(Command::Attach {
                source_id,
                source_key,
                source: source.as_ref().map(Arc::downgrade),
                folder,
                response,
            })
            .await
            .map_err(|_| "download operation lane is unavailable".to_string())?;
        result
            .recv()
            .await
            .map_err(|_| "download attachment did not finish".to_string())?
    }

    pub fn download(&self, subject: DownloadSubject, media_uris: Vec<String>) {
        self.send(Command::Download {
            subject,
            media_uris,
        });
    }

    pub fn remove(&self, media_uris: Vec<String>, notify: bool) {
        self.send(Command::Remove { media_uris, notify });
    }

    pub fn connect_changed(&self) {
        self.send(Command::ConnectChanged);
    }

    pub fn library_changed(&self, source_id: SourceId) {
        self.send(Command::LibraryChanged { source_id });
    }

    pub fn settings_changed(&self, settings: Vec<SourceDownloadSettings>) {
        self.send(Command::SettingsChanged(settings));
    }

    pub fn remove_rule(&self, source_id: SourceId, rule: DownloadRule, delete_downloads: bool) {
        self.send(Command::RemoveRule {
            source_id,
            rule,
            delete_downloads,
        });
    }

    pub fn cancel(&self, source_id: SourceId, job_id: String) {
        self.send(Command::Cancel {
            source_id: Some(source_id),
            job_id,
        });
    }

    pub fn clear_job(&self, source_id: SourceId, job_id: String) {
        self.send(Command::ClearJob { source_id, job_id });
    }

    pub fn set_paused(&self, paused: bool) {
        self.send(Command::SetPaused(paused));
    }

    pub fn move_job(&self, source_id: SourceId, job_id: String, movement: DownloadQueueMove) {
        self.send(Command::Move {
            source_id,
            job_id,
            movement,
        });
    }

    pub fn clear(&self, source_id: SourceId, notify: bool) {
        self.send(Command::Clear { source_id, notify });
    }

    fn send(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            warn!("download operation lane is unavailable");
        }
    }
}

#[derive(Clone, Default)]
pub struct DownloadStatus {
    pub revision: u64,
    pub queue_revision: u64,
    pub paused: bool,
    pub queues: Vec<DownloadQueueSummary>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadQueueSummary {
    pub source_id: Option<SourceId>,
    pub downloaded_tracks: u64,
    pub total_jobs: u64,
}

pub struct DownloadPage {
    pub total: u64,
    pub jobs: Vec<DownloadQueueItem>,
    pub previews: Vec<library::TrackView>,
}

struct DownloadEvents {
    sender: Sender<DownloadEvent>,
    status: tokio::sync::watch::Sender<DownloadStatus>,
    counts_dirty: Mutex<HashSet<Option<SourceId>>>,
}

impl DownloadEvents {
    fn update(&self, event: &DownloadEvent) {
        self.status.send_if_modified(|state| {
            match event {
                DownloadEvent::Changed { .. } => state.revision += 1,
                DownloadEvent::Notice(message) => state.notice = Some(message.clone()),
                DownloadEvent::Queue { summary, paused } => {
                    state.paused = *paused;
                    state.queue_revision += 1;
                    if let Some(previous) = state
                        .queues
                        .iter_mut()
                        .find(|queue| queue.source_id == summary.source_id)
                    {
                        *previous = summary.clone();
                    } else {
                        state.queues.push(summary.clone());
                        state.queues.sort_by(|a, b| a.source_id.cmp(&b.source_id));
                    }
                }
                DownloadEvent::Feedback(_) | DownloadEvent::SubjectChanged { .. } => return false,
            }
            true
        });
    }
    async fn send(
        &self,
        event: DownloadEvent,
    ) -> Result<(), async_channel::SendError<DownloadEvent>> {
        self.update(&event);
        self.sender.send(event).await
    }
    fn try_send(
        &self,
        event: DownloadEvent,
    ) -> Result<(), async_channel::TrySendError<DownloadEvent>> {
        self.update(&event);
        self.sender.try_send(event)
    }
}

impl DownloadJob {
    fn queue_item(&self, source_id: Option<SourceId>) -> DownloadQueueItem {
        DownloadQueueItem {
            id: self.id.clone(),
            source_id,
            subject: self.subject.clone(),
            preview_uris: self
                .completed
                .iter()
                .chain(&self.remaining)
                .take(4)
                .cloned()
                .collect(),
            quality: self.quality,
            completed_tracks: self.completed.len(),
            total_tracks: self.completed.len() + self.remaining.len(),
            state: self.state,
        }
    }
}

impl Downloads {
    pub async fn clear_artwork_cache(&self) -> sources::SourceResult<()> {
        let (response, receive) = async_channel::bounded(1);
        self.send(Command::ClearArtworkCache(response));
        receive
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }

    pub fn subscribe(&self) -> Arc<DownloadSubscription> {
        let mut changes = self.status.clone();
        changes.mark_changed();
        Arc::new(DownloadSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
    pub fn status(&self) -> DownloadStatus {
        self.status.borrow().clone()
    }

    pub async fn queue_page(
        &self,
        scope: DownloadQueueScope,
        offset: u64,
        limit: u32,
    ) -> sources::SourceResult<DownloadPage> {
        let (response, receive) = async_channel::bounded(1);
        self.send(Command::Page {
            scope,
            offset,
            limit,
            response,
        });
        receive
            .recv()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?
            .map_err(SourceError::Other)
    }
}

pub struct DownloadSubscription {
    changes: tokio::sync::Mutex<tokio::sync::watch::Receiver<DownloadStatus>>,
}

impl DownloadSubscription {
    pub async fn next(&self) -> sources::SourceResult<DownloadStatus> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|error| SourceError::Other(error.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}
