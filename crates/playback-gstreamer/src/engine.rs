use super::audio::{SharedQueuedLoudness, passthrough_allowed};
use super::measurement::MeasurementAnalyzer;
use super::pipeline::{AboutToFinishAction, PlayerPipeline, SourceClock};
use super::*;
use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, sync_channel};

mod handoff;
mod messages;
mod status_fade;
#[cfg(test)]
mod tests;
mod tick;

const GAPLESS_BUFFERING_IGNORE_REMAINING_MS: u64 = 5_000;
const STATUS_FADE_DURATION: Duration = Duration::from_millis(150);

struct AudioWarning {
    category: String,
    element: Option<String>,
    message: String,
    captured_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TelemetryKind {
    Position,
    Duration,
    Buffering,
}

struct PendingTelemetry {
    sequence: u64,
    event: BackendEvent,
}

/// Everything the playback worker reacts to, in arrival order.
pub(super) enum WorkerInput {
    Command(Box<BackendCommand>),
    Processing(ProcessingRequest),
    Bus(Slot, PipelineId, gst::Message),
    Wake,
    Shutdown,
}

#[derive(Default)]
pub(super) struct EventMailbox {
    next_sequence: u64,
    ready: VecDeque<BackendEvent>,
    latest: HashMap<(RunId, TelemetryKind), PendingTelemetry>,
    processing_meter: Option<BackendEvent>,
    waker: Option<EventWaker>,
}

impl EventMailbox {
    fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.latest.is_empty() && self.processing_meter.is_none()
    }

    /// Returns the waker when this event makes the mailbox non-empty.
    fn push(&mut self, event: BackendEvent) -> Option<EventWaker> {
        let waker = self.is_empty().then(|| self.waker.clone()).flatten();
        self.insert(event);
        waker
    }

    fn insert(&mut self, event: BackendEvent) {
        if let BackendEvent::ProcessingMeter {
            output,
            mut peak_db,
            spectrum_db,
        } = event
        {
            if let Some(BackendEvent::ProcessingMeter {
                output: old_output,
                peak_db: old_peak,
                spectrum_db: old_spectrum,
            }) = self.processing_meter.as_mut()
                && *old_output == output
            {
                if peak_db.len() == old_peak.len() {
                    for (peak, old) in peak_db.iter_mut().zip(old_peak.iter()) {
                        *peak = peak.max(*old);
                    }
                }
                if !peak_db.is_empty() {
                    *old_peak = peak_db;
                }
                if !spectrum_db.is_empty() {
                    *old_spectrum = spectrum_db;
                }
            } else {
                self.processing_meter = Some(BackendEvent::ProcessingMeter {
                    output,
                    peak_db,
                    spectrum_db,
                });
            }
            return;
        }
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        if let Some(key) = telemetry_key(&event) {
            self.latest
                .insert(key, PendingTelemetry { sequence, event });
        } else {
            self.flush_telemetry();
            self.ready.push_back(event);
        }
    }

    fn drain(&mut self) -> Vec<BackendEvent> {
        self.flush_telemetry();
        if let Some(meter) = self.processing_meter.take() {
            self.ready.push_back(meter);
        }
        self.ready.drain(..).collect()
    }

    fn flush_telemetry(&mut self) {
        let mut telemetry = self
            .latest
            .drain()
            .map(|(_, event)| event)
            .collect::<Vec<_>>();
        telemetry.sort_unstable_by_key(|event| event.sequence);
        self.ready
            .extend(telemetry.into_iter().map(|event| event.event));
    }
}

fn telemetry_key(event: &BackendEvent) -> Option<(RunId, TelemetryKind)> {
    match event {
        BackendEvent::Position { run, .. } => Some((*run, TelemetryKind::Position)),
        BackendEvent::Duration { run, .. } => Some((*run, TelemetryKind::Duration)),
        BackendEvent::Buffering { run, .. } => Some((*run, TelemetryKind::Buffering)),
        BackendEvent::Started { .. }
        | BackendEvent::State { .. }
        | BackendEvent::Seekable { .. }
        | BackendEvent::Ended { .. }
        | BackendEvent::Transitioned { .. }
        | BackendEvent::Bitstream { .. }
        | BackendEvent::NextNeeded { .. }
        | BackendEvent::NextPreparationFailed { .. }
        | BackendEvent::AudioApplied { .. }
        | BackendEvent::ProcessingApplied { .. }
        | BackendEvent::ProcessingFailed { .. }
        | BackendEvent::ProcessingMeter { .. }
        | BackendEvent::OperationFailed { .. }
        | BackendEvent::TransportRejected { .. }
        | BackendEvent::TransportObserved { .. }
        | BackendEvent::Error { .. } => None,
    }
}

pub struct GStreamerPlaybackBackend {
    commands: Option<Sender<WorkerInput>>,
    events: Arc<Mutex<EventMailbox>>,
    thread: Option<thread::JoinHandle<()>>,
    processing: tokio::task::JoinHandle<()>,
}
impl GStreamerPlaybackBackend {
    pub fn new(
        mut processing: tokio::sync::watch::Receiver<ProcessingRequest>,
    ) -> Result<Self, BackendError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|error| BackendError::Backend(error.to_string()))?;
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let thread_events = Arc::clone(&events);
        let initial = processing.borrow_and_update().clone();
        let (ready_sender, ready_receiver) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("slopsonic-gstreamer-playback".to_string())
            .spawn(move || run_gstreamer_thread(thread_events, ready_sender, initial))
            .map_err(|error| BackendError::Backend(error.to_string()))?;
        match ready_receiver.recv() {
            Ok(Ok(commands)) => Ok(Self {
                processing: runtime.spawn({
                    let commands = commands.clone();
                    async move {
                        while processing.changed().await.is_ok() {
                            let request = processing.borrow_and_update().clone();
                            if commands.send(WorkerInput::Processing(request)).is_err() {
                                break;
                            }
                        }
                    }
                }),
                commands: Some(commands),
                events,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(BackendError::Backend(error))
            }
            Err(_) => {
                let _ = thread.join();
                Err(BackendError::ChannelClosed)
            }
        }
    }
}
impl PlaybackBackend for GStreamerPlaybackBackend {
    fn send(&mut self, command: BackendCommand) -> Result<(), BackendError> {
        self.commands
            .as_ref()
            .ok_or(BackendError::ChannelClosed)?
            .send(WorkerInput::Command(Box::new(command)))
            .map_err(|_| BackendError::ChannelClosed)
    }

    fn drain_events(&mut self) -> Vec<BackendEvent> {
        lock_recover(&self.events).drain()
    }

    fn set_event_waker(&mut self, waker: EventWaker) {
        let pending = {
            let mut events = lock_recover(&self.events);
            events.waker = Some(Arc::clone(&waker));
            !events.is_empty()
        };
        if pending {
            waker();
        }
    }

    fn shutdown(&mut self) -> Result<(), BackendError> {
        self.processing.abort();
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(WorkerInput::Shutdown);
        }
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| BackendError::Backend("GStreamer playback worker panicked".to_string()))
    }
}

impl Drop for GStreamerPlaybackBackend {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Slot {
    Primary,
    Secondary,
}

impl Slot {
    fn index(self) -> usize {
        match self {
            Self::Primary => 0,
            Self::Secondary => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct PipelineId(pub(super) u64);

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreparedRun {
    pub(super) run: RunId,
    pub(super) stream: PreparedStream,
}

impl PreparedRun {
    fn from_next(next: &PreparedNext) -> Self {
        Self {
            run: next.run,
            stream: next.stream.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IncomingPhase {
    Prerolling,
    Ready,
}

#[derive(Clone, Debug)]
struct IncomingPipeline {
    id: PipelineId,
    slot: Slot,
    item: PreparedNext,
    phase: IncomingPhase,
}

fn fade_in(item: &PreparedNext) -> Option<Duration> {
    match item.transition {
        NextTransition::Gapless => None,
        NextTransition::Crossfade { duration_millis } => {
            Some(Duration::from_millis(duration_millis))
        }
    }
}

#[derive(Clone, Debug)]
struct PendingHandoff {
    incoming: IncomingPipeline,
    from: Slot,
    old_run: RunId,
}

impl PendingHandoff {
    fn matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.incoming.slot == slot && self.incoming.id == id
    }

    fn item(&self) -> &PreparedNext {
        &self.incoming.item
    }
}

/// What the second player is doing. Only one transition runs at a time.
#[derive(Clone, Debug, Default)]
enum Transition {
    #[default]
    None,
    /// The next item prerolls, paused, on the inactive slot.
    Preparing(IncomingPipeline),
    /// The prepared item was asked to play and commits once it reports Playing.
    Activating(PendingHandoff),
    /// Both players run while `from` fades out under its own gain curve.
    Overlap { from: Slot, old_run: RunId },
}

/// The latest seek target. Only one seek runs at a time; a newer target waits for it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SeekState {
    /// Physical position of the seek the pipeline is performing.
    in_flight: Option<u64>,
    /// Physical position requested while a seek runs or before the session prerolled.
    queued: Option<u64>,
}

#[derive(Debug)]
pub(super) struct SharedBackendState {
    pub(super) settings: BackendAudioSettings,
    pub(super) processing: Option<ProcessingRequest>,
    pub(super) current: Option<PreparedRun>,
    pub(super) next: Option<PreparedNext>,
    pub(super) active: Slot,
    pipeline_ids: [Option<PipelineId>; 2],
}
impl SharedBackendState {
    pub(super) fn new() -> Self {
        let settings = BackendAudioSettings::default();
        Self {
            current: None,
            processing: None,
            next: None,
            active: Slot::Primary,
            pipeline_ids: [None, None],
            settings,
        }
    }

    fn pipeline_id(&self, slot: Slot) -> Option<PipelineId> {
        self.pipeline_ids[slot.index()]
    }

    fn set_pipeline_id(&mut self, slot: Slot, id: Option<PipelineId>) {
        self.pipeline_ids[slot.index()] = id;
    }

    fn pipeline_is_live(&self, slot: Slot, id: PipelineId) -> bool {
        self.pipeline_id(slot) == Some(id)
    }

    pub(super) fn pipeline_is_current(&self, slot: Slot, id: PipelineId) -> bool {
        self.active == slot && self.pipeline_is_live(slot, id)
    }
}
#[derive(Debug, Default)]
pub(super) struct GaplessPlayback {
    pub(super) pending: Option<PreparedNext>,
    pub(super) about_to_finish_pending: bool,
    pub(super) next_needed: Option<RunId>,
    // A passed-through stream can't overlap another, so crossfades play as gapless.
    pub(super) bitstream: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusFadeTarget {
    Pause,
    Playing,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct StatusFade {
    slot: Slot,
    target: StatusFadeTarget,
    muted: bool,
}
impl StatusFade {
    pub(super) fn new(slot: Slot, target: StatusFadeTarget, muted: bool) -> Self {
        Self {
            slot,
            target,
            muted,
        }
    }
}
pub(super) struct GstEngine {
    inputs: Receiver<WorkerInput>,
    input_sender: Sender<WorkerInput>,
    pub(super) primary: PlayerPipeline,
    pub(super) secondary: PlayerPipeline,
    pub(super) shared: Arc<Mutex<SharedBackendState>>,
    events: Arc<Mutex<EventMailbox>>,
    pub(super) measurement: MeasurementAnalyzer,
    pub(super) last_position_tick: Instant,
    pub(super) seek: SeekState,
    // Positions mean nothing before a session's first preroll.
    session_prerolled: bool,
    // A starting session passes through Paused before it plays.
    awaiting_playing: bool,
    pub(super) status_fade: Option<StatusFade>,
    pub(super) play_command_started_at: Option<Instant>,
    desired_playing: bool,
    ended_run: Option<RunId>,
    next_pipeline_number: u64,
    transition: Transition,
}
impl GstEngine {
    fn new(events: Arc<Mutex<EventMailbox>>) -> Self {
        let shared = Arc::new(Mutex::new(SharedBackendState::new()));
        let (input_sender, inputs) = channel();
        let primary = PlayerPipeline::new(
            "slopsonic-primary-player",
            Slot::Primary,
            Arc::clone(&shared),
            input_sender.clone(),
        );
        let secondary = PlayerPipeline::new(
            "slopsonic-secondary-player",
            Slot::Secondary,
            Arc::clone(&shared),
            input_sender.clone(),
        );
        let measurement = MeasurementAnalyzer::new(Arc::clone(&events), Arc::clone(&shared));
        Self {
            inputs,
            input_sender,
            primary,
            secondary,
            shared,
            events,
            measurement,
            last_position_tick: Instant::now(),
            seek: SeekState::default(),
            session_prerolled: false,
            awaiting_playing: false,
            status_fade: None,
            play_command_started_at: None,
            desired_playing: false,
            ended_run: None,
            next_pipeline_number: 1,
            transition: Transition::None,
        }
    }

    fn handle_command(&mut self, command: BackendCommand) {
        let command_run = command.run().or_else(|| self.timing_run_id());
        let changes_transport = matches!(
            command,
            BackendCommand::Start { .. }
                | BackendCommand::Play { .. }
                | BackendCommand::Pause { .. }
        );
        let result = match command {
            BackendCommand::Start {
                run,
                current,
                next,
                start_position_millis,
            } => self.play_prepared(
                PreparedRun {
                    run,
                    stream: current,
                },
                next,
                start_position_millis,
            ),
            BackendCommand::PrepareNext { current_run, next } => {
                if self.run_is_current(current_run) {
                    self.prepare_next(next);
                }
                Ok(())
            }
            BackendCommand::UpdateLoudness { run, loudness } => {
                if self.run_is_current(run) {
                    if let Some(current) = lock_recover(&self.shared).current.as_mut() {
                        current.stream.loudness = Some(loudness.clone());
                    }
                    self.active_pipeline_mut().update_loudness(&loudness);
                    self.end_passthrough_if_needed()
                } else {
                    Ok(())
                }
            }
            BackendCommand::ConfigureAudio(settings) => {
                debug!(
                    fade_on_play_stop = settings.fade_on_status_change,
                    volume_gain = settings.output_gain(),
                    muted = settings.muted,
                    desired_playing = self.desired_playing,
                    "GStreamer audio configuration requested"
                );
                let previous_settings = self.settings();
                if self.desired_playing {
                    self.cancel_status_fade();
                } else {
                    self.status_fade = None;
                }
                let result = (|| -> Result<(), BackendFailure> {
                    if !self.desired_playing {
                        if self.active_pipeline().has_session() {
                            if let Err(error) = self.active_pipeline().set_state(gst::State::Paused)
                            {
                                self.retire_playback();
                                return Err(error);
                            }
                            self.push_state(BackendState::Paused);
                        }
                    }
                    lock_recover(&self.shared).settings = settings.clone();
                    self.primary.configure_audio(&settings)?;
                    self.secondary.configure_audio(&settings)?;
                    self.end_passthrough_if_needed()?;
                    let (gain, muted) = self.output_gain_state();
                    self.apply_output_gain_to_pipelines(gain, muted);
                    push_event(
                        &self.events,
                        BackendEvent::AudioApplied {
                            volume: settings.volume,
                            muted,
                        },
                    );
                    Ok(())
                })();
                if result.is_ok() {
                    self.prepare_reserved_incoming();
                } else {
                    let _ = self.primary.configure_audio(&previous_settings);
                    let _ = self.secondary.configure_audio(&previous_settings);
                    lock_recover(&self.shared).settings = previous_settings;
                    let (gain, muted) = self.output_gain_state();
                    self.apply_output_gain_to_pipelines(gain, muted);
                }
                result
            }
            BackendCommand::SetOutputVolume {
                volume,
                volume_scale,
                muted,
            } => {
                let volume = if volume.is_finite() {
                    volume.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                {
                    let mut shared = lock_recover(&self.shared);
                    shared.settings.volume = volume;
                    shared.settings.volume_scale = volume_scale;
                    shared.settings.muted = muted;
                }
                let (gain, muted) = self.output_gain_state();
                self.apply_output_gain_to_pipelines(gain, muted);
                push_event(&self.events, BackendEvent::AudioApplied { volume, muted });
                self.end_passthrough_if_needed()
            }
            BackendCommand::Play { run } => {
                if self.run_is_current(run) {
                    let result = self.start_status_resume();
                    if result.is_ok() {
                        self.prepare_reserved_incoming();
                    }
                    result
                } else {
                    Ok(())
                }
            }
            BackendCommand::Pause { run } => {
                if self.run_is_current(run) {
                    self.start_status_pause()
                } else {
                    Ok(())
                }
            }
            BackendCommand::Stop { run } => {
                if !self.run_is_current(run) {
                    return;
                }
                self.ended_run = None;
                self.retire_playback();
                push_event(&self.events, BackendEvent::Position { run, millis: 0 });
                push_event(
                    &self.events,
                    BackendEvent::State {
                        run,
                        state: BackendState::Stopped,
                    },
                );
                Ok(())
            }
            BackendCommand::Seek {
                run,
                position_millis,
            } => {
                if self.run_is_current(run) {
                    let result = self.start_seek(position_millis);
                    if result.is_ok() {
                        self.prepare_reserved_incoming();
                    }
                    result
                } else {
                    Ok(())
                }
            }
        };

        if let Err(error) = result {
            let run = command_run;
            if let Some(run) = run
                && (changes_transport || !self.active_pipeline().has_session())
            {
                self.fail_playback(Some(run), error);
            } else {
                push_event(&self.events, BackendEvent::OperationFailed { run, error });
            }
        }
    }

    fn play_prepared(
        &mut self,
        item: PreparedRun,
        next: Option<PreparedNext>,
        start_position_millis: u64,
    ) -> Result<(), BackendFailure> {
        self.desired_playing = true;
        let incoming_next = next.clone();
        let command_started_at = Instant::now();
        self.play_command_started_at = Some(command_started_at);
        let _ = self.cancel_status_fade();
        self.ended_run = None;
        self.transition = Transition::None;
        let settings = self.settings();
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
        let output_gain = settings.output_gain();
        let muted = settings.muted;
        let start_millis =
            SourceClock::from_stream(&item.stream).physical_seek(start_position_millis);
        {
            let mut shared = lock_recover(&self.shared);
            shared.settings = settings.clone();
            shared.current = Some(item.clone());
            shared.next = next;
            shared.active = Slot::Primary;
        }
        self.push_state(BackendState::Buffering);
        let pipeline_started_at = Instant::now();
        let needs_preroll_seek = start_millis > 0;
        let startup_state = if needs_preroll_seek {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        self.start_pipeline(
            Slot::Primary,
            &item,
            &settings,
            output_gain,
            muted,
            startup_state,
            None,
            None,
        )?;
        info!(
            run = %item.run,
            uri_scheme = %stream_uri_scheme(item.stream.uri()),
            start_millis,
            audio_output = self.primary.audio_output_factory().as_deref().unwrap_or("unknown"),
            elapsed_ms = command_started_at.elapsed().as_millis(),
            pipeline_ms = pipeline_started_at.elapsed().as_millis(),
            "queued GStreamer playback item"
        );
        self.session_started(start_millis, needs_preroll_seek, gst::State::Playing);
        if let Some(next) = incoming_next.as_ref() {
            self.prepare_incoming(next);
        }
        Ok(())
    }

    fn prepare_next(&mut self, next: Option<PreparedNext>) {
        let Some(next) = next else {
            self.clear_prepared_next();
            return;
        };
        if self
            .incoming()
            .is_some_and(|incoming| incoming.item == next)
            || self
                .activating()
                .is_some_and(|pending| pending.item() == &next)
        {
            return;
        }

        let ended_run = self.cancel_handoff_for_replan();
        if let Some(ended_run) = ended_run {
            let mut shared = lock_recover(&self.shared);
            shared.next = Some(next);
            drop(shared);
            self.emit_ended_once(ended_run);
            return;
        }

        {
            let mut shared = lock_recover(&self.shared);
            shared.next = Some(next.clone());
            if let Some(gapless) = self.pipeline_for_slot(shared.active).gapless() {
                lock_recover(gapless).next_needed = None;
            }
        }
        self.prepare_incoming(&next);
    }

    fn clear_prepared_next(&mut self) {
        let ended_run = self.cancel_handoff_for_replan();
        {
            let mut shared = lock_recover(&self.shared);
            shared.next = None;
            if let Some(gapless) = self.pipeline_for_slot(shared.active).gapless() {
                let mut gapless = lock_recover(gapless);
                gapless.about_to_finish_pending = false;
                gapless.next_needed = None;
            }
        }
        if let Some(ended_run) = ended_run {
            self.emit_ended_once(ended_run);
        }
    }

    pub(super) fn start_seek(&mut self, millis: u64) -> Result<(), BackendFailure> {
        self.ended_run = None;
        let _ = self.cancel_status_fade();
        self.finish_overlap();
        // A queued URI, or a drain past a failed one, cannot be taken back by a seek.
        let current_after_gapless_cancel = self.cancel_handoff_for_seek().or_else(|| {
            (self.active_pipeline().has_pending_gapless() || self.active_pipeline().draining())
                .then(|| self.current_item().ok())
                .flatten()
        });
        let target_state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let current_after_gapless_cancel = current_after_gapless_cancel.or_else(|| {
            (self.active_pipeline().native_seekable() == Some(false))
                .then(|| self.current_item().ok())
                .flatten()
                .filter(|item| item.stream.time_seekable())
        });
        // Restarting at 0 avoids an audible skip after AAC priming on some files.
        if let Some(current) = current_after_gapless_cancel
            .or_else(|| (millis == 0).then(|| self.current_item().ok()).flatten())
        {
            let bitstream = self.active_pipeline().bitstream_format();
            self.start_item_session_at_millis(current, millis, target_state, bitstream)?;
            self.push_logical_position(millis);
            return Ok(());
        }
        if !self.desired_playing
            && let Err(error) = self.active_pipeline().set_state(gst::State::Paused)
        {
            self.retire_playback();
            return Err(error);
        }
        let target = self.active_pipeline().physical_seek_target(millis);
        // Repeating the running target, as repeated seek requests do, needs no new seek.
        if self.seek.in_flight == Some(target) {
            self.seek.queued = None;
            return Ok(());
        }
        if self.seek.in_flight.is_some() || !self.session_prerolled {
            self.seek.queued = Some(target);
            return Ok(());
        }
        self.send_seek(target).inspect_err(|error| {
            warn!(%error, target_millis = millis, "GStreamer seek request failed");
            if let Some(position) = self.active_pipeline().position() {
                self.push_position(clock_millis(position));
            }
        })
    }

    fn send_seek(&mut self, target: u64) -> Result<(), BackendFailure> {
        let state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        // A source that cannot seek natively reopens at the requested source time.
        if self.active_pipeline().native_seekable() == Some(false)
            && let Some(current) = self
                .current_item()
                .ok()
                .filter(|item| item.stream.time_seekable())
        {
            let logical = self.active_pipeline().logical_position(target);
            let bitstream = self.active_pipeline().bitstream_format();
            self.start_item_session_at_millis(current, logical, state, bitstream)?;
            self.push_logical_position(logical);
            return Ok(());
        }
        self.active_pipeline().seek_physical_millis(target)?;
        self.seek.in_flight = self.active_pipeline().seek_in_progress().then_some(target);
        if let Err(error) = self.active_pipeline().set_state(state) {
            self.retire_playback();
            return Err(error);
        }
        Ok(())
    }

    /// Seek bookkeeping for a session just started at `start_millis`.
    fn session_started(&mut self, start_millis: u64, needs_preroll_seek: bool, state: gst::State) {
        self.seek = SeekState {
            in_flight: None,
            queued: needs_preroll_seek.then_some(start_millis),
        };
        self.session_prerolled = false;
        self.awaiting_playing = state == gst::State::Playing;
    }

    fn cancel_gapless_pending(&self) -> Option<(PreparedRun, PreparedNext)> {
        let shared = lock_recover(&self.shared);
        let gapless = self.pipeline_for_slot(shared.active).gapless()?;
        let mut gapless = lock_recover(gapless);
        let pending = gapless.pending.take()?;
        let current = shared.current.clone()?;
        gapless.about_to_finish_pending = false;
        Some((current, pending))
    }

    fn cancel_handoff_for_seek(&mut self) -> Option<PreparedRun> {
        let pending = self.take_activating()?;
        let ended = self.handoff_outgoing_ended(&pending);
        self.stop_pipeline(pending.incoming.slot);
        if ended {
            lock_recover(&self.shared).current.clone()
        } else {
            None
        }
    }

    fn prepare_reserved_incoming(&mut self) {
        let next = lock_recover(&self.shared).next.clone();
        if let Some(next) = next {
            self.prepare_incoming(&next);
        }
    }

    fn current_item(&self) -> Result<PreparedRun, BackendFailure> {
        lock_recover(&self.shared)
            .current
            .clone()
            .ok_or_else(|| "No current playback item is active".into())
    }

    fn start_item_session_at_millis(
        &mut self,
        mut item: PreparedRun,
        position_millis: u64,
        target_state: gst::State,
        bitstream: Option<gst::Caps>,
    ) -> Result<(), BackendFailure> {
        let (settings, volume, muted, slot) = self.session_context();
        if item.stream.time_seekable()
            && (item.stream.time_offset_millis().is_some()
                || self.active_pipeline().native_seekable() == Some(false))
        {
            *item.stream.stream = item
                .stream
                .reopened_at(position_millis)
                .map_err(|error| error.to_string())?;
        }
        let start_millis = SourceClock::from_stream(&item.stream).physical_seek(position_millis);
        let run = item.run;
        let active = bitstream.is_some();
        let needs_preroll_seek = start_millis > 0;
        let startup_state = if needs_preroll_seek {
            gst::State::Paused
        } else {
            target_state
        };
        self.start_pipeline(
            slot,
            &item,
            &settings,
            volume,
            muted,
            startup_state,
            None,
            bitstream,
        )?;
        self.session_started(start_millis, needs_preroll_seek, target_state);
        push_event(&self.events, BackendEvent::Bitstream { run, active });
        Ok(())
    }

    /// The logical position a restarted session continues from.
    fn restart_position_millis(&self) -> u64 {
        self.seek
            .queued
            .or(self.seek.in_flight)
            .or_else(|| self.active_pipeline().position().map(clock_millis))
            .map(|position| self.active_pipeline().logical_position(position))
            .unwrap_or_default()
    }

    /// The decoded session's first stream can reach the output undecoded.
    fn start_passthrough(&mut self, format: gst::Caps) {
        let Ok(current) = self.current_item() else {
            return;
        };
        let position = self.restart_position_millis();
        let state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        info!(run = %current.run, %format, "passing compressed audio to the output");
        // A crossfade prepared before the format was known can't mix with it.
        self.clear_incoming();
        if let Err(error) =
            self.start_item_session_at_millis(current, position, state, Some(format))
        {
            self.fail_playback(self.timing_run_id(), error);
        }
    }

    /// A setting that changes samples continues the current track decoded.
    fn end_passthrough_if_needed(&mut self) -> Result<(), BackendFailure> {
        let Some(format) = self.active_pipeline().bitstream_format() else {
            return Ok(());
        };
        let current = self.current_item()?;
        let allowed = {
            let shared = lock_recover(&self.shared);
            passthrough_allowed(
                &format,
                &shared.settings,
                shared.processing.as_ref(),
                current.stream.loudness.as_ref(),
            )
        };
        if allowed {
            return Ok(());
        }
        let position = self.restart_position_millis();
        let state = if self.desired_playing {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        self.start_item_session_at_millis(current, position, state, None)?;
        self.push_logical_position(position);
        if !self.desired_playing {
            self.push_state(BackendState::Paused);
        }
        self.prepare_reserved_incoming();
        Ok(())
    }

    fn session_context(&self) -> (BackendAudioSettings, f64, bool, Slot) {
        let shared = lock_recover(&self.shared);
        (
            shared.settings.clone(),
            shared.settings.output_gain(),
            shared.settings.muted,
            shared.active,
        )
    }

    fn set_processing(&mut self, request: ProcessingRequest) {
        if let Err(message) =
            audio_processing::effects::EffectsBin::validate_available(&request.config)
        {
            push_event(
                &self.events,
                BackendEvent::ProcessingFailed {
                    output: request.output,
                    message,
                },
            );
            return;
        }
        lock_recover(&self.shared).processing = Some(request.clone());
        let mut applied = true;
        for slot in [Slot::Primary, Slot::Secondary] {
            if let Err(message) = self
                .pipeline_for_slot_mut(slot)
                .configure_processing(&request)
            {
                applied = false;
                push_event(
                    &self.events,
                    BackendEvent::ProcessingFailed {
                        output: request.output.clone(),
                        message: message.to_string(),
                    },
                );
            }
        }
        self.sync_processing_taps();
        if let Err(error) = self.end_passthrough_if_needed() {
            self.fail_playback(self.timing_run_id(), error);
        }
        // Without an audio graph in use, nothing else reports neutral processing applied.
        let graph_unused = self.active_pipeline().is_bitstream();
        if applied && (graph_unused || lock_recover(&self.shared).current.is_none()) {
            let rate = 48_000;
            let preamp_db = request.config.equalizer.as_ref().map_or(0.0, |p| {
                audio_processing::effects::effective_preamp_db(p, rate)
            });
            push_event(
                &self.events,
                BackendEvent::ProcessingApplied {
                    output: request.output,
                    config: request.config,
                    sample_rate: None,
                    preamp_db,
                },
            );
        }
    }

    fn poll_processing(&mut self) {
        for slot in [Slot::Primary, Slot::Secondary] {
            match self.pipeline_for_slot_mut(slot).poll_processing() {
                Ok(Some((config, sample_rate, preamp_db))) if self.is_active_slot(slot) => {
                    if let Some(output) = self.pipeline_for_slot(slot).processing_output() {
                        push_event(
                            &self.events,
                            BackendEvent::ProcessingApplied {
                                output,
                                config,
                                sample_rate,
                                preamp_db,
                            },
                        );
                    }
                }
                Err(message) => {
                    if let Some(output) = self.pipeline_for_slot(slot).processing_output() {
                        push_event(
                            &self.events,
                            BackendEvent::ProcessingFailed { output, message },
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// Handles every input already queued. Returns false after a shutdown request.
    #[cfg(test)]
    fn poll_bus(&mut self) -> bool {
        let inputs = self.inputs.try_iter().collect();
        self.handle_inputs(inputs)
    }

    fn handle_inputs(&mut self, inputs: VecDeque<WorkerInput>) -> bool {
        let mut inputs = inputs.into_iter().peekable();
        while let Some(input) = inputs.next() {
            match input {
                WorkerInput::Command(command) => self.handle_command(*command),
                WorkerInput::Processing(request) => self.set_processing(request),
                WorkerInput::Bus(slot, id, message) => {
                    // Use the latest consecutive refill update to avoid an unnecessary pause.
                    let superseded = message.type_() == gst::MessageType::Buffering
                        && inputs.peek().is_some_and(|next| {
                            matches!(next, WorkerInput::Bus(next_slot, _, next)
                                if *next_slot == slot
                                    && next.type_() == gst::MessageType::Buffering
                                    && next.src() == message.src())
                        });
                    if !superseded {
                        self.handle_message(slot, id, &message);
                    }
                }
                WorkerInput::Wake => {}
                WorkerInput::Shutdown => return false,
            }
        }
        true
    }

    /// The next time the worker must act without a new input.
    fn next_deadline(&self) -> Option<Duration> {
        const STEP: Duration = Duration::from_millis(50);
        if self.status_fade.is_some() {
            return Some(STEP);
        }
        if !self.desired_playing || !self.active_pipeline().has_session() {
            return None;
        }
        if self
            .active_pipeline()
            .gapless()
            .is_some_and(|gapless| lock_recover(gapless).about_to_finish_pending)
        {
            return Some(STEP);
        }
        let report = Duration::from_millis(500).saturating_sub(self.last_position_tick.elapsed());
        Some(
            self.crossfade_start_wait()
                .map_or(report, |wait| wait.min(report)),
        )
    }

    fn crossfade_start_wait(&self) -> Option<Duration> {
        let NextTransition::Crossfade { duration_millis } =
            lock_recover(&self.shared).next.as_ref()?.transition
        else {
            return None;
        };
        let pipeline = self.active_pipeline();
        let remaining = pipeline.logical_remaining(
            clock_millis(pipeline.position()?),
            clock_millis(pipeline.duration()?),
        );
        let start = duration_millis;
        // Past the start point, readiness messages drive the start instead.
        let wait = remaining.checked_sub(start).filter(|wait| *wait > 0)?;
        Some(Duration::from_millis(wait))
    }

    fn log_audio_warning(&self, warning: AudioWarning) {
        let slot = [Slot::Primary, Slot::Secondary].into_iter().find(|slot| {
            warning
                .element
                .as_deref()
                .is_some_and(|name| self.pipeline_for_slot(*slot).owns_audio_output(name))
        });
        let pipeline = slot.map(|slot| self.pipeline_for_slot(slot));
        let run = slot.and_then(|slot| self.run_for_slot(slot));
        let position_millis = pipeline.and_then(|pipeline| {
            pipeline
                .position()
                .map(|position| pipeline.logical_position(clock_millis(position)))
        });
        let duration_millis = pipeline.and_then(|pipeline| {
            pipeline
                .duration()
                .map(|duration| pipeline.logical_duration(clock_millis(duration)))
        });
        let role = match slot {
            Some(slot) if slot == self.active_slot() => "current",
            Some(_) if run.is_some() => "outgoing",
            Some(_) => "prepared",
            None => "unassigned",
        };
        warn!(
            category = %warning.category, element = ?warning.element, message = %warning.message,
            handling_delay_ms = warning.captured_at.elapsed().as_millis(),
            ?slot, run = run.map(RunId::get), role,
            ?position_millis, ?duration_millis,
            buffering_percent = pipeline.and_then(PlayerPipeline::buffering_percent),
            "GStreamer audio output warning"
        );
    }

    fn settings(&self) -> BackendAudioSettings {
        lock_recover(&self.shared).settings.clone()
    }

    fn output_gain_state(&self) -> (f64, bool) {
        let shared = lock_recover(&self.shared);
        (shared.settings.output_gain(), shared.settings.muted)
    }

    fn apply_output_gain_to_pipelines(&mut self, volume: f64, muted: bool) {
        let running: Vec<Slot> = self.running_slots().collect();
        let fade = self.status_fade.as_mut().map(|fade| {
            fade.muted = muted;
            fade.target
        });
        for slot in [Slot::Primary, Slot::Secondary] {
            let pipeline = self.pipeline_for_slot(slot);
            match fade {
                Some(target) if running.contains(&slot) => pipeline.retarget_output(
                    if target == StatusFadeTarget::Pause {
                        0.0
                    } else {
                        volume
                    },
                    muted,
                ),
                _ => pipeline.set_output_volume(volume, muted),
            }
        }
    }

    fn sync_processing_taps(&mut self) {
        let output = lock_recover(&self.shared)
            .processing
            .as_ref()
            .filter(|request| request.spectrum)
            .map(|request| request.output.clone());
        for slot in [Slot::Primary, Slot::Secondary] {
            let tap = output
                .as_ref()
                .filter(|output| {
                    self.pipeline_for_slot(slot).processing_output().as_ref() == Some(*output)
                })
                .and_then(|output| {
                    let shared = lock_recover(&self.shared);
                    let pipeline_id = shared.pipeline_id(slot)?;
                    let run = shared.current.as_ref()?.run;
                    Some(self.measurement.tap(slot, pipeline_id, run, output.clone()))
                });
            self.pipeline_for_slot_mut(slot).set_processing_tap(tap);
        }
    }

    fn active_pipeline(&self) -> &PlayerPipeline {
        self.pipeline_for_slot(self.active_slot())
    }

    fn active_pipeline_mut(&mut self) -> &mut PlayerPipeline {
        let slot = self.active_slot();
        self.pipeline_for_slot_mut(slot)
    }

    fn pipeline_for_slot(&self, slot: Slot) -> &PlayerPipeline {
        match slot {
            Slot::Primary => &self.primary,
            Slot::Secondary => &self.secondary,
        }
    }

    fn pipeline_for_slot_mut(&mut self, slot: Slot) -> &mut PlayerPipeline {
        match slot {
            Slot::Primary => &mut self.primary,
            Slot::Secondary => &mut self.secondary,
        }
    }

    fn active_slot(&self) -> Slot {
        lock_recover(&self.shared).active
    }

    fn is_active_slot(&self, slot: Slot) -> bool {
        self.active_slot() == slot
    }

    fn error_is_relevant_slot(&self, slot: Slot) -> bool {
        self.running_slots().any(|running| running == slot)
    }

    fn run_for_slot(&self, slot: Slot) -> Option<RunId> {
        if let Some((from, old_run)) = self.overlap()
            && from == slot
        {
            return Some(old_run);
        }
        let shared = lock_recover(&self.shared);
        (shared.active == slot)
            .then(|| shared.current.as_ref().map(|current| current.run))
            .flatten()
    }

    fn fail_playback(&mut self, run: Option<RunId>, error: BackendFailure) {
        self.retire_playback();
        if let Some(run) = run {
            push_event(&self.events, BackendEvent::Error { run, error });
        }
    }

    fn retire_playback(&mut self) {
        self.desired_playing = false;
        self.seek = SeekState::default();
        self.session_prerolled = false;
        self.awaiting_playing = false;
        self.status_fade = None;
        self.play_command_started_at = None;
        self.transition = Transition::None;
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
        {
            let mut shared = lock_recover(&self.shared);
            shared.current = None;
            shared.next = None;
            shared.active = Slot::Primary;
        }
    }

    fn message_source_is_pipeline(&self, slot: Slot, message: &gst::Message) -> bool {
        self.pipeline_for_slot(slot)
            .message_source_is_pipeline(message)
    }

    fn shutdown(&mut self) {
        self.transition = Transition::None;
        self.stop_pipeline(Slot::Primary);
        self.stop_pipeline(Slot::Secondary);
    }
}

#[instrument(skip(events))]
fn run_gstreamer_thread(
    events: Arc<Mutex<EventMailbox>>,
    ready: SyncSender<Result<Sender<WorkerInput>, String>>,
    processing: ProcessingRequest,
) {
    let startup_started_at = Instant::now();
    if let Err(error) = ensure_gstreamer_initialized() {
        let _ = ready.send(Err(format!("GStreamer init failed: {error}")));
        return;
    }

    // These clock corrections and device underruns are debug-log warnings, not
    // bus messages. Queue them so diagnostic file/terminal I/O cannot block audio.
    let (audio_warning_sender, audio_warnings) = sync_channel(32);
    let mut engine = GstEngine::new(Arc::clone(&events));
    let warning_wake = engine.input_sender.clone();
    let audio_log = gst::log::add_log_function(move |category, level, _, _, _, object, message| {
        if level <= gst::DebugLevel::Warning
            && matches!(
                category.name(),
                "audiobasesink" | "pulse" | "opensles_ringbuffer"
            )
            && let Some(message) = message.get()
        {
            if audio_warning_sender
                .try_send(AudioWarning {
                    category: category.name().to_string(),
                    element: object.map(|object| object.to_string()),
                    message: message.to_string(),
                    captured_at: Instant::now(),
                })
                .is_ok()
            {
                let _ = warning_wake.send(WorkerInput::Wake);
            }
        }
    });
    for category in ["audiobasesink", "pulse", "opensles_ringbuffer"] {
        gst::log::set_threshold_for_name(category, gst::DebugLevel::Warning);
    }

    engine.set_processing(processing);
    if ready.send(Ok(engine.input_sender.clone())).is_err() {
        gst::log::remove_log_function(audio_log);
        return;
    }
    info!(
        elapsed_ms = startup_started_at.elapsed().as_millis(),
        "GStreamer playback backend is ready"
    );

    loop {
        let first = match engine.next_deadline() {
            Some(wait) => engine.inputs.recv_timeout(wait).ok(),
            None => engine.inputs.recv().ok(),
        };
        let inputs = first.into_iter().chain(engine.inputs.try_iter()).collect();
        if !engine.handle_inputs(inputs) {
            break;
        }
        for warning in audio_warnings.try_iter() {
            engine.log_audio_warning(warning);
        }
        engine.tick();
    }
    engine.shutdown();
    gst::log::remove_log_function(audio_log);
}
pub(super) fn handle_about_to_finish(
    pipeline: &gst::Element,
    shared: &Arc<Mutex<SharedBackendState>>,
    gapless: &Arc<Mutex<GaplessPlayback>>,
    queued_loudness: &SharedQueuedLoudness,
    trust_invalid_certificate: &AtomicBool,
    slot: Slot,
    id: PipelineId,
) {
    if !about_to_finish_may_query(&lock_recover(shared), slot, id) {
        return;
    }
    let has_position = pipeline
        .query_position::<gst::ClockTime>()
        .is_some_and(|position| position > gst::ClockTime::ZERO);
    let can_preload = has_position || {
        let (_, current, pending) = pipeline.state(gst::ClockTime::ZERO);
        current == gst::State::Playing || pending == gst::State::Playing
    };
    let action = about_to_finish_action_for_pipeline(
        &mut lock_recover(shared),
        &mut lock_recover(gapless),
        slot,
        id,
        can_preload,
    );

    match action {
        AboutToFinishAction::Preload(next) => {
            info!(
                next_run = %next.run,
                uri = %next.stream.redacted_uri(),
                "preloading gapless next stream"
            );
            *queued_loudness
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = next.stream.loudness.clone();
            trust_invalid_certificate
                .store(next.stream.trust_invalid_certificate(), Ordering::SeqCst);
            pipeline.set_property("uri", next.stream.uri());
        }
        AboutToFinishAction::Ignore => {}
    }
}

fn about_to_finish_may_query(shared: &SharedBackendState, slot: Slot, id: PipelineId) -> bool {
    shared.pipeline_is_current(slot, id)
        && shared
            .current
            .as_ref()
            .is_some_and(|current| current.stream.allows_preloading)
}

fn about_to_finish_action_for_pipeline(
    shared: &mut SharedBackendState,
    gapless: &mut GaplessPlayback,
    slot: Slot,
    id: PipelineId,
    can_preload: bool,
) -> AboutToFinishAction {
    if !shared.pipeline_is_current(slot, id) {
        return AboutToFinishAction::Ignore;
    }
    if !can_preload {
        gapless.about_to_finish_pending = true;
        return AboutToFinishAction::Ignore;
    }
    about_to_finish_action(shared, gapless)
}

pub(super) fn about_to_finish_action(
    shared: &mut SharedBackendState,
    gapless: &mut GaplessPlayback,
) -> AboutToFinishAction {
    if gapless.pending.is_some() {
        // The next source can finish reading before its stream-start arrives.
        gapless.about_to_finish_pending = true;
        return AboutToFinishAction::Ignore;
    }

    let Some(next) = shared.next.as_ref() else {
        gapless.about_to_finish_pending = true;
        gapless.next_needed = shared.current.as_ref().map(|current| current.run);
        return AboutToFinishAction::Ignore;
    };

    if !gapless_preload_should_run(shared, next, gapless.bitstream) {
        gapless.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    }

    if !gapless_preload_source_is_supported(next.stream.uri()) {
        debug!(
            next_run = %next.run,
            uri = %next.stream.redacted_uri(),
            "skipping gapless preload for non-local stream"
        );
        gapless.about_to_finish_pending = false;
        return AboutToFinishAction::Ignore;
    }

    gapless.pending = Some(next.clone());
    gapless.about_to_finish_pending = false;
    AboutToFinishAction::Preload(Box::new(next.clone()))
}

fn gapless_preload_should_run(
    shared: &SharedBackendState,
    next: &PreparedNext,
    bitstream: bool,
) -> bool {
    let Some(current) = shared.current.as_ref() else {
        return false;
    };
    (next.transition == NextTransition::Gapless || bitstream)
        && next.stream.allows_preloading
        && current.stream.allows_preloading
}

pub(super) fn gapless_preload_source_is_supported(uri: &str) -> bool {
    uri.starts_with("file://") || uri.starts_with("http://") || uri.starts_with("https://")
}
fn inactive_slot(slot: Slot) -> Slot {
    match slot {
        Slot::Primary => Slot::Secondary,
        Slot::Secondary => Slot::Primary,
    }
}

pub(super) fn push_event(events: &Arc<Mutex<EventMailbox>>, event: BackendEvent) {
    let waker = lock_recover(events).push(event);
    if let Some(waker) = waker {
        waker();
    }
}
fn clock_millis(clock_time: gst::ClockTime) -> u64 {
    clock_time.mseconds()
}
fn stream_uri_scheme(uri: &str) -> &str {
    uri.split_once(':')
        .map(|(scheme, _)| scheme)
        .unwrap_or("unknown")
}
