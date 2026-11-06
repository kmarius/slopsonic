use super::*;

impl GstEngine {
    pub(super) fn tick(&mut self) {
        if let Some(format) = self.active_pipeline().bitstream_request() {
            self.start_passthrough(format);
        }
        self.sync_processing_taps();
        if self.active_pipeline().gapless().is_some_and(|gapless| {
            let gapless = lock_recover(gapless);
            gapless.about_to_finish_pending && gapless.pending.is_none()
        }) && self.desired_playing
            && self.active_pipeline().is_playing()
            && !self.active_pipeline().is_buffering()
            && !self.seek_busy()
            && self
                .active_pipeline()
                .position()
                .is_some_and(|position| position > gst::ClockTime::ZERO)
        {
            let deferred_preload = {
                let mut shared = lock_recover(&self.shared);
                self.pipeline_for_slot(shared.active)
                    .gapless()
                    .and_then(|gapless| {
                        let mut gapless = lock_recover(gapless);
                        if !gapless.about_to_finish_pending || shared.next.is_none() {
                            return None;
                        }
                        match about_to_finish_action(&mut shared, &mut gapless) {
                            AboutToFinishAction::Preload(next) => Some(*next),
                            AboutToFinishAction::Ignore => None,
                        }
                    })
            };
            if let Some(item) = deferred_preload {
                info!(
                    next_run = %item.run,
                    uri = %item.stream.redacted_uri(),
                    "preloading late gapless next stream"
                );
                if let Err(error) = self.active_pipeline_mut().set_stream(&item.stream) {
                    self.cancel_gapless_pending();
                    self.report_next_preparation_failure(item.run, error);
                }
            }
        }
        let next_needed = self
            .active_pipeline()
            .gapless()
            .and_then(|gapless| lock_recover(gapless).next_needed.take());
        if let Some(run) = next_needed {
            push_event(&self.events, BackendEvent::NextNeeded { run });
        }
        self.update_status_fade();
        if self.status_fade.is_some() {
            return;
        }
        self.maybe_start_crossfade();
        // A tail whose fade has rendered is silent; its EOS may still be far off.
        if self
            .overlap()
            .is_some_and(|(from, _)| self.pipeline_for_slot(from).transition_finished())
        {
            self.finish_overlap();
        }

        if self.last_position_tick.elapsed() >= Duration::from_millis(500) {
            self.last_position_tick = Instant::now();
            if self.seek_busy() || !self.session_prerolled {
                return;
            }
            debug!(
                run = self.timing_run_id().map(RunId::get),
                "querying GStreamer playback timing"
            );
            if tracing::enabled!(tracing::Level::DEBUG) {
                self.active_pipeline().log_output_timing();
            }
            let position = self.active_pipeline().position();
            if let Some(position) = position {
                self.push_position(clock_millis(position));
            }
            let duration = self.active_pipeline().duration();
            if let Some(duration) = duration {
                self.push_physical_duration(clock_millis(duration));
            }
            debug!(
                run = self.timing_run_id().map(RunId::get),
                position_millis = position.map(clock_millis),
                duration_millis = duration.map(clock_millis),
                buffering_percent = self.active_pipeline().buffering_percent(),
                pipeline_playing = self.active_pipeline().is_playing(),
                "GStreamer playback timing"
            );
        }
    }

    /// Positions wait for running seeks and the first preroll, so none are stale.
    pub(super) fn push_position(&mut self, millis: u64) {
        if self.seek_busy() || !self.session_prerolled {
            return;
        }
        let logical_millis = self.active_pipeline().logical_position(millis);
        if let Some(run) = self.timing_run_id() {
            if self.ended_run == Some(run) {
                return;
            }
            self.push_logical_position(logical_millis);
        }
    }

    pub(super) fn promote_prepared_gapless(&mut self) -> bool {
        if !self.desired_playing {
            return false;
        }
        let Some(incoming) = self.incoming().filter(|incoming| {
            incoming.phase == IncomingPhase::Ready
                && incoming.item.transition == NextTransition::Gapless
        }) else {
            return false;
        };
        let slot = incoming.slot;
        let id = incoming.id;
        let (volume, muted) = self.output_gain_state();
        self.pipeline_for_slot(slot)
            .set_output_volume(volume, muted);
        match self.pipeline_for_slot(slot).set_state(gst::State::Playing) {
            Ok(result) => self.begin_incoming_handoff(slot, id, result),
            Err(error) => {
                self.fail_incoming(slot, id, error);
                false
            }
        }
    }

    pub(super) fn push_logical_position(&self, millis: u64) {
        if let Some(run) = self.timing_run_id() {
            push_event(&self.events, BackendEvent::Position { run, millis });
        }
    }

    pub(super) fn push_duration(&self, millis: u64) {
        if let Some(run) = self.duration_run_id() {
            push_event(&self.events, BackendEvent::Duration { run, millis });
        }
    }

    pub(super) fn push_seekable(&self) {
        let Some(run) = self.timing_run_id() else {
            return;
        };
        let Some(seekable) = self.active_pipeline().seekable() else {
            return;
        };
        push_event(&self.events, BackendEvent::Seekable { run, seekable });
    }

    pub(super) fn push_physical_duration(&self, millis: u64) {
        self.push_duration(self.active_pipeline().logical_duration(millis));
    }

    pub(super) fn emit_ended_once(&mut self, run: RunId) {
        if self.ended_run == Some(run) {
            return;
        }
        self.ended_run = Some(run);
        if self.run_is_current(run) {
            if !self.active_pipeline().has_pending_gapless() {
                if let Some(position) = self.active_pipeline().position() {
                    self.push_logical_position(
                        self.active_pipeline()
                            .logical_position(clock_millis(position)),
                    );
                }
                if let Some(duration) = self.active_pipeline().duration() {
                    self.push_physical_duration(clock_millis(duration));
                }
            }
            self.retire_playback();
        }
        push_event(&self.events, BackendEvent::Ended { run });
    }

    pub(super) fn timing_run_id(&self) -> Option<RunId> {
        lock_recover(&self.shared)
            .current
            .as_ref()
            .map(|item| item.run)
    }

    pub(super) fn duration_run_id(&self) -> Option<RunId> {
        let shared = lock_recover(&self.shared);
        if self.pipeline_for_slot(shared.active).has_pending_gapless() {
            return None;
        }
        shared.current.as_ref().map(|item| item.run)
    }

    pub(super) fn run_is_current(&self, run: RunId) -> bool {
        self.timing_run_id() == Some(run)
    }

    pub(super) fn maybe_start_crossfade(&mut self) {
        if !self.desired_playing || self.seek_busy() || self.overlap().is_some() {
            return;
        }
        let request = (|| {
            let shared = lock_recover(&self.shared);
            let next = shared.next.clone()?;
            let NextTransition::Crossfade {
                duration_millis: crossfade_ms,
            } = next.transition
            else {
                return None;
            };
            Some((next, crossfade_ms))
        })();

        let Some((next, crossfade_ms)) = request else {
            return;
        };
        let crossfade_start_remaining = crossfade_ms;
        let Some(position) = self.active_pipeline().position() else {
            return;
        };
        let Some(duration) = self.active_pipeline().duration() else {
            return;
        };
        let position_ms = clock_millis(position);
        let duration_ms = clock_millis(duration);
        let logical_position = self.active_pipeline().logical_position(position_ms);
        let logical_duration = self.active_pipeline().logical_duration(duration_ms);
        let remaining = self
            .active_pipeline()
            .logical_remaining(position_ms, duration_ms);
        if logical_duration == 0
            || logical_position >= logical_duration
            || remaining > crossfade_start_remaining
            || logical_duration <= crossfade_start_remaining.saturating_add(1_000)
        {
            return;
        }
        let Some((to, id)) = self
            .incoming()
            .filter(|incoming| incoming.item == next && incoming.phase == IncomingPhase::Ready)
            .map(|incoming| (incoming.slot, incoming.id))
        else {
            return;
        };
        match self.pipeline_for_slot(to).set_state(gst::State::Playing) {
            Ok(result) => {
                self.begin_incoming_handoff(to, id, result);
            }
            Err(error) => self.fail_incoming(to, id, error),
        }
    }

    /// Retires the crossfade tail. The current player keeps its own fade-in curve.
    pub(super) fn finish_overlap(&mut self) {
        let Some((from, _)) = self.overlap() else {
            return;
        };
        self.transition = Transition::None;
        self.stop_pipeline(from);
        if let Err(error) = self.active_pipeline().set_held(false) {
            self.fail_playback(self.timing_run_id(), error);
            return;
        }
        self.prepare_reserved_incoming();
    }
}
