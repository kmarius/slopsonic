mod freedesktop {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use app_identity::{APP_ID, DISPLAY_NAME};
    use glib;
    use mpris_server::{
        LoopStatus, Metadata, PlaybackStatus, Player as MprisPlayer, Time, TrackId as MprisTrackId,
    };
    use playback::{PlaybackView, PositionDiscontinuity, RepeatMode, RunId, TransportStatus};
    use tracing::warn;

    #[derive(Clone, Debug, PartialEq)]
    struct MprisDesiredState {
        run: Option<RunId>,
        track_id: MprisTrackId,
        playback_status: PlaybackStatus,
        loop_status: LoopStatus,
        shuffle: bool,
        metadata: Metadata,
        volume: f64,
        can_play: bool,
        can_pause: bool,
        can_seek: bool,
        can_go_next: bool,
        can_go_previous: bool,
        position: Option<Time>,
    }

    impl MprisDesiredState {
        fn replace_position(&mut self, position: Option<Time>) -> bool {
            if self.position == position {
                return false;
            }
            self.position = position;
            true
        }
    }

    pub struct MediaControls {
        transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
        player: RefCell<Option<Rc<MprisPlayer>>>,
        desired: RefCell<Option<MprisDesiredState>>,
        applied: RefCell<Option<MprisDesiredState>>,
        pending_seeked: Cell<Option<PositionDiscontinuity>>,
        generation: Cell<u64>,
        running: Cell<bool>,
    }

    impl MediaControls {
        fn new(transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>) -> Self {
            Self {
                transport,
                player: RefCell::new(None),
                desired: RefCell::new(None),
                applied: RefCell::new(None),
                pending_seeked: Cell::new(None),
                generation: Cell::new(0),
                running: Cell::new(false),
            }
        }

        fn install_player(self: &Rc<Self>, player: Rc<MprisPlayer>) {
            *self.player.borrow_mut() = Some(player);
            self.applied.borrow_mut().take();
            self.pending_seeked.set(None);
            self.start_drain();
        }

        fn queue(
            self: &Rc<Self>,
            desired: MprisDesiredState,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            if self
                .pending_seeked
                .get()
                .is_some_and(|pending| Some(pending.run) != desired.run)
            {
                self.pending_seeked.set(None);
            }
            if let Some(discontinuity) = discontinuity
                && desired.run == Some(discontinuity.run)
            {
                self.pending_seeked.set(Some(discontinuity));
            }
            *self.desired.borrow_mut() = Some(desired);
            self.generation.set(self.generation.get().saturating_add(1));
            self.start_drain();
        }

        fn start_drain(self: &Rc<Self>) {
            if self.player.borrow().is_none() || self.running.replace(true) {
                return;
            }
            let adapter = Rc::clone(self);
            glib::spawn_future_local(async move {
                adapter.drain().await;
                adapter.running.set(false);
            });
        }

        async fn drain(&self) {
            loop {
                let Some(player) = self.player.borrow().as_ref().cloned() else {
                    return;
                };
                let Some(desired) = self.desired.borrow().clone() else {
                    return;
                };
                let applied = self.applied.borrow().clone();
                let generation = self.generation.get();
                let discontinuity = self.pending_seeked.take();

                apply_mpris_desired(&player, applied.as_ref(), &desired).await;
                let mut applied = desired.clone();
                if let Some(discontinuity) = discontinuity
                    && self
                        .desired
                        .borrow()
                        .as_ref()
                        .is_some_and(|current| current.run == Some(discontinuity.run))
                {
                    let position = mpris_time(discontinuity.position_millis);
                    player.set_position(position);
                    let _sent = player.seeked(position).await;
                    applied.position = Some(position);
                }
                *self.applied.borrow_mut() = Some(applied);

                if self.generation.get() == generation && self.pending_seeked.get().is_none() {
                    return;
                }
            }
        }
    }

    async fn apply_mpris_desired(
        player: &MprisPlayer,
        applied: Option<&MprisDesiredState>,
        desired: &MprisDesiredState,
    ) {
        if applied.is_none_or(|applied| applied.playback_status != desired.playback_status) {
            let _updated = player.set_playback_status(desired.playback_status).await;
        }
        if applied.is_none_or(|applied| applied.loop_status != desired.loop_status) {
            let _updated = player.set_loop_status(desired.loop_status).await;
        }
        if applied.is_none_or(|applied| applied.shuffle != desired.shuffle) {
            let _updated = player.set_shuffle(desired.shuffle).await;
        }
        if applied.is_none_or(|applied| applied.metadata != desired.metadata) {
            let _updated = player.set_metadata(desired.metadata.clone()).await;
        }
        if applied.is_none_or(|applied| applied.volume != desired.volume) {
            let _updated = player.set_volume(desired.volume).await;
        }
        if applied.is_none_or(|applied| applied.can_play != desired.can_play) {
            let _updated = player.set_can_play(desired.can_play).await;
        }
        if applied.is_none_or(|applied| applied.can_pause != desired.can_pause) {
            let _updated = player.set_can_pause(desired.can_pause).await;
        }
        if applied.is_none_or(|applied| applied.can_seek != desired.can_seek) {
            let _updated = player.set_can_seek(desired.can_seek).await;
        }
        if applied.is_none_or(|applied| applied.can_go_next != desired.can_go_next) {
            let _updated = player.set_can_go_next(desired.can_go_next).await;
        }
        if applied.is_none_or(|applied| applied.can_go_previous != desired.can_go_previous) {
            let _updated = player.set_can_go_previous(desired.can_go_previous).await;
        }
        if applied.is_none_or(|applied| applied.position != desired.position)
            && let Some(position) = desired.position
        {
            player.set_position(position);
        }
    }

    impl MediaControls {
        pub fn start(
            transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
            open_uri: impl Fn(&str) + 'static,
        ) -> Rc<Self> {
            let mpris = Rc::new(Self::new(transport));
            let setup = Rc::clone(&mpris);
            glib::spawn_future_local(async move {
                setup.install(open_uri).await;
            });
            mpris
        }

        pub fn observe(
            self: &Rc<Self>,
            playback: Option<&PlaybackView>,
            art_url: Option<String>,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            self.queue(mpris_desired_state(playback, art_url), discontinuity);
        }

        pub fn observe_position(
            self: &Rc<Self>,
            position_millis: Option<u64>,
            discontinuity: Option<PositionDiscontinuity>,
        ) {
            let position = position_millis.map(mpris_time);
            let (position_changed, desired_run) = {
                let mut desired = self.desired.borrow_mut();
                let Some(desired) = desired.as_mut() else {
                    return;
                };
                (desired.replace_position(position), desired.run)
            };

            if self
                .pending_seeked
                .get()
                .is_some_and(|pending| Some(pending.run) != desired_run)
            {
                self.pending_seeked.set(None);
            }
            let matching_discontinuity =
                discontinuity.filter(|discontinuity| desired_run == Some(discontinuity.run));
            if let Some(discontinuity) = matching_discontinuity {
                self.pending_seeked.set(Some(discontinuity));
            }

            if !position_changed && matching_discontinuity.is_none() {
                return;
            }

            if self.running.get() || self.pending_seeked.get().is_some() {
                self.generation.set(self.generation.get().saturating_add(1));
                self.start_drain();
                return;
            }

            let Some(player) = self.player.borrow().as_ref().cloned() else {
                return;
            };
            if let Some(position) = position {
                player.set_position(position);
            }
            if let Some(applied) = self.applied.borrow_mut().as_mut() {
                applied.position = position;
            }
        }

        async fn install(self: Rc<Self>, open_uri: impl Fn(&str) + 'static) {
            let player = match MprisPlayer::builder(APP_ID)
                .identity(DISPLAY_NAME)
                .desktop_entry(APP_ID)
                .supported_uri_schemes(["http", "https", "file"])
                .supported_mime_types(["audio/mpeg", "audio/flac", "audio/ogg", "audio/x-wav"])
                .can_play(true)
                .can_pause(true)
                .can_go_next(true)
                .can_go_previous(true)
                .can_seek(true)
                .can_control(true)
                .build()
                .await
            {
                Ok(player) => Rc::new(player),
                Err(error) => {
                    warn!(%error, "failed to start MPRIS server");
                    return;
                }
            };

            let transport = self.transport.clone();
            player.connect_play_pause(move |_| transport.play_pause());
            player.connect_open_uri(move |_, uri| open_uri(uri));
            let transport = self.transport.clone();
            player.connect_play(move |_| transport.play());
            let transport = self.transport.clone();
            player.connect_pause(move |_| transport.pause());
            let transport = self.transport.clone();
            player.connect_stop(move |_| transport.stop());
            let transport = self.transport.clone();
            player.connect_next(move |_| transport.next());
            let transport = self.transport.clone();
            player.connect_previous(move |_| transport.previous());
            let transport = self.transport.clone();
            let seek_mpris = Rc::clone(&self);
            player.connect_seek(move |_, offset| {
                let current = seek_mpris
                    .desired
                    .borrow()
                    .as_ref()
                    .and_then(|desired| desired.position)
                    .map_or(0, |position| (position.as_micros() / 1_000).max(0) as u64);
                let offset_millis = offset.as_micros() / 1_000;
                let target = if offset_millis.is_negative() {
                    current.saturating_sub(offset_millis.unsigned_abs())
                } else {
                    current.saturating_add(offset_millis as u64)
                };
                transport.seek_millis(target);
            });
            let position_mpris = Rc::clone(&self);
            let transport = self.transport.clone();
            player.connect_set_position(move |_, track_id, position| {
                let current_matches = position_mpris
                    .desired
                    .borrow()
                    .as_ref()
                    .is_some_and(|desired| &desired.track_id == track_id);
                if current_matches {
                    transport.seek_millis((position.as_micros() / 1_000).max(0) as u64);
                }
            });
            let transport = self.transport.clone();
            player.connect_set_volume(move |_, volume| {
                let volume = if volume.is_finite() {
                    volume.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                transport.set_volume(volume);
                transport.persist_volume(volume);
            });
            let transport = self.transport.clone();
            player.connect_set_shuffle(move |_, enabled| transport.set_shuffle(enabled));
            let transport = self.transport.clone();
            player.connect_set_loop_status(move |_, status| {
                transport.set_repeat(repeat_mode_from_mpris(status));
            });

            self.install_player(Rc::clone(&player));
            glib::spawn_future_local(async move {
                player.run().await;
            });
        }
    }

    fn mpris_desired_state(
        playback: Option<&PlaybackView>,
        art_url: Option<String>,
    ) -> MprisDesiredState {
        let has_current = playback.is_some_and(|playback| playback.transport.current.is_some());
        let has_active_run = playback.is_some_and(|playback| {
            playback
                .transport
                .current
                .as_ref()
                .is_some_and(|media| media.id.run.is_some())
                && !matches!(
                    playback.transport.state,
                    TransportStatus::Stopped | TransportStatus::Failed
                )
        });
        let can_go_next = playback.is_some_and(|playback| {
            has_current && (playback.queue.can_next || playback.controls.auto_dj_enabled)
        });
        MprisDesiredState {
            run: playback
                .and_then(|playback| playback.transport.current.as_ref())
                .and_then(|media| media.id.run),
            track_id: playback
                .and_then(|playback| playback.transport.current.as_ref())
                .map_or(MprisTrackId::NO_TRACK, |media| mpris_track_id(&media.id)),
            playback_status: playback.map_or(PlaybackStatus::Stopped, |playback| {
                mpris_playback_status(playback.transport.effective_state())
            }),
            loop_status: mpris_loop_status(
                playback.map_or(RepeatMode::Off, |playback| playback.controls.repeat_mode),
            ),
            shuffle: playback.is_some_and(|playback| playback.controls.shuffle_enabled),
            metadata: mpris_metadata(playback, art_url),
            volume: playback.map_or(1.0, |playback| playback.controls.volume.clamp(0.0, 1.0)),
            can_play: has_current,
            can_pause: has_active_run,
            can_seek: has_current && playback.is_some_and(|playback| playback.transport.can_seek),
            can_go_next,
            can_go_previous: has_current,
            position: playback
                .filter(|playback| playback.transport.current.is_some())
                .map(|playback| mpris_time(playback.transport.position_millis)),
        }
    }

    fn mpris_metadata(playback: Option<&PlaybackView>, art_url: Option<String>) -> Metadata {
        let Some(entry) = playback.and_then(|playback| playback.transport.current.as_ref()) else {
            return Metadata::builder().trackid(MprisTrackId::NO_TRACK).build();
        };
        let mut builder = Metadata::builder()
            .trackid(mpris_track_id(&entry.id))
            .title(entry.row.track.metadata.title.clone())
            .artist([entry.row.track.metadata.artist.clone()])
            .album(entry.row.track.metadata.album.clone())
            .length(Time::from_micros(
                entry
                    .row
                    .track
                    .metadata
                    .duration_millis
                    .saturating_mul(1_000),
            ));
        if let Some(art_url) = art_url {
            builder = builder.art_url(art_url);
        }
        builder.build()
    }

    fn mpris_track_id(media: &playback::CurrentMediaId) -> MprisTrackId {
        let encoded = blake3::hash(media.media_uri.as_bytes()).to_hex();
        MprisTrackId::try_from(format!(
            "/io/github/screwys/Rufin/track/{}/{encoded}",
            media.run.map_or(0, playback::RunId::get)
        ))
        .unwrap_or(MprisTrackId::NO_TRACK)
    }

    #[cfg(test)]
    fn mpris_set_position_matches(
        playback: Option<&PlaybackView>,
        track_id: &MprisTrackId,
    ) -> bool {
        playback
            .and_then(|playback| playback.transport.current.as_ref())
            .is_some_and(|entry| &mpris_track_id(&entry.id) == track_id)
    }

    fn mpris_time(position_millis: u64) -> Time {
        Time::from_millis(position_millis.min(i64::MAX as u64) as i64)
    }

    fn mpris_loop_status(repeat_mode: RepeatMode) -> LoopStatus {
        match repeat_mode {
            RepeatMode::Off => LoopStatus::None,
            RepeatMode::One => LoopStatus::Track,
            RepeatMode::All => LoopStatus::Playlist,
        }
    }

    fn repeat_mode_from_mpris(status: LoopStatus) -> RepeatMode {
        match status {
            LoopStatus::None => RepeatMode::Off,
            LoopStatus::Track => RepeatMode::One,
            LoopStatus::Playlist => RepeatMode::All,
        }
    }

    fn mpris_playback_status(state: TransportStatus) -> PlaybackStatus {
        match state {
            TransportStatus::Resolving | TransportStatus::Buffering | TransportStatus::Playing => {
                PlaybackStatus::Playing
            }
            TransportStatus::Paused => PlaybackStatus::Paused,
            TransportStatus::Stopped | TransportStatus::Failed => PlaybackStatus::Stopped,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            RepeatMode, TransportStatus, mpris_desired_state, mpris_loop_status, mpris_metadata,
            mpris_set_position_matches, mpris_time, mpris_track_id, repeat_mode_from_mpris,
        };
        use crate::test_support::test_view;
        use playback::{CurrentMediaId, RunId};

        #[test]
        fn mpris_run_paths_distinguish_duplicate_tracks_and_guard_set_position() {
            let playback = test_view(1, "Album", TransportStatus::Playing, 1_000);
            let first = playback
                .transport
                .current
                .as_ref()
                .expect("current media")
                .id
                .clone();
            let second = CurrentMediaId {
                run: Some(RunId::new(2)),
                media_uri: first.media_uri.clone(),
            };
            let first_id = mpris_track_id(&first);
            let second_id = mpris_track_id(&second);
            assert_ne!(first_id, second_id);
            let private = CurrentMediaId {
                run: first.run,
                media_uri: "https://user:secret@example.test/track?token=secret".into(),
            };
            let encoded = private
                .media_uri
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert!(!mpris_track_id(&private).to_string().contains(&encoded));

            assert!(mpris_set_position_matches(Some(&playback), &first_id));
            assert!(!mpris_set_position_matches(Some(&playback), &second_id));
        }

        #[test]
        fn mpris_exact_repeat_mapping_round_trips() {
            for repeat in [RepeatMode::Off, RepeatMode::One, RepeatMode::All] {
                assert_eq!(repeat_mode_from_mpris(mpris_loop_status(repeat)), repeat);
            }
        }

        #[test]
        fn mpris_capabilities_follow_queue_summary() {
            let mut playback = test_view(1, "Album", TransportStatus::Playing, 0);
            let exhausted = mpris_desired_state(Some(&playback), None);
            assert!(!exhausted.can_go_next);
            assert!(exhausted.can_go_previous);

            playback.queue.next_index = Some(1);
            playback.queue.can_next = true;
            let with_next = mpris_desired_state(Some(&playback), None);
            assert!(with_next.can_go_next);

            playback.queue.next_index = None;
            assert!(mpris_desired_state(Some(&playback), None).can_go_next);
        }

        #[test]
        fn mpris_metadata_updates_when_cached_art_arrives() {
            let playback = test_view(1, "Album", TransportStatus::Paused, 0);
            assert_ne!(
                mpris_metadata(Some(&playback), None),
                mpris_metadata(Some(&playback), Some("file:///tmp/cover.png".to_string()))
            );
        }

        #[test]
        fn mpris_position_update_preserves_static_desired_state() {
            let playback = test_view(1, "Album", TransportStatus::Playing, 1_000);
            let mut desired =
                mpris_desired_state(Some(&playback), Some("file:///cover.png".to_string()));
            let expected = super::MprisDesiredState {
                position: Some(mpris_time(1_500)),
                ..desired.clone()
            };

            assert!(desired.replace_position(Some(mpris_time(1_500))));
            assert_eq!(desired, expected);
            assert!(!desired.replace_position(Some(mpris_time(1_500))));
        }
    }
}

pub use freedesktop::MediaControls;
