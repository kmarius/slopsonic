use super::*;

impl Player {
    pub(super) fn apply_batch(
        &mut self,
        request: crate::playback::PlayRequest,
        _sample: &ClockSample,
    ) -> Result<(), SequenceError> {
        let replacing = matches!(request.placement, Placement::Replace { .. });
        let loading_changed = !self.queue_loading;
        self.queue_loading = true;
        if self.pending_queue.is_some() && !replacing {
            self.deferred_queue.push_back(Command::ApplyQueue(request));
            self.dirty |= loading_changed;
            return Ok(());
        }
        if replacing {
            self.deferred_queue.clear();
            self.queue_transport = None;
        }
        let completion = match request.placement {
            Placement::Replace { .. } => QueueCompletion::Replace,
            Placement::AfterCurrent => QueueCompletion::Insert {
                target: self
                    .sequence
                    .selected()
                    .map_or(QueueReorderTarget::End, |row| {
                        QueueReorderTarget::AfterOccurrence(
                            row.occurrence_id.expect("local queue entry"),
                        )
                    }),
            },
            Placement::End => QueueCompletion::Insert {
                target: QueueReorderTarget::End,
            },
        };
        self.request_queue(request, completion);
        Ok(())
    }

    pub(super) fn queue_loading(&self) -> bool {
        self.queue_loading || !self.candidate_tasks.is_empty()
    }

    pub(super) fn request_queue(
        &mut self,
        request: crate::playback::PlayRequest,
        completion: QueueCompletion,
    ) {
        let id = self.next_queue_request;
        self.next_queue_request = id.wrapping_add(1);
        self.queue_loading = true;
        self.cancel_queue_read();
        let owner = self.owner();
        let database = owner.database.clone();
        let playback = self.playback.clone();
        let shuffled = self.sequence.shuffle_enabled();
        let task = owner.runtime.spawn(async move {
            let result = request
                .prepare(&database, shuffled, super::super::random_u64())
                .await
                .map_err(super::super::string_error);
            playback.post(move |player, sample| {
                if let Err(error) = player.complete_queue(id, result, sample) {
                    player.report_error(error.to_string());
                }
            });
        });
        self.pending_queue = Some((id, completion, task));
        self.dirty = true;
    }

    pub(super) fn hydrate_queue(&mut self) {
        if self.pending_queue.is_some() || self.pending_hydration.is_some() {
            return;
        }
        let Some(entries) = self.sequence.read_request() else {
            self.dirty |= self.queue_loading;
            self.queue_loading = false;
            return;
        };
        let owner = self.owner();
        let database = owner.database.clone();
        let playback = self.playback.clone();
        self.pending_hydration = Some(owner.runtime.spawn(async move {
            let requested = entries
                .iter()
                .enumerate()
                .map(|(position, (_, entry))| (position, entry.clone()))
                .collect::<Vec<_>>();
            let result = database
                .queue_rows_for_entries(&requested, &library::ReadCancellation::new())
                .await
                .map(|rows| {
                    entries
                        .into_iter()
                        .map(|(id, _)| id)
                        .zip(rows.into_iter().map(|row| row.track.clone()))
                        .collect()
                })
                .map_err(super::super::string_error);
            playback.post(move |player, sample| player.complete_queue_metadata(result, sample));
        }));
    }

    pub(super) fn complete_queue_metadata(
        &mut self,
        result: Result<Vec<(OccurrenceId, library::TrackView)>, String>,
        _sample: &ClockSample,
    ) {
        self.pending_hydration = None;
        match result {
            Ok(rows) => self.dirty |= self.sequence.hydrate(rows),
            Err(error) => self.report_error(error),
        }
        if matches!(self.pending_queue, Some((_, QueueCompletion::Replace, _))) {
            return;
        }
        if self.current_run.is_none()
            && self.queue_transport != Some(TransportStatus::Stopped)
            && !self.restored_paused
        {
            self.begin_selected_run();
            if self.queue_transport == Some(TransportStatus::Paused) {
                self.set_playing(false);
            }
        } else {
            self.replan_next_if_changed();
        }
        if self.current_run.is_some() {
            self.queue_transport = None;
        }
    }

    pub(super) fn finish_queue_run(&mut self, sample: &ClockSample, reason: RunEndReason) {
        if let Some(run) = &self.current_run {
            self.send_backend(BackendCommand::Stop { run: run.id });
        }
        self.finish_current(reason, sample);
    }

    pub(super) fn edit_queue(&mut self, edit: impl FnOnce(&mut Sequence)) {
        edit(&mut self.sequence);
        if let (Some(run), Some(row)) = (self.current_run.as_mut(), self.sequence.selected()) {
            run.row = row.clone();
            if let Some(stream) = &mut run.resolved_stream {
                stream.row = Some(row.clone());
            }
        }
    }

    pub(super) fn changed_queue(&mut self, changed: bool, stop: bool, sample: &ClockSample) {
        self.dirty = true;
        if changed {
            if stop {
                self.queue_transport = Some(TransportStatus::Stopped);
            }
            if let Some(run) = &self.current_run {
                self.send_backend(BackendCommand::Stop { run: run.id });
            }
            self.finish_current(
                if stop {
                    RunEndReason::Stopped
                } else {
                    RunEndReason::ManualSkip
                },
                sample,
            );
            self.restored_paused = false;
            if !stop && self.queue_transport != Some(TransportStatus::Stopped) {
                self.begin_selected_run();
                if self.queue_transport == Some(TransportStatus::Paused) {
                    self.set_playing(false);
                }
            }
            self.current_media_changed = true;
        } else {
            self.replan_next_if_changed();
        }
        self.sequence.persist_state();
        if self.sequence.selected_index().is_none() {
            self.auto_dj_in_flight = None;
            self.auto_dj_waiting_for_continuation = false;
        }
        self.maybe_request_auto_dj();
    }

    pub(super) fn complete_queue(
        &mut self,
        id: u64,
        result: Result<super::super::commands::QueueBatch, String>,
        sample: &ClockSample,
    ) -> Result<bool, SequenceError> {
        if !self.accepts_queue(id) {
            return Ok(false);
        }
        let (_, completion, task) = self.pending_queue.take().unwrap();
        task.abort();
        match result {
            Err(error) => {
                self.queue_loading = false;

                self.report_error(error);
                self.dirty = true;
            }
            Ok(page) => match completion {
                QueueCompletion::Replace => {
                    self.finish_queue_run(sample, RunEndReason::ManualSkip);
                    self.sequence.add_page(page, QueueReorderTarget::End, true);
                    self.restored_paused = false;
                    self.changed_queue(true, false, sample);
                }
                QueueCompletion::Insert { target } => {
                    let advance =
                        self.auto_dj_waiting_for_continuation && self.current_run.is_none();
                    let had_current = self.sequence.selected_index().is_some();
                    self.edit_queue(|sequence| sequence.add_page(page, target, false));
                    if advance {
                        self.sequence.advance_index(false);
                        self.auto_dj_waiting_for_continuation = false;
                    }
                    self.changed_queue(advance || !had_current, false, sample)
                }
            },
        };
        if self.current_run.is_some() {
            self.queue_transport = None;
        }
        while self.pending_queue.is_none()
            && let Some(command) = self.deferred_queue.pop_front()
        {
            self.handle_command(command, sample)?;
        }
        Ok(true)
    }

    pub(super) fn activate_context(
        &mut self,
        context_id: &str,
        track_key: library::TrackKey,
        source_rank: usize,
        sample: &ClockSample,
    ) -> bool {
        let Some(index) = self
            .sequence
            .context_index(context_id, track_key, source_rank)
        else {
            return false;
        };
        self.activate_index(index, sample);
        true
    }

    pub(super) fn activate(&mut self, position: &usize, sample: &ClockSample) {
        self.activate_index(*position, sample)
    }

    pub(super) fn activate_index(&mut self, index: usize, sample: &ClockSample) {
        if index >= self.sequence.total() {
            return;
        }
        self.cancel_candidate_tasks(true);
        self.restored_paused = false;
        self.dirty = true;
        if let Some(run) = self.current_run.as_ref() {
            self.send_backend(BackendCommand::Stop { run: run.id });
        }
        self.finish_current(RunEndReason::ManualSkip, sample);
        if !self.sequence.activate_index(index) {
            return;
        }
        self.begin_selected_run();
    }

    pub(super) fn remove(&mut self, id: &usize, sample: &ClockSample) {
        self.remove_many(&QueueSelection::Positions(vec![*id]), sample)
    }
    pub(super) fn remove_many(&mut self, selection: &QueueSelection, sample: &ClockSample) {
        let previous = self
            .sequence
            .selected_index()
            .and_then(|i| self.sequence.id_at(i));
        let removing_current = previous.is_some_and(|id| match selection {
            QueueSelection::Positions(positions) => positions
                .iter()
                .any(|&i| self.sequence.id_at(i) == Some(id)),
            QueueSelection::Occurrences(ids) => ids.contains(&id.0),
        });
        if removing_current {
            self.finish_queue_run(sample, RunEndReason::ManualSkip);
        }
        self.edit_queue(|sequence| sequence.remove(selection));
        self.changed_queue(removing_current, false, sample);
    }
    pub(super) fn forget(&mut self, keys: &[library::TrackKey], sample: &ClockSample) {
        let keys = keys
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let ids = (0..self.sequence.total())
            .filter(|&position| {
                self.sequence
                    .entry_at(position)
                    .is_some_and(|entry| keys.contains(&entry.track_key))
            })
            .collect::<Vec<_>>();
        let previous = self.sequence.selected_index();
        if previous.as_ref().is_some_and(|id| ids.contains(id)) {
            self.finish_queue_run(sample, RunEndReason::Stopped)
        }
        self.edit_queue(|sequence| sequence.remove(&QueueSelection::Positions(ids.clone())));
        self.changed_queue(
            previous.is_some_and(|position| ids.contains(&position)),
            true,
            sample,
        );
    }
    pub(super) fn reorder(
        &mut self,
        selection: &QueueSelection,
        target: &QueueReorderTarget,
        sample: &ClockSample,
    ) {
        self.edit_queue(|sequence| sequence.reorder(selection, target));
        self.changed_queue(false, false, sample)
    }
    pub(super) fn insert(&mut self, input: library::QueueLoad, target: &QueueReorderTarget) {
        if self.current_run.is_none() {
            self.queue_transport.get_or_insert(TransportStatus::Stopped);
        }
        self.request_queue(
            crate::playback::PlayRequest::from_input(input, Placement::End),
            QueueCompletion::Insert {
                target: match target {
                    QueueReorderTarget::Before(i) => self
                        .sequence
                        .id_at(*i)
                        .map_or(QueueReorderTarget::End, |id| {
                            QueueReorderTarget::BeforeOccurrence(id.0)
                        }),
                    QueueReorderTarget::After(i) => self
                        .sequence
                        .id_at(*i)
                        .map_or(QueueReorderTarget::End, |id| {
                            QueueReorderTarget::AfterOccurrence(id.0)
                        }),
                    target => target.clone(),
                },
            },
        )
    }
    pub(super) fn move_after_current(&mut self, id: &usize, sample: &ClockSample) {
        let target = self
            .sequence
            .selected()
            .map_or(QueueReorderTarget::End, |row| {
                QueueReorderTarget::AfterOccurrence(row.occurrence_id.expect("local queue entry"))
            });
        self.reorder(&QueueSelection::Positions(vec![*id]), &target, sample)
    }
    pub(super) fn clear(&mut self, include_current: bool, sample: &ClockSample) {
        let previous = self.sequence.selected_index();
        if include_current {
            self.finish_queue_run(sample, RunEndReason::Stopped)
        }
        self.cancel_queue_read();
        self.queue_loading = false;
        self.deferred_queue.clear();
        let removing_current = include_current || self.current_run.is_none();
        self.edit_queue(|sequence| sequence.clear(removing_current));
        self.changed_queue(removing_current && previous.is_some(), true, sample);
    }
}
