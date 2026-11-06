use super::*;

impl GstEngine {
    pub(super) fn handle_message(&mut self, slot: Slot, id: PipelineId, message: &gst::Message) {
        if !lock_recover(&self.shared).pipeline_is_live(slot, id) {
            return;
        }
        if audio_processing::effects::EffectsBin::is_change_message(message) {
            self.poll_processing();
            return;
        }
        use gst::MessageView;
        if self.is_active_slot(slot)
            && let Some(peak_db) = self.pipeline_for_slot(slot).processing_peak(message)
            && let Some(output) = self.pipeline_for_slot(slot).processing_output()
        {
            push_event(
                &self.events,
                BackendEvent::ProcessingMeter {
                    output,
                    peak_db,
                    spectrum_db: Vec::new(),
                },
            );
        }

        let preroll_message = matches!(message.view(), MessageView::AsyncDone(_))
            || matches!(message.view(), MessageView::StateChanged(state)
                if state.current() == gst::State::Paused
                    && state.pending() == gst::State::VoidPending);
        if preroll_message
            && self.message_source_is_pipeline(slot, message)
            && let Err(error) = self.pipeline_for_slot(slot).complete_seek()
        {
            if self.incoming_matches(slot, id) {
                self.fail_incoming(slot, id, error);
            } else if self.is_active_slot(slot) {
                self.seek = SeekState::default();
                push_event(
                    &self.events,
                    BackendEvent::OperationFailed {
                        run: self.timing_run_id(),
                        error,
                    },
                );
                let target = if self.desired_playing {
                    gst::State::Playing
                } else {
                    gst::State::Paused
                };
                if let Err(error) = self.active_pipeline().set_state(target) {
                    self.fail_playback(self.timing_run_id(), error);
                } else if let Some(position) = self.active_pipeline().position() {
                    self.push_position(clock_millis(position));
                }
            } else if self.pending_handoff_matches(slot, id) {
                self.fail_handoff(slot, id, error);
            } else if self.error_is_relevant_slot(slot) {
                self.fail_playback(self.run_for_slot(slot), error);
            }
            return;
        }

        match message.view() {
            MessageView::StreamStart(_) if self.message_source_is_pipeline(slot, message) => {
                let initial = !self.pipeline_for_slot(slot).initial_stream_started;
                self.pipeline_for_slot_mut(slot).initial_stream_started = true;
                // Queuing the next URI can precede the current item's first message.
                if self.is_active_slot(slot) && !initial {
                    self.handle_stream_start();
                }
                return;
            }
            MessageView::Buffering(buffering) => {
                self.handle_buffering(
                    slot,
                    id,
                    buffering.percent().clamp(0, 100) as u8,
                    buffering.buffering_stats().0,
                );
                return;
            }
            MessageView::NewClock(clock) => {
                debug!(?slot, clock = ?clock.clock().map(|clock| clock.name()), "GStreamer playback clock selected");
            }
            // The documented recovery: pausing and resuming lets the pipeline select a clock.
            MessageView::ClockLost(_) => {
                if let Err(error) = self.pipeline_for_slot(slot).recover_clock() {
                    warn!(?slot, %error, "could not recover the GStreamer playback clock");
                }
            }
            MessageView::Latency(_) => self.pipeline_for_slot(slot).recalculate_latency(),
            MessageView::Warning(warning) => {
                warn!(?slot, message = %warning.error(), details = ?warning.debug(), "GStreamer playback warning");
            }
            _ => {}
        }

        if self.pending_handoff_matches(slot, id) {
            match message.view() {
                MessageView::Error(error) => {
                    let output = self.pipeline_for_slot(slot).audio_output_factory();
                    let details = gstreamer_playback_error(
                        message,
                        "prepared playback handoff",
                        output.as_deref(),
                    )
                    .unwrap_or_else(|| BackendFailure::new(error.error().to_string()));
                    self.fail_handoff(slot, id, details);
                }
                MessageView::StateChanged(state)
                    if self.message_source_is_pipeline(slot, message)
                        && state.current() == gst::State::Playing
                        && state.pending() == gst::State::VoidPending =>
                {
                    self.confirm_handoff(slot, id);
                }
                _ => {}
            }
            return;
        }

        match message.view() {
            _ if preroll_message
                && self.incoming_matches(slot, id)
                && self.message_source_is_pipeline(slot, message) =>
            {
                self.handle_incoming_async_done(slot, id);
                return;
            }
            MessageView::Error(error) if self.incoming_matches(slot, id) => {
                let output = self.pipeline_for_slot(slot).audio_output_factory();
                let details =
                    gstreamer_playback_error(message, "prepared playback", output.as_deref())
                        .unwrap_or_else(|| BackendFailure::new(error.error().to_string()));
                self.fail_incoming(slot, id, details);
                return;
            }
            _ => {}
        }

        match message.view() {
            MessageView::StateChanged(state)
                if self.message_source_is_pipeline(slot, message) && self.is_active_slot(slot) =>
            {
                // Synchronous prerolls and live sources post no ASYNC_DONE for a queued seek.
                if preroll_message
                    && self.active_pipeline().has_prerolled()
                    && !self.active_pipeline().seek_in_progress()
                    && self.seek.queued.is_some()
                {
                    self.handle_async_done();
                    return;
                }
                if let Some(started_at) = self.play_command_started_at {
                    let run = self.timing_run_id();
                    debug!(
                        run = run.map(RunId::get).unwrap_or_default(),
                        ?slot,
                        old = ?state.old(),
                        current = ?state.current(),
                        pending = ?state.pending(),
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "GStreamer startup state changed"
                    );
                }
                let playback_state = match state.current() {
                    gst::State::Null | gst::State::Ready => BackendState::Stopped,
                    gst::State::Paused => BackendState::Paused,
                    gst::State::Playing => BackendState::Playing,
                    gst::State::VoidPending => BackendState::Buffering,
                };
                self.handle_state_changed(playback_state);
            }
            MessageView::AsyncDone(_)
                if self.is_active_slot(slot) && self.message_source_is_pipeline(slot, message) =>
            {
                if let Some(started_at) = self.play_command_started_at {
                    let run = self.timing_run_id();
                    debug!(
                        run = run.map(RunId::get).unwrap_or_default(),
                        ?slot,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "GStreamer startup async done"
                    );
                }
                self.handle_async_done();
            }
            MessageView::Tag(tag) if self.is_active_slot(slot) => {
                self.log_stream_diagnostics(slot, &tag.tags());
            }
            MessageView::DurationChanged(_) if self.is_active_slot(slot) => {
                if !self.seek_busy()
                    && let Some(duration) = self.active_pipeline().duration()
                {
                    self.push_physical_duration(clock_millis(duration));
                }
            }
            MessageView::Eos(_) => self.handle_end(slot),
            MessageView::Error(error_message) => {
                let output = self.pipeline_for_slot(slot).audio_output_factory();
                let error = gstreamer_playback_error(message, "playback", output.as_deref())
                    .unwrap_or_else(|| BackendFailure::new(error_message.error().to_string()));
                let source = message
                    .src()
                    .map(|source| source.name().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                let active_slot = self.active_slot();
                let relevant = self.error_is_relevant_slot(slot);
                error!(
                    message = %error,
                    %source,
                    ?slot,
                    ?active_slot,
                    relevant,
                    "GStreamer playback error"
                );
                if relevant
                    && (self.handle_gapless_preload_error(slot, error_message, &error)
                        || self.handle_crossfade_next_error(slot, &error))
                {
                    return;
                }
                if relevant {
                    let run = self.run_for_slot(slot).or_else(|| self.timing_run_id());
                    self.fail_playback(run, error);
                }
            }
            _ => {}
        }
    }

    pub(super) fn log_stream_diagnostics(&self, slot: Slot, tags: &gst::TagListRef) {
        let codec = tags
            .get::<gst::tags::AudioCodec>()
            .map(|value| value.get().to_string())
            .or_else(|| {
                tags.get::<gst::tags::Codec>()
                    .map(|value| value.get().to_string())
            })
            .or_else(|| {
                tags.get::<gst::tags::ContainerFormat>()
                    .map(|value| value.get().to_string())
            });
        let bitrate = tags
            .get::<gst::tags::Bitrate>()
            .map(|value| value.get())
            .or_else(|| {
                tags.get::<gst::tags::NominalBitrate>()
                    .map(|value| value.get())
            })
            .map(|bits_per_second| bits_per_second / 1_000);
        if codec.is_none() && bitrate.is_none() {
            return;
        }
        let run = self.run_for_slot(slot).or_else(|| self.timing_run_id());
        debug!(
            run = run.map(RunId::get).unwrap_or_default(),
            codec = codec.as_deref().unwrap_or("unknown"),
            reported_bitrate_kbps = bitrate,
            "received GStreamer stream metadata"
        );
    }

    pub(super) fn handle_gapless_preload_error(
        &mut self,
        slot: Slot,
        message: &gst::message::Error,
        error: &BackendFailure,
    ) -> bool {
        if !self.is_active_slot(slot) {
            return false;
        }
        let Some(uri) = message
            .details()
            .and_then(|details| details.get::<&str>("uri").ok())
        else {
            return false;
        };
        {
            let shared = lock_recover(&self.shared);
            // A pending item alone cannot identify the failing stream, including repeat.
            if shared
                .current
                .as_ref()
                .is_none_or(|item| item.stream.uri() == uri)
            {
                return false;
            }
            // The failed item can report again while its drain is under way.
            if self.pipeline_for_slot(slot).draining() {
                return true;
            }
            let gapless = lock_recover(self.pipeline_for_slot(slot).gapless().unwrap());
            if gapless
                .pending
                .as_ref()
                .is_none_or(|item| item.stream.uri() != uri)
            {
                return false;
            }
        }
        let Some((current, pending)) = self.cancel_gapless_pending() else {
            return false;
        };
        warn!(
            next_run = %pending.run,
            error = %error,
            "gapless next stream failed before commit; finishing the current stream"
        );
        push_event(
            &self.events,
            BackendEvent::NextPreparationFailed {
                current_run: current.run,
                next_run: pending.run,
                error: error.clone(),
            },
        );
        // The current run ends through the drained EOS, so core skips the failed item.
        self.pipeline_for_slot(slot).drain();
        true
    }

    pub(super) fn handle_crossfade_next_error(
        &mut self,
        slot: Slot,
        error: &BackendFailure,
    ) -> bool {
        let Some((from, old_run)) = self.overlap().filter(|(from, _)| *from == slot) else {
            return false;
        };
        warn!(%error, old_run = %old_run, ?from, "outgoing crossfade tail failed");
        self.finish_overlap();
        true
    }

    pub(super) fn handle_stream_start(&mut self) {
        let started = (|| {
            let mut shared = lock_recover(&self.shared);
            let gapless = self.pipeline_for_slot(shared.active).gapless()?;
            let mut gapless = lock_recover(gapless);
            let item = gapless.pending.take()?;
            let old_run = shared.current.as_ref()?.run;
            if shared.next.as_ref() == Some(&item) {
                shared.next = None;
            }
            shared.current = Some(PreparedRun::from_next(&item));
            Some((old_run, item.run, item.stream))
        })();
        self.handle_stream_started_run(started);
    }

    pub(super) fn handle_stream_started_run(
        &mut self,
        started: Option<(RunId, RunId, PreparedStream)>,
    ) {
        let Some((old_run, new_run, stream)) = started else {
            return;
        };
        info!(
            old_run = %old_run,
            new_run = %new_run,
            "gapless stream started"
        );
        self.seek = SeekState::default();
        self.ended_run = None;
        self.last_position_tick = Instant::now();
        self.pipeline_for_slot_mut(self.active_slot())
            .activate_stream(&stream);
        push_event(
            &self.events,
            BackendEvent::Transitioned { old_run, new_run },
        );
        if self.active_pipeline().is_bitstream() {
            push_event(
                &self.events,
                BackendEvent::Bitstream {
                    run: new_run,
                    active: true,
                },
            );
        }
        push_event(
            &self.events,
            BackendEvent::Position {
                run: new_run,
                millis: 0,
            },
        );
        self.push_seekable();
    }

    pub(super) fn handle_state_changed(&mut self, state: BackendState) {
        if state == BackendState::Playing {
            self.awaiting_playing = false;
        }
        // Starting sessions and flushing seeks pass through Paused on their way to playing.
        if self.desired_playing
            && matches!(state, BackendState::Stopped | BackendState::Paused)
            && (self.awaiting_playing || self.seek_busy())
        {
            return;
        }
        if state == BackendState::Paused && !self.desired_playing {
            self.sample_pause_position();
        }
        self.push_state(state);
    }

    pub(super) fn seek_busy(&self) -> bool {
        self.seek.in_flight.is_some()
            || self.seek.queued.is_some()
            || self.active_pipeline().seek_in_progress()
    }

    pub(super) fn handle_buffering(
        &mut self,
        slot: Slot,
        id: PipelineId,
        percent: u8,
        mode: gst::BufferingMode,
    ) {
        if self.is_active_slot(slot) && percent < 100 && self.gapless_preload_near_end() {
            debug!(
                percent,
                "ignoring buffering while gapless handoff is pending near end"
            );
            return;
        }
        if let Err(error) = self.pipeline_for_slot(slot).set_buffering(percent, mode) {
            if self.fail_handoff(slot, id, error.clone())
                || self.handle_crossfade_next_error(slot, &error)
            {
                return;
            }
            if self.incoming_matches(slot, id) {
                self.fail_incoming(slot, id, error);
                return;
            }
            self.fail_playback(self.run_for_slot(slot), error);
            return;
        }
        // Both halves of a crossfade wait while either one refills.
        if let Some((from, _)) = self.overlap()
            && (slot == from || self.is_active_slot(slot))
        {
            let other = if slot == from {
                self.active_slot()
            } else {
                from
            };
            let held = self.pipeline_for_slot(slot).is_buffering();
            if let Err(error) = self.pipeline_for_slot(other).set_held(held)
                && !self.handle_crossfade_next_error(other, &error)
            {
                self.fail_playback(self.timing_run_id(), error);
                return;
            }
        }
        if matches!(percent, 0 | 100) {
            debug!(
                ?slot,
                pipeline_id = id.0,
                percent,
                ?mode,
                "GStreamer buffering"
            );
        }
        if !self.is_active_slot(slot) {
            return;
        }
        if let Some(run) = self.timing_run_id() {
            push_event(&self.events, BackendEvent::Buffering { run, percent });
        }
        if self.desired_playing && self.active_pipeline().is_buffering() {
            self.push_state(BackendState::Buffering);
        }
    }

    pub(super) fn gapless_preload_near_end(&self) -> bool {
        if !self.active_pipeline().has_pending_gapless() {
            return false;
        }
        let Some(position) = self.active_pipeline().position() else {
            return false;
        };
        let Some(duration) = self.active_pipeline().duration() else {
            return false;
        };
        let position_ms = clock_millis(position);
        let duration_ms = clock_millis(duration);
        let remaining_ms = self
            .active_pipeline()
            .logical_remaining(position_ms, duration_ms);
        duration_ms > 0 && position_ms > 0 && remaining_ms < GAPLESS_BUFFERING_IGNORE_REMAINING_MS
    }

    /// A preroll finished: send the queued target, or finish the seek that ran.
    pub(super) fn handle_async_done(&mut self) {
        self.push_seekable();
        self.session_prerolled = true;
        if self.active_pipeline().seek_in_progress() {
            return;
        }
        self.seek.in_flight = None;
        if let Some(target) = self.seek.queued.take() {
            let run = self.timing_run_id();
            let Err(error) = self.send_seek(target) else {
                return;
            };
            if !self.active_pipeline().has_session() {
                if let Some(run) = run {
                    push_event(&self.events, BackendEvent::Error { run, error });
                }
                return;
            }
            warn!(%error, target_millis = target, "queued seek failed; playing from the current position");
            let state = if self.desired_playing {
                gst::State::Playing
            } else {
                gst::State::Paused
            };
            if let Err(error) = self.active_pipeline().set_state(state) {
                self.fail_playback(run, error);
                return;
            }
        }
        if let Some(position) = self.active_pipeline().position() {
            self.push_position(clock_millis(position));
        }
    }

    pub(super) fn push_state(&mut self, state: BackendState) {
        let state = if self.desired_playing
            && self.active_pipeline().is_buffering()
            && matches!(state, BackendState::Playing | BackendState::Paused)
        {
            BackendState::Buffering
        } else {
            state
        };
        let run = self.timing_run_id();
        if state == BackendState::Playing
            && let Some(started_at) = self.play_command_started_at.take()
        {
            info!(
                run = run.map(RunId::get).unwrap_or_default(),
                elapsed_ms = started_at.elapsed().as_millis(),
                "GStreamer playback reached playing"
            );
            if let Some(run) = run {
                push_event(&self.events, BackendEvent::Started { run });
            }
        }
        if let Some(run) = run {
            push_event(&self.events, BackendEvent::State { run, state });
        }
    }

    pub(super) fn handle_end(&mut self, slot: Slot) {
        let pipeline_id = lock_recover(&self.shared).pipeline_id(slot);
        debug!(
            ?slot,
            pipeline_id = pipeline_id.map(|id| id.0),
            run = self.run_for_slot(slot).map(RunId::get),
            "GStreamer end of stream"
        );
        if self.overlap().is_some_and(|(from, _)| from == slot) {
            self.finish_overlap();
            return;
        }
        if self.gapless_handoff_waits_for_confirmation(slot) {
            return;
        }
        if self.desired_playing && self.is_active_slot(slot) && self.promote_prepared_gapless() {
            return;
        }
        if self.is_active_slot(slot) {
            self.clear_incoming_candidate_at_end(slot);
            let run = self.timing_run_id();
            info!(
                run = run.map(RunId::get).unwrap_or_default(),
                "playback reached end"
            );
            if let Some(run) = run {
                self.emit_ended_once(run);
            }
        }
    }
}
