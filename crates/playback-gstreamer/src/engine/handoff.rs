use super::*;

impl GstEngine {
    pub(super) fn incoming(&self) -> Option<&IncomingPipeline> {
        match &self.transition {
            Transition::Preparing(incoming) => Some(incoming),
            _ => None,
        }
    }

    pub(super) fn incoming_mut(&mut self) -> Option<&mut IncomingPipeline> {
        match &mut self.transition {
            Transition::Preparing(incoming) => Some(incoming),
            _ => None,
        }
    }

    pub(super) fn take_incoming(&mut self) -> Option<IncomingPipeline> {
        self.incoming()?;
        let Transition::Preparing(incoming) = std::mem::take(&mut self.transition) else {
            unreachable!();
        };
        Some(incoming)
    }

    pub(super) fn activating(&self) -> Option<&PendingHandoff> {
        match &self.transition {
            Transition::Activating(pending) => Some(pending),
            _ => None,
        }
    }

    pub(super) fn take_activating(&mut self) -> Option<PendingHandoff> {
        self.activating()?;
        let Transition::Activating(pending) = std::mem::take(&mut self.transition) else {
            unreachable!();
        };
        Some(pending)
    }

    pub(super) fn overlap(&self) -> Option<(Slot, RunId)> {
        match self.transition {
            Transition::Overlap { from, old_run } => Some((from, old_run)),
            _ => None,
        }
    }

    /// The players following transport: the current one, and an outgoing crossfade tail.
    pub(super) fn running_slots(&self) -> impl Iterator<Item = Slot> + use<> {
        std::iter::once(self.active_slot()).chain(self.overlap().map(|(from, _)| from))
    }

    pub(super) fn next_pipeline_id(&mut self) -> PipelineId {
        let id = PipelineId(self.next_pipeline_number);
        self.next_pipeline_number = self.next_pipeline_number.wrapping_add(1).max(1);
        id
    }

    pub(super) fn start_pipeline(
        &mut self,
        slot: Slot,
        item: &PreparedRun,
        settings: &BackendAudioSettings,
        volume: f64,
        muted: bool,
        startup_state: gst::State,
        fade_in: Option<Duration>,
        bitstream: Option<gst::Caps>,
    ) -> Result<PipelineId, BackendFailure> {
        let id = self.next_pipeline_id();
        let previous_id = lock_recover(&self.shared).pipeline_id(slot);
        lock_recover(&self.shared).set_pipeline_id(slot, Some(id));
        let result = self.pipeline_for_slot_mut(slot).play_item(
            id,
            slot,
            item,
            settings,
            volume,
            muted,
            startup_state,
            fade_in,
            bitstream,
        );
        if result.is_err() {
            let mut shared = lock_recover(&self.shared);
            if shared.pipeline_id(slot) == Some(id) {
                shared.set_pipeline_id(slot, previous_id);
            }
        }
        result.map(|()| id)
    }

    pub(super) fn stop_pipeline(&mut self, slot: Slot) {
        lock_recover(&self.shared).set_pipeline_id(slot, None);
        self.pipeline_for_slot_mut(slot).stop();
    }

    pub(super) fn clear_incoming(&mut self) {
        if let Some(incoming) = self.take_incoming() {
            self.stop_pipeline(incoming.slot);
        }
    }

    pub(super) fn cancel_handoff_for_replan(&mut self) -> Option<RunId> {
        self.clear_incoming();
        let pending = self.take_activating()?;
        let ended = self.handoff_outgoing_ended(&pending);
        self.stop_pipeline(pending.incoming.slot);
        if ended {
            self.stop_pipeline(pending.from);
            Some(pending.old_run)
        } else {
            None
        }
    }

    pub(super) fn handoff_outgoing_ended(&self, pending: &PendingHandoff) -> bool {
        pending.incoming.item.transition == NextTransition::Gapless
    }

    pub(super) fn prepare_incoming(&mut self, next: &PreparedNext) {
        // The output takes one stream while passed through: no overlap or second player.
        if !next.stream.allows_preloading || self.active_pipeline().is_bitstream() {
            self.clear_incoming();
            return;
        }
        if self
            .incoming()
            .is_some_and(|incoming| incoming.item == *next)
            || self
                .activating()
                .is_some_and(|pending| pending.item() == next)
        {
            return;
        }
        if self.activating().is_some() {
            return;
        }
        let overlapping = self.overlap().is_some();
        let context = (|| {
            let shared = lock_recover(&self.shared);
            if overlapping {
                return None;
            }
            let current = shared.current.as_ref()?;
            if !current.stream.allows_preloading {
                return None;
            }
            let should_prepare = match next.transition {
                NextTransition::Crossfade { .. } => true,
                NextTransition::Gapless => false,
            };
            should_prepare.then(|| {
                (
                    inactive_slot(shared.active),
                    shared.settings.clone(),
                    shared.settings.muted,
                )
            })
        })();
        let Some((slot, settings, muted)) = context else {
            self.clear_incoming();
            return;
        };

        self.clear_incoming();
        self.stop_pipeline(slot);
        let item = PreparedRun::from_next(next);
        let fade_in = fade_in(next);
        let id = match self.start_pipeline(
            slot,
            &item,
            &settings,
            settings.output_gain(),
            muted,
            gst::State::Paused,
            fade_in,
            None,
        ) {
            Ok(id) => id,
            Err(error) => {
                self.report_next_preparation_failure(next.run, error);
                return;
            }
        };
        self.transition = Transition::Preparing(IncomingPipeline {
            id,
            slot,
            item: next.clone(),
            phase: IncomingPhase::Prerolling,
        });
    }

    pub(super) fn incoming_matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.incoming()
            .is_some_and(|incoming| incoming.slot == slot && incoming.id == id)
    }

    pub(super) fn pending_handoff_matches(&self, slot: Slot, id: PipelineId) -> bool {
        self.activating()
            .is_some_and(|pending| pending.matches(slot, id))
    }

    pub(super) fn handle_incoming_async_done(&mut self, slot: Slot, id: PipelineId) {
        if self.pipeline_for_slot(slot).seek_in_progress() {
            return;
        }
        let Some(incoming) = self
            .incoming_mut()
            .filter(|incoming| incoming.slot == slot && incoming.id == id)
        else {
            return;
        };
        match incoming.phase {
            IncomingPhase::Prerolling => {
                incoming.phase = IncomingPhase::Ready;
            }
            IncomingPhase::Ready => {}
        }
    }

    pub(super) fn begin_incoming_handoff(
        &mut self,
        slot: Slot,
        id: PipelineId,
        result: gst::StateChangeSuccess,
    ) -> bool {
        if self.activating().is_some() || !self.desired_playing {
            return false;
        }
        let (from, old_run) = {
            let shared = lock_recover(&self.shared);
            let Some(old_run) = shared.current.as_ref().map(|current| current.run) else {
                return false;
            };
            (shared.active, old_run)
        };
        let ready = self.incoming().is_some_and(|incoming| {
            incoming.slot == slot && incoming.id == id && incoming.phase == IncomingPhase::Ready
        });
        if !ready {
            return false;
        }
        let Some(incoming) = self.take_incoming() else {
            return false;
        };
        self.transition = Transition::Activating(PendingHandoff {
            incoming,
            from,
            old_run,
        });
        match result {
            gst::StateChangeSuccess::Async | gst::StateChangeSuccess::NoPreroll => true,
            gst::StateChangeSuccess::Success => self.confirm_handoff(slot, id),
        }
    }

    pub(super) fn confirm_handoff(&mut self, slot: Slot, id: PipelineId) -> bool {
        if self.pipeline_for_slot(slot).is_buffering() {
            return false;
        }
        if !self.desired_playing {
            self.cancel_unconfirmed_handoff_for_pause();
            return false;
        }
        let Some(pending) = self
            .activating()
            .filter(|pending| pending.matches(slot, id))
        else {
            return false;
        };
        let (still_current, target_is_live) = {
            let shared = lock_recover(&self.shared);
            let target_is_live = shared.pipeline_is_live(slot, id);
            (
                shared.active == pending.from
                    && shared.current.as_ref().map(|current| current.run) == Some(pending.old_run)
                    && target_is_live
                    && shared.next.as_ref() == Some(&pending.incoming.item),
                target_is_live,
            )
        };
        if !still_current {
            self.transition = Transition::None;
            if target_is_live {
                self.stop_pipeline(slot);
            }
            return false;
        }
        let Some(PendingHandoff {
            incoming,
            from,
            old_run,
        }) = self.take_activating()
        else {
            return false;
        };
        match incoming.item.transition {
            NextTransition::Gapless => {
                self.commit_prepared_gapless_handoff(incoming, from, old_run)
            }
            NextTransition::Crossfade { duration_millis } => {
                self.commit_crossfade_handoff(incoming, from, old_run, duration_millis);
            }
        }
        true
    }

    pub(super) fn fail_handoff(
        &mut self,
        slot: Slot,
        id: PipelineId,
        error: BackendFailure,
    ) -> bool {
        if !self.pending_handoff_matches(slot, id) {
            return false;
        }
        let Some(pending) = self.take_activating() else {
            return false;
        };
        let ended = self.handoff_outgoing_ended(&pending);
        self.stop_pipeline(pending.incoming.slot);
        self.report_next_preparation_failure(pending.incoming.item.run, error);
        if ended {
            self.emit_ended_once(pending.old_run);
        }
        true
    }

    pub(super) fn cancel_unconfirmed_handoff_for_pause(&mut self) {
        if self.activating().is_none() {
            return;
        }
        if let Some(old_run) = self.cancel_handoff_for_replan() {
            self.emit_ended_once(old_run);
        }
    }

    pub(super) fn clear_incoming_candidate_at_end(&mut self, slot: Slot) {
        let target = match &self.transition {
            Transition::Activating(pending) if pending.from == slot => Some(pending.incoming.slot),
            Transition::Preparing(incoming) if self.is_active_slot(slot) => Some(incoming.slot),
            _ => None,
        };
        if let Some(target) = target {
            self.transition = Transition::None;
            self.stop_pipeline(target);
        }
    }

    pub(super) fn gapless_handoff_waits_for_confirmation(&self, slot: Slot) -> bool {
        self.activating().is_some_and(|pending| {
            pending.incoming.item.transition == NextTransition::Gapless && pending.from == slot
        })
    }

    pub(super) fn commit_prepared_gapless_handoff(
        &mut self,
        incoming: IncomingPipeline,
        from: Slot,
        old_run: RunId,
    ) {
        let slot = incoming.slot;
        self.stop_pipeline(from);
        let (volume, muted) = self.output_gain_state();
        self.pipeline_for_slot(slot)
            .set_output_volume(volume, muted);
        {
            let mut shared = lock_recover(&self.shared);
            shared.active = slot;
            shared.current = Some(PreparedRun::from_next(&incoming.item));
            shared.next = None;
        }
        let new_run = incoming.item.run;
        self.seek = SeekState::default();
        self.session_prerolled = true;
        self.awaiting_playing = false;
        self.ended_run = None;
        self.last_position_tick = Instant::now();
        push_event(
            &self.events,
            BackendEvent::Transitioned { old_run, new_run },
        );
        push_event(
            &self.events,
            BackendEvent::Position {
                run: new_run,
                millis: 0,
            },
        );
    }

    pub(super) fn commit_crossfade_handoff(
        &mut self,
        incoming: IncomingPipeline,
        from: Slot,
        old_run: RunId,
        duration_millis: u64,
    ) {
        let to = incoming.slot;
        let new_run = incoming.item.run;
        self.pipeline_for_slot(from)
            .fade_transition_out(Duration::from_millis(duration_millis));
        {
            let mut shared = lock_recover(&self.shared);
            shared.next = None;
            shared.active = to;
            shared.current = Some(PreparedRun::from_next(&incoming.item));
        }
        self.transition = Transition::Overlap { from, old_run };
        self.seek = SeekState::default();
        self.session_prerolled = true;
        self.awaiting_playing = false;
        self.ended_run = None;
        push_event(
            &self.events,
            BackendEvent::Transitioned { old_run, new_run },
        );
        push_event(
            &self.events,
            BackendEvent::Position {
                run: new_run,
                millis: 0,
            },
        );
    }

    pub(super) fn fail_incoming(&mut self, slot: Slot, id: PipelineId, error: BackendFailure) {
        if !self.incoming_matches(slot, id) {
            return;
        }
        let Some(incoming) = self.take_incoming() else {
            return;
        };
        self.stop_pipeline(slot);
        self.report_next_preparation_failure(incoming.item.run, error);
    }

    pub(super) fn report_next_preparation_failure(&self, next_run: RunId, error: BackendFailure) {
        let current_run = self.timing_run_id();
        warn!(
            %error,
            current_run = current_run.map(RunId::get),
            next_run = %next_run,
            "next-stream preparation failed; retaining the cold-start fallback"
        );
        if let Some(current_run) = current_run {
            push_event(
                &self.events,
                BackendEvent::NextPreparationFailed {
                    current_run,
                    next_run,
                    error: error,
                },
            );
        }
    }
}
