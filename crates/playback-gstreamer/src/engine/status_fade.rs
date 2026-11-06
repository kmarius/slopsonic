use super::*;

impl GstEngine {
    pub(super) fn start_status_pause(&mut self) -> Result<(), BackendFailure> {
        if !self.desired_playing {
            return Ok(());
        }
        self.desired_playing = false;
        let previous_fade = self.status_fade.take();
        self.cancel_unconfirmed_handoff_for_pause();
        let (volume, muted, enabled) = self.status_fade_gain_settings();
        debug!(
            fade_on_play_stop = enabled,
            volume_gain = volume,
            muted,
            "GStreamer pause requested"
        );
        if !self.active_pipeline().has_session() {
            self.push_state(BackendState::Paused);
            return Ok(());
        }
        if !enabled
            || muted
            || volume <= 0.0
            || self.active_pipeline().is_buffering()
            || self.seek_busy()
        {
            self.pause_current_pipeline()?;
            return Ok(());
        }
        let slot = self.active_slot();
        self.status_fade = Some(StatusFade::new(slot, StatusFadeTarget::Pause, muted));
        let start = previous_fade.is_none().then_some(volume);
        self.pipeline_for_slot(slot)
            .fade_output(start, 0.0, STATUS_FADE_DURATION);
        self.with_tail(|tail| {
            tail.fade_output(start, 0.0, STATUS_FADE_DURATION);
            Ok(())
        });
        Ok(())
    }

    /// Lets a crossfade tail follow transport; its failure retires only the tail.
    pub(super) fn with_tail(
        &mut self,
        apply: impl FnOnce(&PlayerPipeline) -> Result<(), BackendFailure>,
    ) {
        let Some((from, _)) = self.overlap() else {
            return;
        };
        if let Err(error) = apply(self.pipeline_for_slot(from)) {
            self.handle_crossfade_next_error(from, &error);
        }
    }

    pub(super) fn start_status_resume(&mut self) -> Result<(), BackendFailure> {
        self.desired_playing = true;
        let previous_fade = self.status_fade.take();
        let (volume, muted, enabled) = self.status_fade_gain_settings();
        debug!(
            fade_on_play_stop = enabled,
            volume_gain = volume,
            muted,
            "GStreamer resume requested"
        );
        if self.seek_busy() {
            self.active_pipeline().set_output_volume(volume, muted);
            if self.active_pipeline().seek_in_progress() {
                self.active_pipeline().set_state(gst::State::Playing)?;
            }
            self.push_state(BackendState::Buffering);
            return Ok(());
        }
        if !self.active_pipeline().has_session() {
            return if self.current_item().is_ok() {
                Err("No active GStreamer session to resume".into())
            } else {
                Ok(())
            };
        }
        if self.active_pipeline().is_buffering() || !enabled || muted || volume <= 0.0 {
            self.with_tail(|tail| {
                tail.set_output_volume(volume, muted);
                tail.set_state(gst::State::Playing).map(drop)
            });
        }
        if self.active_pipeline().is_buffering() {
            self.active_pipeline().set_output_volume(volume, muted);
            self.active_pipeline().set_state(gst::State::Playing)?;
            self.push_state(BackendState::Buffering);
            return Ok(());
        }
        if !enabled || muted || volume <= 0.0 {
            self.active_pipeline().set_output_volume(volume, muted);
            return self.set_active_state(gst::State::Playing);
        }
        let slot = self.active_slot();
        let start = previous_fade.is_none().then_some(0.0);
        self.pipeline_for_slot(slot)
            .fade_output(start, volume, STATUS_FADE_DURATION);
        self.with_tail(|tail| {
            tail.fade_output(start, volume, STATUS_FADE_DURATION);
            tail.set_state(gst::State::Playing).map(drop)
        });
        self.set_active_state(gst::State::Playing)?;
        self.status_fade = Some(StatusFade::new(slot, StatusFadeTarget::Playing, muted));
        Ok(())
    }

    /// Applies `state` to the active pipeline; a synchronous change is reported immediately.
    fn set_active_state(&mut self, state: gst::State) -> Result<(), BackendFailure> {
        if self.active_pipeline().set_state(state)? != gst::StateChangeSuccess::Async {
            self.handle_state_changed(if state == gst::State::Playing {
                BackendState::Playing
            } else {
                BackendState::Paused
            });
        }
        Ok(())
    }

    fn set_tail_volume(&mut self, volume: f64, muted: bool) {
        self.with_tail(|tail| {
            tail.set_output_volume(volume, muted);
            Ok(())
        });
    }

    pub(super) fn update_status_fade(&mut self) {
        let Some(fade) = self.status_fade else {
            return;
        };
        let pipeline = self.pipeline_for_slot(fade.slot);
        if !pipeline.is_buffering()
            && self.ended_run != self.timing_run_id()
            && !pipeline.fade_finished()
        {
            return;
        }

        self.status_fade = None;
        match fade.target {
            StatusFadeTarget::Pause => {
                self.pipeline_for_slot(fade.slot)
                    .set_output_volume(0.0, fade.muted);
                if let Err(error) = self.pause_current_pipeline() {
                    self.fail_playback(self.timing_run_id(), error);
                    return;
                }
            }
            StatusFadeTarget::Playing => {
                let (volume, muted) = self.output_gain_state();
                self.pipeline_for_slot(fade.slot)
                    .set_output_volume(volume, muted);
                self.set_tail_volume(volume, muted);
            }
        }
    }

    pub(super) fn pause_current_pipeline(&mut self) -> Result<(), BackendFailure> {
        self.sample_pause_position();
        self.with_tail(|tail| tail.set_state(gst::State::Paused).map(drop));
        self.set_active_state(gst::State::Paused)
    }

    pub(super) fn sample_pause_position(&mut self) {
        if let Some(position) = self.active_pipeline().position() {
            self.push_position(clock_millis(position));
        }
    }

    pub(super) fn cancel_status_fade(&mut self) -> Option<StatusFade> {
        let fade = self.status_fade.take();
        if let Some(fade) = fade {
            let (volume, muted) = self.output_gain_state();
            let volume = if self.desired_playing { volume } else { 0.0 };
            self.pipeline_for_slot(fade.slot)
                .set_output_volume(volume, muted);
            self.set_tail_volume(volume, muted);
        }
        fade
    }

    pub(super) fn status_fade_gain_settings(&self) -> (f64, bool, bool) {
        // Compressed audio has no gain to fade.
        let bitstream = self.active_pipeline().is_bitstream();
        let shared = lock_recover(&self.shared);
        (
            shared.settings.output_gain(),
            shared.settings.muted,
            shared.settings.fade_on_status_change && !bitstream,
        )
    }
}
