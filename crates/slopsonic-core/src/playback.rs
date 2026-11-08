//! Playback controls, source jobs, and persistent playback facts.
pub(crate) mod actor;
mod commands;
pub mod equalizer;
pub use equalizer::{
    EqEdit, EqPresetFormat, EqPresetSummary, EqSubscription, EqView, EqualizerPreferences,
};
mod queue;
pub(crate) mod state;
pub use commands::*;
#[cfg(test)]
mod queue_tests;

pub fn seek_preview_matches_position(target_millis: u64, position_millis: u64) -> bool {
    const TOLERANCE_MILLIS: u64 = 1_500;
    (target_millis.saturating_sub(TOLERANCE_MILLIS)
        ..=target_millis.saturating_add(TOLERANCE_MILLIS))
        .contains(&position_millis)
}

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use actor::{Playback, WeakPlayback};
use library::QueueUpdate;
use library::{Database, ListenWrite, ReadCancellation};
use playback::{
    PlaybackBackend, PlaybackProjection, PreparedStream, RepeatMode, RunId, StreamRequest,
};
use queue::Sequence;
use scrobbling::Scrobbler;
use state::{AutoDjRequest, ClockSample, Command};
use tracing::{debug, warn};

use crate::settings::SettingsFile;
use crate::source::SourceOwner;

pub struct PlaybackOwner {
    database: Arc<Database>,
    settings: SettingsFile,
    runtime: tokio::runtime::Handle,
    pub updates: playback::PlaybackUpdates,
    scrobbler: Arc<Scrobbler>,
    source: Mutex<std::sync::Weak<SourceOwner>>,
    active: Mutex<Option<Playback>>,
    pending_queue: Mutex<Option<QueueUpdate>>,
    equalizer: Mutex<equalizer::EqualizerState>,
    store_sender: async_channel::Sender<PlaybackStoreWork>,
    report_sender: async_channel::Sender<(sources::SourceId, playback::SourceReportFact)>,
    monotonic_origin: Instant,
    start_backend: Box<
        dyn Fn(
                tokio::sync::watch::Receiver<playback::ProcessingRequest>,
            ) -> Result<Box<dyn PlaybackBackend>, String>
            + Send
            + Sync,
    >,
}

enum PlaybackStoreWork {
    Activity(library::TrackKey, Box<ListenWrite>),
    Flush(std::sync::mpsc::SyncSender<()>),
    Output {
        volume: f64,
        muted: bool,
    },
    Settings,
    Equalizer {
        snapshot: EqualizerPreferences,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

fn enqueue_queue_save(
    pending: &Mutex<Option<QueueUpdate>>,
    sender: &async_channel::Sender<PlaybackStoreWork>,
    update: QueueUpdate,
) {
    let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(current) = pending.as_mut() {
        current.coalesce(update);
    } else {
        *pending = Some(update);
        let _ = sender.try_send(PlaybackStoreWork::Settings);
    }
}

async fn save_pending_queue(
    database: &Database,
    pending: &Mutex<Option<QueueUpdate>>,
    failed: &mut Option<QueueUpdate>,
) -> library::LibraryResult<()> {
    let pending = pending.lock().unwrap_or_else(|p| p.into_inner()).take();
    let update = match (failed.take(), pending) {
        (Some(mut older), Some(newer)) => {
            older.coalesce(newer);
            older
        }
        (Some(update), None) | (None, Some(update)) => update,
        (None, None) => return Ok(()),
    };
    if let Err(error) = database.apply_queue_update(&update).await {
        *failed = Some(update);
        return Err(error);
    }
    Ok(())
}

impl PlaybackOwner {
    pub(crate) fn insert_load(&self, input: library::QueueLoad, target: QueueReorderTarget) {
        self.send(Command::Insert { input, target });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<StartBackend>(
        database: Arc<Database>,
        settings: SettingsFile,
        runtime: tokio::runtime::Handle,
        scrobbler: Arc<Scrobbler>,
        start_backend: StartBackend,
    ) -> Arc<Self>
    where
        StartBackend: Fn(
                tokio::sync::watch::Receiver<playback::ProcessingRequest>,
            ) -> Result<Box<dyn PlaybackBackend>, String>
            + Send
            + Sync
            + 'static,
    {
        let stored = settings.load();
        let eq = equalizer::EqualizerState::new(stored.equalizer);
        let (store_sender, store_receiver) = async_channel::unbounded();
        let (report_sender, report_receiver) = async_channel::unbounded();
        let owner = Arc::new(Self {
            database,
            settings,
            runtime: runtime.clone(),
            updates: playback::PlaybackUpdates::default(),
            scrobbler,
            source: Mutex::new(std::sync::Weak::new()),
            active: Mutex::new(None),
            pending_queue: Mutex::new(None),
            equalizer: Mutex::new(eq),
            store_sender,
            report_sender,
            monotonic_origin: Instant::now(),
            start_backend: Box::new(start_backend),
        });
        let weak = Arc::downgrade(&owner);
        owner.runtime.spawn(async move {
            while let Ok((source_id, report)) = report_receiver.recv().await {
                let Some(owner) = weak.upgrade() else { break };
                let source = owner.source_owner();
                if let Ok(Some(source)) =
                    tokio::task::spawn_blocking(move || source?.client(&source_id).ok()).await
                    && let Err(error) = source.report_playback(&report).await
                {
                    debug!(%error, "could not report playback to source");
                }
            }
        });
        let weak = Arc::downgrade(&owner);
        owner.runtime.spawn(async move {
            let mut failed_queue_save = None;
            while let Ok(work) = store_receiver.recv().await {
                let Some(owner) = weak.upgrade() else { break };
                owner.consume_store(work, &mut failed_queue_save).await;
            }
        });
        owner
    }

    pub(crate) fn install_source_owner(&self, source: &Arc<SourceOwner>) {
        *self
            .source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(source);
    }

    pub(crate) async fn start(self: &Arc<Self>) -> Result<PlaybackProjection, String> {
        let stored = self.settings.load();
        let sequence = match self.database.restore_queue().await {
            Ok(mut restore) => {
                if restore.entries.is_empty() {
                    restore.repeat_mode = stored.ui.repeat_mode;
                    restore.shuffled = stored.ui.shuffle_enabled;
                }
                Sequence::from_window(restore, 0, 0).map_err(string_error)
            }
            Err(error) => Err(string_error(error)),
        }.unwrap_or_else(|error| {
            warn!(%error, "could not restore optional Queue; starting with an empty playback window");
            Sequence::new()
        });
        let owner = Arc::downgrade(self);
        let clock_owner = Arc::downgrade(self);
        let backend_owner = Arc::clone(self);
        let backend = tokio::task::spawn_blocking(move || {
            let receiver = backend_owner.processing_receiver();
            (backend_owner.start_backend)(receiver)
        })
        .await
        .map_err(string_error)??;
        let (playback, projection) = Playback::start(
            sequence,
            random_identity(),
            stored.ui.playback,
            stored.ui.auto_dj_enabled,
            usize::from(stored.ui.application.auto_dj_refill_threshold),
            backend,
            Arc::new(move || {
                clock_owner
                    .upgrade()
                    .map_or_else(empty_clock_sample, |owner| owner.clock_sample())
            }),
            owner,
        )
        .map_err(string_error)?;
        *self.active.lock().unwrap_or_else(|p| p.into_inner()) = Some(playback.clone());
        Ok(projection)
    }

    pub(crate) async fn forget_source(&self, source: library::SourceKey) -> Result<(), String> {
        let Some(active) = self.active() else {
            return Ok(());
        };
        let queue = active.handoff_snapshot().map_err(string_error)?.0;
        let positions = self
            .database
            .queue_positions_for_source(&queue, source)
            .await
            .map_err(string_error)?;
        if !positions.is_empty() {
            let keys = positions
                .into_iter()
                .map(|position| queue.entries[queue.order[position] as usize].track_key)
                .collect();
            active
                .command(Command::Forget(keys))
                .map_err(string_error)?;
        }
        Ok(())
    }

    fn take_active(&self) -> Option<Playback> {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.updates.clear();
        active
    }

    fn publish(&self, projection: PlaybackProjection) {
        self.updates.publish(projection);
    }

    fn save_queue(&self, state: QueueUpdate) {
        enqueue_queue_save(&self.pending_queue, &self.store_sender, state);
    }

    fn persist_output(&self, volume: f64, muted: bool) {
        let _ = self
            .store_sender
            .try_send(PlaybackStoreWork::Output { volume, muted });
    }

    async fn consume_store(
        &self,
        work: PlaybackStoreWork,
        failed_queue_save: &mut Option<QueueUpdate>,
    ) {
        match work {
            PlaybackStoreWork::Equalizer { snapshot, reply } => {
                let result = self.settings.update(|stored| {
                    stored.equalizer = snapshot;
                    Ok(())
                });
                self.equalizer_write_finished(&result);
                let _ = reply.send(result);
            }
            PlaybackStoreWork::Activity(key, activity) => self.store_activity(key, *activity).await,
            PlaybackStoreWork::Flush(reply) => {
                if let Err(error) =
                    save_pending_queue(&self.database, &self.pending_queue, failed_queue_save).await
                {
                    warn!(%error, "could not persist Queue");
                }
                let _ = reply.send(());
            }
            PlaybackStoreWork::Output { volume, muted } => {
                let _ = self.settings.update(|stored| {
                    stored.ui.playback.volume = volume;
                    stored.ui.playback.muted = muted;
                    Ok(())
                });
            }
            PlaybackStoreWork::Settings => {
                if let Err(error) =
                    save_pending_queue(&self.database, &self.pending_queue, failed_queue_save).await
                {
                    warn!(%error, "could not persist Queue");
                }
            }
        }
    }

    fn record_activity(&self, key: library::TrackKey, activity: Box<ListenWrite>) {
        let _ = self
            .store_sender
            .try_send(PlaybackStoreWork::Activity(key, activity));
    }

    fn report_source(&self, report: playback::SourceReportFact) {
        if let Some((source_id, kind, _)) = library::source_entity_parts(&report.media_uri)
            && kind == "track"
        {
            let _ = self.report_sender.try_send((source_id, report));
        }
    }

    fn request_auto_dj(&self, playback: WeakPlayback, request: AutoDjRequest) {
        let database = self.database.clone();
        let source = self.source_owner();
        let selected = source.as_ref().and_then(|owner| owner.selected_library());
        let source = source.as_ref().map(Arc::downgrade).unwrap_or_default();
        self.runtime.spawn(async move {
            let candidates = crate::radio::radio_candidates(
                &database,
                source,
                selected,
                library::RadioSeed::Track(request.seed_media_uri.clone()),
                request.requested_count,
                &library::ReadCancellation::new(),
            )
            .await;
            playback.post(move |player, sample| match candidates {
                Ok(candidates) => {
                    if let Err(error) = player.complete_auto_dj_candidates(
                        request.seed_run,
                        &request.seed_media_uri,
                        candidates,
                        request.requested_count,
                        sample,
                    ) {
                        player.report_error(error.to_string());
                    }
                }
                Err(error) => {
                    player.auto_dj_unavailable(
                        request.seed_run,
                        &request.seed_media_uri,
                        Some(error),
                    );
                }
            });
        });
    }

    async fn store_activity(&self, current_track: library::TrackKey, listen: ListenWrite) {
        let deliveries = self.scrobbler.listen_delivery_targets(&listen);
        match self
            .database
            .record_listen(&listen, Some(current_track), &deliveries)
            .await
        {
            Ok(_) => self.scrobbler.listen_recorded(deliveries.len()),
            Err(error) => warn!(%error, "could not record Slopsonic Activity"),
        }
    }

    fn resolve_stream(
        &self,
        playback: WeakPlayback,
        run: RunId,
        occurrence: Arc<playback::QueueRow>,
        request: StreamRequest,
        normalization: playback::LoudnessNormalization,
    ) -> tokio::task::JoinHandle<()> {
        let database = Arc::clone(&self.database);
        let source_owner = self.source_owner();
        self.runtime.spawn(async move {
            let loudness =
                cached_loudness(&database, &occurrence.track.media_uri, normalization).await;
            let result = prepare_stream(&database, request, move |source_id| async move {
                tokio::task::spawn_blocking(move || {
                    source_owner
                        .ok_or_else(crate::source::source_access_unavailable)?
                        .client(&source_id)
                })
                .await
                .map_err(string_error)?
            })
            .await;
            let result = match result {
                Ok(stream) if !local_stream_available(stream.uri()).await => {
                    let kind = playback::FailureKind::UnavailableMedia;
                    Err(playback::BackendFailure::with_kind(kind, kind.summary()))
                }
                Ok(stream) => Ok(prepare_media_stream(stream, loudness, occurrence)),
                Err(error) => Err(error),
            };
            playback.post(move |player, sample| match result {
                Ok(stream) => player.stream_resolved(run, stream),
                Err(error) => player.stream_failed(run, error, sample),
            });
        })
    }

    fn resolve_loudness(
        &self,
        playback: WeakPlayback,
        run: RunId,
        mut stream: PreparedStream,
    ) -> tokio::task::JoinHandle<()> {
        let database = self.database.clone();
        self.runtime.spawn(async move {
            stream.loudness = cached_loudness(
                &database,
                &stream
                    .row
                    .as_ref()
                    .expect("prepared queue entry")
                    .track
                    .media_uri,
                playback::LoudnessNormalization::ReplayGain,
            )
            .await;
            playback.post(move |player, _| player.loudness_resolved(run, stream));
        })
    }

    pub(crate) fn playback_settings_changed(&self, settings: playback::PlaybackSettings) {
        self.send(Command::UpdateSettings(settings));
    }
    pub(crate) fn auto_dj_threshold_changed(&self, enabled: bool, threshold: u8) {
        self.send(Command::SetAutoDj {
            enabled,
            refill_threshold: usize::from(threshold),
        });
    }
    pub(crate) fn rekey_tracks(&self, pairs: Arc<[(library::TrackKey, library::TrackKey)]>) {
        self.send(Command::RekeyTracks(pairs));
    }
    pub(crate) fn catalog_changed(&self) {
        if self.active().is_some() {
            self.send(Command::CatalogChanged);
        }
    }
    pub(crate) fn current_media(&self) -> Option<Arc<playback::CurrentMedia>> {
        self.updates
            .current()
            .and_then(|view| view.transport.current.clone())
            .map(Arc::new)
    }

    fn active(&self) -> Option<Playback> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn source_owner(&self) -> Option<Arc<SourceOwner>> {
        self.source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .upgrade()
    }
    fn send(&self, command: Command) {
        if let Some(active) = self.active() {
            active.downgrade().post(move |player, sample| {
                if let Err(error) = player.handle_command(command, sample) {
                    player.report_error(error.to_string());
                }
            });
        }
    }

    fn clock_sample(&self) -> ClockSample {
        let now = SystemTime::now();
        let unix_seconds = unix_seconds(now);
        ClockSample {
            monotonic_millis: self
                .monotonic_origin
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            unix_seconds,
            local_period: local_calendar_period(unix_seconds),
        }
    }
}

impl PlaybackOwner {
    pub fn snapshot(&self) -> Result<library::QueueRestore, String> {
        let Some(active) = self.active() else {
            return Ok(library::QueueRestore::default());
        };
        active
            .handoff_snapshot()
            .map(|(queue, _, _)| queue)
            .map_err(string_error)
    }

    pub fn play_selection(&self, request: PlayRequest) {
        let Some(active) = self.active() else {
            return;
        };
        active.downgrade().post(move |player, sample| {
            if let Err(error) = player.play(request, sample) {
                player.report_error(error.to_string());
            }
        });
    }
    pub fn insert(&self, input: library::MemberSelection, target: QueueReorderTarget) {
        self.insert_load(library::QueueLoad::tracks(input), target);
    }
}

impl PlaybackOwner {
    pub fn remove(&self, position: usize) {
        self.send(Command::Remove(position));
    }
    pub fn remove_many(&self, selection: QueueSelection) {
        self.send(Command::RemoveMany(selection));
    }
    pub fn activate(&self, position: usize) {
        self.send(Command::Activate(position));
    }
    pub fn activate_occurrence(&self, id: u64) {
        self.send(Command::ActivateOccurrence(id));
    }
    pub fn move_occurrence_after_current(&self, id: u64) {
        self.send(Command::MoveOccurrenceAfterCurrent(id));
    }
    pub fn move_after_current(&self, position: usize) {
        self.send(Command::MoveAfterCurrent(position));
    }
    pub fn reorder(&self, selection: QueueSelection, target: QueueReorderTarget) {
        self.send(Command::Reorder { selection, target });
    }
    pub fn clear(&self, include_current: bool) {
        self.send(Command::Clear { include_current });
    }
}

impl PlaybackOwner {
    pub fn candidates(
        &self,
        seed: library::RadioSeed,
        requested: usize,
        cancellation: library::ReadCancellation,
    ) -> impl std::future::Future<Output = Result<Vec<library::TrackKey>, String>> + Send + 'static + use<>
    {
        let database = self.database.clone();
        let source = self
            .source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let selected = source.upgrade().and_then(|owner| owner.selected_library());
        async move {
            crate::radio::radio_candidates(
                &database,
                source,
                selected,
                seed,
                requested,
                &cancellation,
            )
            .await
        }
    }

    pub fn play_random(&self, request: RandomPlayRequest) {
        self.send(Command::Random(request));
    }
    pub fn play_radio(&self, request: RadioPlayRequest) {
        self.send(Command::Radio(request));
    }
}

impl PlaybackOwner {
    pub fn play_pause(&self) {
        self.send(Command::PlayPause)
    }
    pub fn play(&self) {
        self.send(Command::Play)
    }
    pub fn pause(&self) {
        self.send(Command::Pause)
    }
    pub fn stop(&self) {
        self.send(Command::Stop)
    }
    pub fn next(&self) {
        self.send(Command::Next)
    }
    pub fn previous(&self) {
        self.send(Command::Previous)
    }
    pub fn seek_seconds(&self, seconds: u32) {
        self.seek_millis(u64::from(seconds) * 1000)
    }
    pub fn seek_millis(&self, millis: u64) {
        self.send(Command::Seek(millis))
    }
    pub fn set_volume(&self, volume: f64) {
        self.send(Command::SetVolume(volume))
    }
    pub fn persist_volume(&self, volume: f64) {
        self.send(Command::PersistVolume(volume))
    }
    pub fn set_muted(&self, muted: bool) {
        self.send(Command::SetMuted(muted))
    }
    pub fn set_shuffle(&self, enabled: bool) {
        if let Err(error) = self.settings.edit(|stored| {
            stored.ui.shuffle_enabled = enabled;
            Ok(())
        }) {
            warn!(%error, "could not save shuffle setting");
        }
        self.send(Command::SetShuffle {
            enabled,
            seed: random_u64(),
        })
    }
    pub fn cycle_repeat(&self) {
        match self.settings.edit(|stored| {
            stored.ui.repeat_mode = next_repeat(stored.ui.repeat_mode);
            Ok(stored.ui.repeat_mode)
        }) {
            Ok(repeat) => self.send(Command::SetRepeat(repeat)),
            Err(error) => {
                warn!(%error, "could not save repeat setting");
                self.set_repeat(RepeatMode::Off);
            }
        }
    }
    pub fn set_repeat(&self, repeat: RepeatMode) {
        if let Err(error) = self.settings.edit(|stored| {
            stored.ui.repeat_mode = repeat;
            Ok(())
        }) {
            warn!(%error, "could not save repeat setting");
        }
        self.send(Command::SetRepeat(repeat))
    }
    pub fn toggle_auto_dj(&self) {
        let (value, threshold) = self
            .settings
            .edit(|stored| {
                stored.ui.auto_dj_enabled = !stored.ui.auto_dj_enabled;
                Ok((
                    stored.ui.auto_dj_enabled,
                    stored.ui.application.auto_dj_refill_threshold,
                ))
            })
            .unwrap_or_else(|error| {
                warn!(%error, "could not save Auto DJ setting");
                (false, 1)
            });
        self.send(Command::SetAutoDj {
            enabled: value,
            refill_threshold: usize::from(threshold),
        })
    }
}

impl PlaybackOwner {
    pub(crate) async fn receiver_queue_page(
        &self,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<QueuePage, String> {
        let limit = limit.min(library::QUEUE_CONTEXT_LIMIT as u32);
        let Some(active) = self.active() else {
            return Ok(QueuePage {
                rows: Vec::new(),
                total: 0,
                window_offset: 0,
            });
        };
        let (requested, total) = if filter.trim().is_empty() {
            let (entries, total) = active
                .page_entries(offset as usize, limit as usize)
                .map_err(string_error)?;
            (entries, total as u64)
        } else {
            let queue = active.handoff_snapshot().map_err(string_error)?.0;
            let (positions, total) = self
                .database
                .queue_search_page(&queue, &filter, offset, limit)
                .await
                .map_err(string_error)?;
            let entries = positions
                .into_iter()
                .map(|position| {
                    let index = queue.order[position] as usize;
                    (
                        position,
                        queue.ids.get(index).copied(),
                        queue.entries[index].clone(),
                    )
                })
                .collect();
            (entries, total)
        };
        let entries = requested
            .iter()
            .map(|(position, _, entry)| (*position, entry.clone()))
            .collect::<Vec<_>>();
        let mut rows = self
            .database
            .queue_rows_for_entries(&entries, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        for (row, (_, id, _)) in rows.iter_mut().zip(requested) {
            Arc::make_mut(row).occurrence_id = id;
        }
        Ok(QueuePage {
            rows,
            total,
            window_offset: offset.min(total),
        })
    }

    pub fn shutdown(&self) {
        if let Some(active) = self.active() {
            let _ = active.shutdown();
        }
        let (reply, done) = std::sync::mpsc::sync_channel(0);
        if self
            .store_sender
            .try_send(PlaybackStoreWork::Flush(reply))
            .is_ok()
        {
            let _ = done.recv();
        }
        self.take_active();
    }
}

pub(crate) async fn prepare_stream<F>(
    database: &Database,
    request: StreamRequest,
    source: impl FnOnce(sources::SourceId) -> F + Send,
) -> Result<audio_processing::ResolvedStream, playback::BackendFailure>
where
    F: std::future::Future<Output = Result<Arc<sources::Source>, String>> + Send,
{
    let mut access = database
        .playback_access(&request.media_uri)
        .await
        .ok()
        .flatten();
    if let Some(uri) = access.as_ref()
        && !local_access_available(uri).await?
    {
        access = None;
    }
    // Downloads names its owned file using the actual transcoded extension.
    // Mapped and original files still use their parsed source format.
    let content_type = if let Some(uri) = &access {
        let path = library::file_media_path(uri);
        path.and_then(|path: std::path::PathBuf| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .and_then(audio_mime)
        })
    } else {
        None
    };
    if let Some(uri) = access {
        return Ok(audio_processing::ResolvedStream::new(uri).with_content_type(content_type));
    }
    let (source_id, kind, _) = library::source_entity_parts(&request.media_uri)
        .ok_or_else(crate::source::source_access_unavailable)?;
    if kind != "track" {
        return Err(crate::source::source_access_unavailable().into());
    }
    let source = source(source_id).await?;
    source.stream(request).await.map_err(|error| match error {
        sources::SourceError::NotFound => {
            let kind = playback::FailureKind::UnavailableMedia;
            playback::BackendFailure::with_kind(kind, kind.summary())
        }
        error => error.to_string().into(),
    })
}

// Catches a removed local file before it reaches playback, where a failed
// gapless preload would cut the current song short.
async fn local_stream_available(uri: &str) -> bool {
    let local = url::Url::parse(uri).is_ok_and(|uri| uri.scheme() == "file");
    !local || local_access_available(uri).await.unwrap_or(true)
}

async fn local_access_available(uri: &str) -> Result<bool, String> {
    let Some(path) = library::file_media_path(uri) else {
        return Ok(false);
    };
    tokio::task::spawn_blocking(move || match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    })
    .await
    .map_err(string_error)?
}

async fn cached_loudness(
    database: &Database,
    uri: &str,
    mode: playback::LoudnessNormalization,
) -> Option<playback::TrackLoudness> {
    if mode == playback::LoudnessNormalization::Off {
        return None;
    }
    let (track, album) = database
        .playback_loudness(uri, &ReadCancellation::new())
        .await
        .unwrap_or_default();
    Some(playback::TrackLoudness {
        track: track.map(Box::new),
        album: album.map(Box::new),
    })
}

fn prepare_media_stream(
    stream: audio_processing::ResolvedStream,
    loudness: Option<playback::TrackLoudness>,
    occurrence: Arc<playback::QueueRow>,
) -> PreparedStream {
    let content_type = occurrence
        .track
        .metadata
        .source_format
        .as_deref()
        .and_then(audio_mime);
    PreparedStream::new(stream, loudness).with_row(occurrence, content_type)
}

pub(crate) fn audio_mime(source_format: &str) -> Option<String> {
    let mime = match source_format.trim().to_ascii_lowercase().as_str() {
        "mp3" | "mp2" | "mpeg" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "mp4" => "audio/mp4",
        "aac" => "audio/aac",
        "opus" => "audio/ogg; codecs=opus",
        "vorbis" => "audio/ogg; codecs=vorbis",
        "ogg" | "oga" => "audio/ogg",
        "wav" | "wave" => "audio/wav",
        "webm" => "audio/webm",
        _ => return None,
    };
    Some(mime.to_string())
}

const fn next_repeat(value: RepeatMode) -> RepeatMode {
    match value {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}
pub(crate) fn random_u64() -> u64 {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes)
        .map(|()| u64::from_le_bytes(bytes))
        .unwrap_or_else(|_| unix_seconds(SystemTime::now()) as u64)
}
fn random_identity() -> String {
    format!("{:016x}{:016x}", random_u64(), random_u64())
}
fn unix_seconds(now: SystemTime) -> i64 {
    now.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}
fn local_calendar_period(unix_seconds: i64) -> String {
    glib::DateTime::from_unix_local(unix_seconds)
        .or_else(|_| glib::DateTime::from_unix_utc(unix_seconds))
        .and_then(|date| date.format("%Y-%m"))
        .map(|period| period.to_string())
        .unwrap_or_else(|_| "1970-01".to_string())
}
fn empty_clock_sample() -> ClockSample {
    ClockSample {
        monotonic_millis: 0,
        unix_seconds: 0,
        local_period: "1970-01".to_string(),
    }
}
fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::prepare_media_stream;

    fn test_media(source_format: &str) -> playback::TrackReference {
        playback::TrackReference {
            media_uri: library::source_entity_uri(
                &library::SourceId::new("source"),
                "track",
                "track",
            ),
            artwork_binding: None,
            metadata: library::TrackMetadata {
                audio_properties: Default::default(),
                title: "Track".to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                album_display_artist: None,
                duration_millis: 180_000,
                disc_number: 0,
                track_number: 0,
                year: None,
                release_date: None,
                source_format: Some(source_format.to_string()),
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                primary_artist_musicbrainz_id: None,
                ..Default::default()
            },
        }
    }

    fn test_occurrence(source_format: &str) -> playback::QueueRow {
        playback::QueueRow {
            occurrence_id: None,
            position: 0,
            provenance: playback::Provenance::Manual,
            track: {
                let reference = test_media(source_format);
                library::TrackView {
                    track_key: library::TrackKey::from_raw(1),
                    source_key: None,
                    source_id: None,
                    object_id: None,
                    media_uri: reference.media_uri,
                    metadata: reference.metadata,
                    artwork_binding: reference.artwork_binding,
                    state: Default::default(),
                    access: Default::default(),
                    links: Default::default(),
                }
            },
        }
    }

    #[test]
    fn prepared_original_stream_keeps_format() {
        let prepared = prepare_media_stream(
            audio_processing::ResolvedStream::new("https://provider.example/stream?id=track"),
            Some(playback::TrackLoudness::default()),
            test_occurrence("flac").into(),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/flac"));
    }

    #[test]
    fn provider_transcode_is_published_as_mpeg() {
        let prepared = prepare_media_stream(
            audio_processing::ResolvedStream::new("https://provider.example/stream?id=track")
                .with_content_type(Some("audio/mpeg".to_string())),
            Some(playback::TrackLoudness::default()),
            test_occurrence("flac").into(),
        );

        assert_eq!(prepared.content_type.as_deref(), Some("audio/mpeg"));
    }

    #[tokio::test]
    async fn downloaded_transcode_keeps_its_actual_format_and_canonical_occurrence() {
        let folder = tempfile::tempdir().unwrap();
        let database = library::Database::open(folder.path().join("library.sqlite3"))
            .await
            .unwrap();
        let occurrence = test_occurrence("flac");
        database
            .queue_entries(
                library::QueueLoad::items(vec![(
                    occurrence.track.clone().into(),
                    library::QueueProvenance::Manual,
                )]),
                None,
            )
            .await
            .unwrap();
        let path = folder.path().join("track.opus");
        std::fs::write(&path, b"downloaded audio").unwrap();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        database
            .write_download_access(
                None,
                &[&library::DownloadAccessWrite {
                    media_uri: occurrence.track.media_uri.clone(),
                    path: path.to_string_lossy().into_owned(),
                    root: folder.path().to_string_lossy().into_owned(),
                    relative_path: "track.opus".into(),
                    size_bytes: 0,
                    mtime_ns: 0,
                    device_id: None,
                    inode: None,
                    parser_version: 1,
                    access_uri: uri.clone(),
                    loudness_analysis_key: None,
                }],
            )
            .await
            .unwrap();
        let stream = super::prepare_stream(
            &database,
            playback::StreamRequest::new(
                occurrence.track.media_uri.clone(),
                playback::StreamQuality::Original,
            ),
            |_| async { panic!("a completed download must resolve without contacting its source") },
        )
        .await
        .unwrap();
        let canonical = occurrence.track.media_uri.clone();
        let prepared = prepare_media_stream(
            stream,
            Some(playback::TrackLoudness::default()),
            occurrence.into(),
        );
        assert_eq!(prepared.uri(), uri);
        assert_eq!(
            prepared.content_type.as_deref(),
            Some("audio/ogg; codecs=opus")
        );
        assert_eq!(prepared.row.as_ref().unwrap().track.media_uri, canonical);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("{0}")]
    Message(String),
}

pub struct PreparedQueuePage {
    pub revision: u64,
    pub rows: Vec<Arc<library::QueueRow>>,
    pub total: u64,
    pub window_offset: u64,
}

impl PlaybackOwner {
    pub async fn selection(
        self: Arc<Self>,
    ) -> Result<Arc<library::MemberSelection>, PlaybackError> {
        let runtime = self.runtime.clone();
        runtime
            .spawn_blocking(move || {
                let queue = self.snapshot().map_err(PlaybackError::Message)?;
                Ok(Arc::new(library::MemberSelection::Keys {
                    source: None,
                    keys: queue
                        .order
                        .iter()
                        .map(|index| queue.entries[*index as usize].track_key)
                        .collect::<Vec<_>>()
                        .into(),
                }))
            })
            .await
            .map_err(|error| PlaybackError::Message(error.to_string()))?
    }

    pub async fn queue_page(
        self: Arc<Self>,
        offset: u64,
        limit: u32,
        filter: String,
    ) -> Result<PreparedQueuePage, PlaybackError> {
        let runtime = self.runtime.clone();
        tokio_util::task::AbortOnDropHandle::new(runtime.spawn(async move {
            let revision = self.updates.current().map_or(0, |view| view.queue.revision);
            let page = self
                .receiver_queue_page(offset, limit, filter)
                .await
                .map_err(PlaybackError::Message)?;
            Ok(PreparedQueuePage {
                revision,
                rows: page.rows,
                total: page.total,
                window_offset: page.window_offset,
            })
        }))
        .await
        .map_err(|error| PlaybackError::Message(error.to_string()))?
    }
}
