use super::*;

impl Player {
    pub(super) fn current_stream_resolved(&mut self, run: RunId, mut stream: PreparedStream) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        stream.row = Some(current.row.clone());
        if !current.desired_playing {
            current.status = TransportStatus::Paused;
            current.resolved_stream = Some(stream);
            self.dirty = true;
            return;
        }
        current.status = TransportStatus::Buffering;
        current.resolved_stream = Some(stream.clone());
        current.backend_loaded = true;
        let next = self.prepared_next(run);

        self.send_backend(BackendCommand::ConfigureAudio(self.settings.clone().into()));
        self.send_backend(BackendCommand::Start {
            run,
            current: stream,
            next,
            start_position_millis: self.sequence.progress_millis(),
        });

        self.dirty = true;
    }

    pub(super) fn prepared_next(&self, current_run: RunId) -> Option<PreparedNext> {
        self.next_plan.as_ref().and_then(|plan| {
            if plan.current_run != current_run {
                return None;
            }
            let NextResolution::Ready(stream) = &plan.resolution else {
                return None;
            };
            Some(PreparedNext::new(
                plan.next_run,
                stream.clone(),
                plan.transition,
            ))
        })
    }

    pub(super) fn accept_started(&mut self, run: RunId, sample: &ClockSample) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        self.consecutive_skips = 0;
        current.backend_loaded = true;
        if current.started_at_unix_seconds.is_some() {
            if current.status != TransportStatus::Buffering {
                return;
            }
            current.status = TransportStatus::Playing;
            current.last_monotonic_millis = Some(sample.monotonic_millis);
            self.dirty = true;
            return;
        }
        self.dirty = true;
        self.mark_started(sample);
    }

    pub(super) fn accept_state(&mut self, run: RunId, state: BackendState, sample: &ClockSample) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        current.advance_clock(sample.monotonic_millis);
        let status = match state {
            BackendState::Stopped => TransportStatus::Stopped,
            BackendState::Buffering => TransportStatus::Buffering,
            BackendState::Paused => TransportStatus::Paused,
            BackendState::Playing => TransportStatus::Playing,
        };
        if current.status == status {
            return;
        }
        current.status = status;
        self.dirty = true;
        self.qualify_current();
        if status == TransportStatus::Paused {
            self.sequence.persist_state();
        }
    }

    pub(super) fn accept_position(&mut self, run: RunId, millis: u64, sample: &ClockSample) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        let clock_changed = current.position_observed_at_millis != sample.monotonic_millis;
        current.position_observed_at_millis = sample.monotonic_millis;
        current.advance_clock(sample.monotonic_millis);
        let playhead_millis = if current.duration_millis == 0 {
            millis
        } else {
            millis.min(current.duration_millis)
        };
        let changed = self.sequence.progress_millis() != playhead_millis;
        if !changed && current.status == TransportStatus::Paused {
            return;
        }
        self.sequence.set_progress_millis(playhead_millis);
        self.dirty |= changed || clock_changed;
        self.qualify_current();
    }

    pub(super) fn accept_duration(&mut self, run: RunId, millis: u64) {
        let Some(current) = self
            .current_run
            .as_mut()
            .filter(|current| current.id == run)
        else {
            return;
        };
        if millis == 0 || current.duration_millis == millis {
            return;
        }
        current.duration_millis = millis;
        self.dirty = true;
    }

    pub(super) fn accept_buffering(&mut self, run: RunId, percent: u8) {
        if !self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            return;
        }
        // The level is kept only while a refill is under way.
        self.buffering_percent = (percent < 100).then_some(percent);
        self.dirty = true;
    }

    pub(super) fn accept_ended(&mut self, run: RunId, sample: &ClockSample) {
        if !self
            .current_run
            .as_ref()
            .is_some_and(|current| current.id == run)
        {
            return;
        }
        let desired_playing = self
            .current_run
            .as_ref()
            .is_some_and(|current| current.desired_playing);
        self.dirty = true;
        let reserved = self.next_plan.clone();
        self.finish_current(RunEndReason::Completed, sample);
        let next = self.sequence.advance_index(true);
        if let Some(next) = next {
            self.next_plan =
                reserved.filter(|plan| Some(plan.occurrence) == self.sequence.id_at(next));
            self.promote_or_begin(next, desired_playing);
        } else {
            self.auto_dj_waiting_for_continuation = true;
            self.maybe_request_auto_dj();
        }
    }

    pub(super) fn accept_transitioned(
        &mut self,
        old_run: RunId,
        new_run: RunId,
        sample: &ClockSample,
    ) {
        let current_run = self.current_run.as_ref().map(|current| current.id);
        if current_run == Some(new_run) {
            return;
        }
        if current_run != Some(old_run) {
            self.send_backend(BackendCommand::Stop { run: new_run });
            return;
        }
        let desired_playing = self
            .current_run
            .as_ref()
            .is_none_or(|current| current.desired_playing);
        if !self
            .next_plan
            .as_ref()
            .is_some_and(|plan| plan.current_run == old_run && plan.next_run == new_run)
        {
            self.send_backend(BackendCommand::Stop { run: new_run });
            self.accept_ended(old_run, sample);
            return;
        }
        let Some(position) = self
            .next_plan
            .as_ref()
            .and_then(|plan| self.sequence.index_of(plan.occurrence))
        else {
            return;
        };
        let transitioned_stream = self.next_plan.as_ref().and_then(|plan| {
            let NextResolution::Ready(stream) = &plan.resolution else {
                return None;
            };
            Some(stream.clone())
        });
        self.dirty = true;
        self.finish_current(RunEndReason::Completed, sample);
        if !self.sequence.activate_backend(position) {
            return;
        }
        self.install_reserved_run(new_run, position, desired_playing);
        if let Some(current) = self.current_run.as_mut() {
            current.resolved_stream = transitioned_stream;
            current.backend_loaded = true;
        }
        self.mark_started(sample);
        if !desired_playing {
            self.send_backend(BackendCommand::Pause { run: new_run });
        }
    }
}
