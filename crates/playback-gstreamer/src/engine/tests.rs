use std::fs::File;
use std::io::Write;

use super::*;

const ACTIVE_PIPELINE: PipelineId = PipelineId(7);

const INCOMING_PIPELINE: PipelineId = PipelineId(8);

struct HandoffFixture {
    engine: GstEngine,
    events: Arc<Mutex<EventMailbox>>,
    old_run: RunId,
    next_run: RunId,
}

impl HandoffFixture {
    fn separate(transition: NextTransition) -> Self {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let old_run = RunId::new(1);
        let next_run = RunId::new(2);
        let (current_stream, next_stream) = (
            ResolvedStream::new("file:///music/current.flac"),
            ResolvedStream::new("file:///music/next.flac"),
        );
        let next = PreparedNext::new(next_run, next_stream, transition);
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(PreparedRun {
                run: old_run,
                stream: current_stream.into(),
            });
            shared.next = Some(next.clone());
            shared.active = Slot::Primary;
            shared.set_pipeline_id(Slot::Primary, Some(ACTIVE_PIPELINE));
            shared.set_pipeline_id(Slot::Secondary, Some(INCOMING_PIPELINE));
        }
        engine.transition = Transition::Preparing(IncomingPipeline {
            id: INCOMING_PIPELINE,
            slot: Slot::Secondary,
            item: next.clone(),
            phase: IncomingPhase::Ready,
        });
        engine.desired_playing = true;
        Self {
            engine,
            events,
            old_run,
            next_run,
        }
    }

    fn begin_separate(&mut self) {
        assert!(self.engine.begin_incoming_handoff(
            Slot::Secondary,
            INCOMING_PIPELINE,
            gst::StateChangeSuccess::Async,
        ));
    }

    fn current_run(&self) -> Option<RunId> {
        lock_recover(&self.engine.shared)
            .current
            .as_ref()
            .map(|item| item.run)
    }

    fn drain(&self) -> Vec<BackendEvent> {
        lock_recover(&self.events).drain()
    }
}

#[test]
fn repeated_stream_is_preloaded_on_the_current_pipeline() {
    let mut gapless = GaplessPlayback::default();
    let pipeline = PipelineId(5);
    let current_run = RunId::new(10);
    let repeated = PreparedStream::from(ResolvedStream::new("file:///music/repeated.flac"));
    let next = PreparedNext::new(RunId::new(11), repeated.clone(), NextTransition::Gapless);
    let mut shared = SharedBackendState::new();
    shared.active = Slot::Primary;
    shared.current = Some(PreparedRun {
        run: current_run,
        stream: repeated,
    });
    shared.next = Some(next.clone());
    shared.set_pipeline_id(Slot::Primary, Some(pipeline));

    assert_eq!(
        about_to_finish_action_for_pipeline(
            &mut shared,
            &mut gapless,
            Slot::Primary,
            pipeline,
            true
        ),
        AboutToFinishAction::Preload(Box::new(next.clone()))
    );
    assert_eq!(shared.next, Some(next.clone()));
    assert_eq!(gapless.pending, Some(next));
}

#[test]
fn fresh_playback_outputs_the_selected_gain_and_mute() {
    ensure_gstreamer_initialized().expect("initialize GStreamer");
    let directory = tempfile::tempdir().expect("playback fixture directory");
    let path = directory.path().join("initial-volume.wav");
    write_silent_wave(&path);
    let mut wav = std::fs::read(&path).unwrap();
    for bytes in wav[44..].chunks_exact_mut(2) {
        bytes.copy_from_slice(&8192_i16.to_le_bytes());
    }
    std::fs::write(&path, wav).unwrap();
    let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");

    for (volume_scale, muted, expected_gain) in [
        (VolumeScale::Perceptual, false, 0.056_234_132_519_034_91),
        (VolumeScale::Linear, false, 0.5),
        (VolumeScale::Perceptual, true, 0.056_234_132_519_034_91),
    ] {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(events);
        let settings = BackendAudioSettings {
            volume: 0.5,
            volume_scale,
            muted,
            audio_output: Some("appsink".to_string()),
            ..BackendAudioSettings::default()
        };
        lock_recover(&engine.shared).settings = settings;

        engine
            .play_prepared(
                PreparedRun {
                    run: RunId::new(1),
                    stream: ResolvedStream::new(uri.as_str()).into(),
                },
                None,
                0,
            )
            .expect("start fresh playback");

        let sink = engine
            .primary
            .audio_output()
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let sample = loop {
            engine.poll_bus();
            engine.tick();
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                break sample;
            }
            assert!(Instant::now() < deadline, "no audio output after startup");
            std::thread::sleep(Duration::from_millis(1));
        };
        let map = sample.buffer().unwrap().map_readable().unwrap();
        let expected = if muted { 0.0 } else { 0.25 * expected_gain };
        assert!(!map.is_empty());
        for bytes in map.as_slice().chunks_exact(4) {
            let actual = f32::from_le_bytes(bytes.try_into().unwrap());
            assert!(
                (f64::from(actual) - expected).abs() < 0.0001,
                "first audio sample {actual}, expected {expected}"
            );
        }
        engine.shutdown();
    }
}

#[test]
fn startup_seek_waits_for_preroll_before_playing() {
    ensure_gstreamer_initialized().expect("initialize GStreamer");
    let directory = tempfile::tempdir().expect("playback fixture directory");
    let path = directory.path().join("startup-seek.wav");
    write_long_silent_wave(&path);
    let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    lock_recover(&engine.shared).settings = BackendAudioSettings {
        audio_output: Some("fakesink".to_string()),
        ..BackendAudioSettings::default()
    };

    engine
        .play_prepared(
            PreparedRun {
                run: RunId::new(1),
                stream: ResolvedStream::new(uri.as_str()).into(),
            },
            None,
            5_000,
        )
        .expect("start playback with a saved position");
    assert!(engine.primary.has_or_targets_state(gst::State::Paused));
    assert!(lock_recover(&events).drain().iter().any(|event| matches!(
        event,
        BackendEvent::State {
            state: BackendState::Buffering,
            ..
        }
    )));
    assert_eq!(engine.seek.queued, Some(5_000));
    engine
        .primary
        .audio_output()
        .unwrap()
        .set_property("sync", true);
    engine.push_position(5_000);
    assert!(!engine.primary.is_playing());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !engine.primary.is_playing() && Instant::now() < deadline {
        engine.poll_bus();
        engine.tick();
        thread::sleep(Duration::from_millis(10));
    }
    engine.poll_bus();
    assert!(engine.primary.is_playing());
    assert!(lock_recover(&events).drain().iter().any(|event| matches!(
        event,
        BackendEvent::State {
            state: BackendState::Playing,
            ..
        }
    )));
    assert!(engine.primary.position().unwrap().mseconds() >= 5_000);
    engine.shutdown();
}

#[test]
fn seeks_wait_for_the_running_seek_and_keep_only_the_latest_target() {
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(events);
    lock_recover(&engine.shared).current = Some(PreparedRun {
        run: RunId::new(1),
        stream: ResolvedStream::new("file:///music/current.flac").into(),
    });
    engine.desired_playing = true;
    engine.session_prerolled = true;
    engine.seek.in_flight = Some(1_000);

    engine.start_seek(2_000).unwrap();
    engine.start_seek(3_000).unwrap();
    assert_eq!(engine.seek.queued, Some(3_000));
    // Repeating the running target, as repeated seek requests do, drops the queued one.
    engine.start_seek(1_000).unwrap();
    assert_eq!(engine.seek.queued, None);
    assert_eq!(engine.seek.in_flight, Some(1_000));
}

#[test]
fn unconfirmed_gapless_activation_keeps_the_old_run_until_playing() {
    for result in [
        gst::StateChangeSuccess::Async,
        gst::StateChangeSuccess::NoPreroll,
    ] {
        let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
        assert!(
            fixture
                .engine
                .begin_incoming_handoff(Slot::Secondary, INCOMING_PIPELINE, result,)
        );

        assert_eq!(fixture.current_run(), Some(fixture.old_run));
        assert!(fixture.engine.incoming().is_none());
        assert!(matches!(
            fixture.engine.activating(),
            Some(PendingHandoff {
                incoming,
                from: Slot::Primary,
                old_run,
            }) if incoming.id == INCOMING_PIPELINE
                && incoming.item.run == fixture.next_run
                && *old_run == fixture.old_run
        ));
        assert!(fixture.drain().is_empty());
    }
}

#[test]
fn matching_playing_confirmation_commits_gapless_handoff_once() {
    let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
    fixture.begin_separate();

    assert!(
        fixture
            .engine
            .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
    );
    assert!(
        !fixture
            .engine
            .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
    );

    let shared = lock_recover(&fixture.engine.shared);
    assert_eq!(
        shared.current.as_ref().map(|item| item.run),
        Some(fixture.next_run)
    );
    assert_eq!(shared.active, Slot::Secondary);
    drop(shared);
    assert!(matches!(fixture.engine.transition, Transition::None));
    assert_eq!(
        fixture
            .drain()
            .into_iter()
            .filter(|event| matches!(event, BackendEvent::Transitioned { .. }))
            .count(),
        1
    );
}

#[test]
fn crossfade_overlap_starts_only_after_the_incoming_pipeline_is_playing() {
    let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
        duration_millis: 5_000,
    });
    fixture.begin_separate();
    assert!(fixture.engine.overlap().is_none());
    assert!(fixture.drain().is_empty());

    assert!(
        fixture
            .engine
            .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
    );

    assert_eq!(
        fixture.engine.overlap(),
        Some((Slot::Primary, fixture.old_run))
    );
    let shared = lock_recover(&fixture.engine.shared);
    assert_eq!(shared.active, Slot::Secondary);
    assert_eq!(
        shared.current.as_ref().map(|item| item.run),
        Some(fixture.next_run)
    );
    drop(shared);
    assert!(fixture.drain().iter().any(|event| matches!(
        event,
        BackendEvent::Transitioned {
            old_run: transitioned_old,
            new_run: transitioned_new,
        } if *transitioned_old == fixture.old_run
            && *transitioned_new == fixture.next_run
    )));
}

#[test]
fn failed_asynchronous_gapless_activation_falls_back_after_the_old_run_ends() {
    let mut fixture = HandoffFixture::separate(NextTransition::Gapless);
    fixture.begin_separate();

    fixture.engine.fail_handoff(
        Slot::Secondary,
        INCOMING_PIPELINE,
        "incoming activation failed".into(),
    );

    let shared = lock_recover(&fixture.engine.shared);
    assert!(shared.current.is_none());
    assert!(shared.next.is_none());
    drop(shared);
    assert!(matches!(
        fixture.drain().as_slice(),
        [
            BackendEvent::NextPreparationFailed {
                current_run,
                next_run: failed_next,
                ..
            },
            BackendEvent::Ended { run: ended_run }
        ] if *current_run == fixture.old_run
            && *failed_next == fixture.next_run
            && *ended_run == fixture.old_run
    ));
}

#[test]
fn volume_scale_change_preserves_an_unconfirmed_handoff() {
    let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
        duration_millis: 5_000,
    });
    fixture.begin_separate();

    fixture
        .engine
        .handle_command(BackendCommand::SetOutputVolume {
            volume: 0.5,
            volume_scale: VolumeScale::Perceptual,
            muted: false,
        });

    assert!(fixture.engine.activating().is_some());
    let (gain, muted) = fixture.engine.output_gain_state();
    assert!((gain - VolumeScale::Perceptual.gain(0.5)).abs() < f64::EPSILON);
    assert!(!muted);
    assert!(
        !fixture
            .drain()
            .iter()
            .any(|event| matches!(event, BackendEvent::Ended { .. }))
    );
}

#[test]
fn pause_cancels_an_unconfirmed_crossfade_and_late_playing_cannot_commit_it() {
    let mut fixture = HandoffFixture::separate(NextTransition::Crossfade {
        duration_millis: 5_000,
    });
    fixture.begin_separate();

    fixture.engine.handle_command(BackendCommand::Pause {
        run: fixture.old_run,
    });

    assert!(matches!(fixture.engine.transition, Transition::None));
    assert!(
        !fixture
            .engine
            .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
    );
    assert_eq!(fixture.current_run(), Some(fixture.old_run));
    assert!(fixture.drain().iter().all(|event| !matches!(
        event,
        BackendEvent::Transitioned { .. } | BackendEvent::Ended { .. }
    )));
}

#[test]
fn late_backend_states_after_pause_cannot_authorize_a_transition() {
    for late_state in [BackendState::Buffering, BackendState::Playing] {
        let mut crossfade = HandoffFixture::separate(NextTransition::Crossfade {
            duration_millis: 5_000,
        });
        crossfade.engine.handle_command(BackendCommand::Pause {
            run: crossfade.old_run,
        });
        crossfade.engine.handle_state_changed(late_state);
        crossfade.engine.maybe_start_crossfade();

        assert!(crossfade.engine.incoming().is_some());

        let mut gapless = HandoffFixture::separate(NextTransition::Gapless);
        gapless.engine.handle_command(BackendCommand::Pause {
            run: gapless.old_run,
        });
        gapless.engine.handle_state_changed(late_state);
        gapless.engine.handle_end(Slot::Primary);

        assert_eq!(gapless.current_run(), None);
        assert!(gapless.engine.activating().is_none());
        assert!(gapless.drain().iter().all(|event| !matches!(
            event,
            BackendEvent::Transitioned { .. } | BackendEvent::NextPreparationFailed { .. }
        )));
    }
}

#[test]
fn pause_intent_survives_handoff_audio_reconfiguration_and_late_playing_state() {
    ensure_gstreamer_initialized().expect("initialize GStreamer");
    let directory = tempfile::tempdir().expect("playback fixture directory");
    let path = directory.path().join("pause-fade.wav");
    write_silent_wave(&path);
    let uri = gst::glib::filename_to_uri(&path, None).expect("playback fixture URI");
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    let old_run = RunId::new(1);
    let new_run = RunId::new(2);
    let current = PreparedRun {
        run: old_run,
        stream: ResolvedStream::new(uri.as_str()).into(),
    };
    let next = PreparedNext::new(
        new_run,
        ResolvedStream::new(uri.as_str()),
        NextTransition::Gapless,
    );
    let mut settings = BackendAudioSettings::default();
    settings.audio_output = Some("fakesink".to_string());
    let pipeline_id = engine
        .start_pipeline(
            Slot::Primary,
            &current,
            &settings,
            settings.volume,
            settings.muted,
            gst::State::Paused,
            None,
            None,
        )
        .expect("start inert GStreamer session");
    {
        let mut shared = lock_recover(&engine.shared);
        shared.settings = settings.clone();
        shared.current = Some(current);
        lock_recover(engine.primary.gapless().unwrap()).pending = Some(next);
        shared.active = Slot::Primary;
        shared.set_pipeline_id(Slot::Primary, Some(pipeline_id));
    }
    engine.desired_playing = true;

    engine.handle_command(BackendCommand::Pause { run: old_run });
    let original_fade = engine.status_fade.expect("pause fade");
    engine.handle_stream_start();
    assert_eq!(engine.timing_run_id(), Some(new_run));

    engine.handle_command(BackendCommand::Pause { run: new_run });

    let preserved_fade = engine.status_fade.expect("preserved pause fade");
    assert_eq!(preserved_fade.slot, original_fade.slot);
    assert_eq!(preserved_fade.target, StatusFadeTarget::Pause);

    lock_recover(&events).drain();
    engine.handle_command(BackendCommand::ConfigureAudio(settings.clone()));
    assert!(!engine.desired_playing);
    assert!(engine.status_fade.is_none());
    let failed_events = lock_recover(&events).drain();
    failed_events
        .iter()
        .position(|event| {
            matches!(
                event,
                BackendEvent::State {
                    run,
                    state: BackendState::Paused,
                } if *run == new_run
            )
        })
        .expect("paused acknowledgement before reconfiguration");
    assert!(
        failed_events
            .iter()
            .all(|event| !matches!(event, BackendEvent::OperationFailed { .. }))
    );

    let mut configured_settings = settings;
    configured_settings.fade_on_status_change = false;
    engine.handle_command(BackendCommand::ConfigureAudio(configured_settings));
    assert!(!engine.desired_playing);
    assert!(engine.status_fade.is_none());
    assert!(lock_recover(&events).drain().iter().any(|event| matches!(
        event,
        BackendEvent::State {
            run,
            state: BackendState::Paused,
        } if *run == new_run
    )));

    engine.handle_command(BackendCommand::Play { run: new_run });
    assert!(engine.desired_playing);
    assert!(engine.status_fade.is_none());
    engine.handle_command(BackendCommand::Pause { run: new_run });
    assert!(!engine.desired_playing);

    engine.handle_state_changed(BackendState::Playing);
    engine.start_seek(0).expect("seek while paused");
    assert!(!engine.session_prerolled && !engine.awaiting_playing);
    assert!(!engine.desired_playing);
    engine.shutdown();
}

fn write_silent_wave(path: &std::path::Path) {
    write_silent_mono_wave(path, 800);
}

#[test]
fn repeats_preserve_samples_on_one_pipeline() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("segments.wav");
    write_silent_mono_wave(&path, 16_000);
    let source = (0..16_000)
        .map(|i| ((i % 1000) as i16 - 500) * 16)
        .collect::<Vec<_>>();
    let mut wav = std::fs::read(&path).unwrap();
    for (sample, bytes) in source.iter().zip(wav[44..].chunks_exact_mut(2)) {
        bytes.copy_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(&path, wav).unwrap();
    let uri = gst::glib::filename_to_uri(&path, None).unwrap();
    {
        let stream = || ResolvedStream::new(uri.as_str());
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            fade_on_status_change: false,
            ..BackendAudioSettings::default()
        }));
        engine.handle_command(BackendCommand::Start {
            run: RunId::new(1),
            current: stream().into(),
            next: Some(PreparedNext::new(
                RunId::new(2),
                stream(),
                NextTransition::Gapless,
            )),
            start_position_millis: 0,
        });
        let sink = engine
            .active_pipeline()
            .audio_output()
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        let mut samples = Vec::new();
        let mut transitions = 0;
        let mut ended = false;
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline && !ended {
            engine.poll_bus();
            engine.tick();
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let map = sample.buffer().unwrap().map_readable().unwrap();
                samples.extend(
                    map.as_slice()
                        .chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
                );
            }
            for event in lock_recover(&events).drain() {
                match event {
                    BackendEvent::Transitioned { old_run, new_run } => {
                        assert_eq!((old_run, new_run), (RunId::new(1), RunId::new(2)));
                        transitions += 1;
                        assert!(!engine.secondary.has_session());
                    }
                    BackendEvent::Ended { run } => {
                        assert_eq!(run, RunId::new(2));
                        ended = true;
                    }
                    BackendEvent::Error { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        assert!(ended, "no EOS");
        assert_eq!(transitions, 1);
        let expected = (0..2)
            .flat_map(|_| source.iter().map(|sample| f32::from(*sample) / 32768.0))
            .collect::<Vec<_>>();
        assert_eq!(samples.len(), expected.len(), "sample count");
        assert!(
            samples
                .iter()
                .zip(&expected)
                .all(|(actual, expected)| (actual - expected).abs() < 0.00001),
            "samples changed"
        );
    }
}

#[test]
fn accepted_compressed_audio_reaches_the_output_undecoded_and_switches_at_format_changes() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let wave = directory.path().join("decoded.wav");
    write_silent_mono_wave(&wave, 4_000);
    let wave = ResolvedStream::new(gst::glib::filename_to_uri(&wave, None).unwrap().as_str());
    let fixture = |name: &str| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        ResolvedStream::new(gst::glib::filename_to_uri(path, None).unwrap().as_str())
    };
    // 12 E-AC3 5.1 frames each.
    let (first, second) = (fixture("surround-a.eac3"), fixture("surround-b.eac3"));
    let crossfade = NextTransition::Crossfade {
        duration_millis: 1_000,
    };

    #[derive(Debug, Default)]
    struct Rendered {
        formats: Vec<String>,
        frames: usize,
        samples: usize,
        transitions: usize,
        ended: Vec<RunId>,
        largest_gap: Duration,
        passed_through: Vec<RunId>,
    }
    // Plays until `last` ends. A core that receives `Ended` starts the queued item cold.
    let render = |volume: f64,
                  current: &ResolvedStream,
                  next: Option<(&ResolvedStream, NextTransition)>,
                  last: RunId| {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            fade_on_status_change: false,
            volume,
            ..BackendAudioSettings::default()
        }));
        engine.handle_command(BackendCommand::Start {
            run: RunId::new(1),
            current: current.clone().into(),
            next: next.map(|(stream, transition)| {
                PreparedNext::new(RunId::new(2), stream.clone(), transition)
            }),
            start_position_millis: 0,
        });
        // A slow worker: short sources finish reading and queue their next item first.
        std::thread::sleep(Duration::from_millis(500));
        let mut rendered = Rendered::default();
        let mut end = None::<gst::ClockTime>;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !rendered.ended.contains(&last) {
            engine.poll_bus();
            engine.tick();
            assert!(
                !(engine.active_pipeline().is_bitstream() && engine.secondary.has_session()),
                "a second player opened beside a passed-through stream"
            );
            let sink = engine
                .active_pipeline()
                .audio_output()
                .and_then(|output| output.downcast::<gstreamer_app::AppSink>().ok());
            while let Some(sample) = sink
                .as_ref()
                .and_then(|sink| sink.try_pull_sample(gst::ClockTime::ZERO))
            {
                let format = sample
                    .caps()
                    .unwrap()
                    .structure(0)
                    .unwrap()
                    .name()
                    .to_string();
                let buffer = sample.buffer().unwrap();
                if format == "audio/x-eac3" {
                    rendered.frames += 1;
                    let segment = sample.segment().unwrap();
                    let segment = segment.downcast_ref::<gst::ClockTime>().unwrap();
                    let start = segment.to_running_time(buffer.pts().unwrap()).unwrap();
                    if let Some(end) = end {
                        rendered.largest_gap = rendered.largest_gap.max(Duration::from_nanos(
                            start.nseconds().abs_diff(end.nseconds()),
                        ));
                    }
                    end = Some(start + buffer.duration().unwrap());
                } else {
                    rendered.samples += buffer.size() / 4;
                }
                if rendered.formats.last() != Some(&format) {
                    rendered.formats.push(format);
                }
            }
            let drained = lock_recover(&events).drain();
            for event in drained {
                match event {
                    BackendEvent::Transitioned { .. } => rendered.transitions += 1,
                    BackendEvent::Bitstream { run, active: true } => {
                        rendered.passed_through.push(run);
                    }
                    BackendEvent::Ended { run } => {
                        rendered.ended.push(run);
                        if run == RunId::new(1)
                            && let Some((stream, _)) = next
                            && rendered.transitions == 0
                        {
                            engine.handle_command(BackendCommand::Start {
                                run: RunId::new(2),
                                current: stream.clone().into(),
                                next: None,
                                start_position_millis: 0,
                            });
                        }
                    }
                    BackendEvent::Error { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        assert!(rendered.ended.contains(&last), "{rendered:?}");
        rendered
    };

    let passed = render(1.0, &first, None, RunId::new(1));
    assert_eq!(
        (passed.formats, passed.frames),
        (vec!["audio/x-eac3".into()], 12)
    );
    // Volume below 100% changes samples, so the track stays decoded.
    let decoded = render(0.5, &first, None, RunId::new(1));
    assert_eq!(decoded.formats, ["audio/x-raw"]);
    assert!(decoded.samples > 0 && decoded.frames == 0, "{decoded:?}");
    assert!(decoded.passed_through.is_empty(), "{decoded:?}");
    // A passed-through album stays one gapless stream; a crossfade can't mix it.
    let album = render(1.0, &first, Some((&second, crossfade)), RunId::new(2));
    assert_eq!((album.frames, album.transitions), (24, 1), "{album:?}");
    assert!(album.largest_gap < Duration::from_micros(1), "{album:?}");
    // Both runs are labelled, including the one that started gaplessly.
    assert_eq!(
        album.passed_through,
        [RunId::new(1), RunId::new(2)],
        "{album:?}"
    );
    // A format change finishes the current track, then starts the next one in its mode.
    for (current, next, formats) in [
        (&wave, &second, ["audio/x-raw", "audio/x-eac3"]),
        (&first, &wave, ["audio/x-eac3", "audio/x-raw"]),
    ] {
        let changed = render(
            1.0,
            current,
            Some((next, NextTransition::Gapless)),
            RunId::new(2),
        );
        assert_eq!(changed.formats, formats, "{changed:?}");
        assert_eq!(
            (changed.frames, changed.samples, changed.transitions),
            (12, 4_000, 0),
            "{changed:?}"
        );
        assert_eq!(changed.ended, [RunId::new(1), RunId::new(2)]);
    }
}

fn write_long_silent_wave(path: &std::path::Path) {
    write_silent_mono_wave(path, 80_000);
}

#[test]
fn aac_priming_seeks_render_audio_and_end_at_the_real_duration() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("priming.m4a");
    // Generated 440 Hz AAC: no edit list, iTunSMPB delay 2112 and padding 204.
    std::fs::write(
        &path,
        include_bytes!("../../tests/fixtures/aac-gapless-priming.m4a"),
    )
    .unwrap();
    let uri = gst::glib::filename_to_uri(&path, None).unwrap();
    for paused_seek in [false, true] {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            fade_on_status_change: false,
            ..BackendAudioSettings::default()
        }));
        let run = RunId::new(1);
        engine.handle_command(BackendCommand::Start {
            run,
            current: ResolvedStream::new(uri.as_str()).into(),
            next: None,
            start_position_millis: 700,
        });
        let sink = engine
            .primary
            .audio_output()
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        let mut frames = [0_usize; 2];
        let mut peak = [0.0_f32; 2];
        let mut sought = false;
        let mut ended = false;
        let mut last_audio = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !ended {
            engine.poll_bus();
            engine.tick();
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let segment = sample
                    .segment()
                    .unwrap()
                    .downcast_ref::<gst::ClockTime>()
                    .unwrap();
                assert!(
                    segment.stop().unwrap().mseconds() <= 2_967,
                    "seek extended the AAC segment past its real end: {segment:?}"
                );
                let buffer = sample.buffer().unwrap();
                let map = buffer.map_readable().unwrap();
                last_audio = Some(Instant::now());
                let index = usize::from(sought);
                frames[index] += map.len() / 4;
                for bytes in map.as_slice().chunks_exact(4) {
                    peak[index] =
                        peak[index].max(f32::from_le_bytes(bytes.try_into().unwrap()).abs());
                }
            }
            for event in lock_recover(&events).drain() {
                match event {
                    BackendEvent::Ended { run: finished } => {
                        assert_eq!(finished, run);
                        assert!(
                            last_audio.unwrap().elapsed() < Duration::from_millis(400),
                            "AAC EOS stalled after the last rendered buffer"
                        );
                        ended = true;
                    }
                    BackendEvent::Error { error, .. }
                    | BackendEvent::OperationFailed { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            if !sought && frames[0] >= 1_600 {
                assert!(engine.primary.position().unwrap().mseconds() >= 850);
                if paused_seek {
                    engine.handle_command(BackendCommand::Pause { run });
                    let paused = engine.primary.position().unwrap();
                    std::thread::sleep(Duration::from_millis(100));
                    engine.poll_bus();
                    engine.tick();
                    assert!(!engine.primary.is_playing());
                    assert!(
                        engine
                            .primary
                            .position()
                            .unwrap()
                            .absdiff(paused)
                            .mseconds()
                            <= 2
                    );
                }
                engine.handle_command(BackendCommand::Seek {
                    run,
                    position_millis: 1_700,
                });
                if paused_seek {
                    engine.handle_command(BackendCommand::Play { run });
                }
                sought = true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        assert!(
            sought && ended,
            "AAC seek never resumed or reached EOS, paused={paused_seek}"
        );
        assert!(frames[1] >= 9_000, "seek lost the decoded tail: {frames:?}");
        assert!(
            peak.iter().all(|peak| *peak > 0.1),
            "Playing produced silence: {peak:?}"
        );
    }
}

#[test]
fn aac_priming_repeat_one_renders_each_run_without_stalls_or_lost_audio() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("repeat-priming.m4a");
    std::fs::write(
        &path,
        include_bytes!("../../tests/fixtures/aac-gapless-priming.m4a"),
    )
    .unwrap();
    let uri = gst::glib::filename_to_uri(&path, None).unwrap();
    let mut reference = Vec::new();
    for runs in [1_u64, 4] {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
            audio_output: Some("appsink".into()),
            fade_on_status_change: false,
            ..BackendAudioSettings::default()
        }));
        engine.handle_command(BackendCommand::Start {
            run: RunId::new(1),
            current: ResolvedStream::new(uri.as_str()).into(),
            next: None,
            start_position_millis: 0,
        });
        let sink = engine
            .primary
            .audio_output()
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        let mut samples = Vec::new();
        let mut transitions = 0;
        let mut positions = [0_u64; 4];
        let mut ended = false;
        let mut last_audio = None;
        let deadline = Instant::now() + Duration::from_secs(runs * 3 + 5);
        while Instant::now() < deadline && !ended {
            engine.poll_bus();
            engine.tick();
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let map = sample.buffer().unwrap().map_readable().unwrap();
                last_audio = Some(Instant::now());
                samples.extend(
                    map.as_slice()
                        .chunks_exact(4)
                        .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
                );
            }
            for event in lock_recover(&events).drain() {
                match event {
                    BackendEvent::NextNeeded { run } => {
                        engine.handle_command(BackendCommand::PrepareNext {
                            current_run: run,
                            next: (run.get() < runs).then(|| {
                                PreparedNext::new(
                                    RunId::new(run.get() + 1),
                                    ResolvedStream::new(uri.as_str()),
                                    NextTransition::Gapless,
                                )
                            }),
                        })
                    }
                    BackendEvent::Transitioned { old_run, new_run } => {
                        assert!(
                            last_audio.unwrap().elapsed() < Duration::from_millis(400),
                            "repeat stalled after the last rendered buffer"
                        );
                        transitions += 1;
                        assert_eq!(old_run.get(), transitions);
                        assert_eq!(new_run.get(), transitions + 1);
                    }
                    BackendEvent::Position { run, millis } => {
                        let previous = &mut positions[(run.get() - 1) as usize];
                        assert!(
                            millis.saturating_add(20) >= *previous,
                            "repeat position oscillated for {run}: {previous} -> {millis}"
                        );
                        *previous = millis;
                    }
                    BackendEvent::Ended { run } => {
                        assert_eq!(run.get(), runs);
                        assert!(
                            last_audio.unwrap().elapsed() < Duration::from_millis(400),
                            "final repeat stalled before EOS"
                        );
                        ended = true;
                    }
                    BackendEvent::Error { error, .. }
                    | BackendEvent::OperationFailed { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        engine.shutdown();
        assert!(
            ended,
            "repeat playback stalled after {transitions} transitions"
        );
        assert_eq!(transitions, runs - 1);
        assert!(
            positions[..runs as usize]
                .iter()
                .all(|position| *position > 2_000),
            "a repeated run never advanced its clock: {positions:?}"
        );
        if runs == 1 {
            assert!(samples.len() > 23_000);
            assert!(samples.iter().any(|sample| sample.abs() > 0.1));
            reference = samples;
        } else {
            assert!(
                samples.len().abs_diff(reference.len() * runs as usize) <= runs as usize,
                "repeat lost or inserted audio: {} vs {}",
                samples.len(),
                reference.len() * runs as usize
            );
            let width = reference.len() / 4;
            for chunk in samples.chunks(width) {
                let energy =
                    chunk.iter().map(|sample| sample * sample).sum::<f32>() / chunk.len() as f32;
                assert!(energy > 0.01, "repeat rendered a silent span");
            }
        }
    }
}

#[test]
fn pause_resume_fades_render_smoothly_without_position_reversal() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fade-tone.wav");
    write_long_silent_wave(&path);
    let mut wav = std::fs::read(&path).unwrap();
    for sample in wav[44..].chunks_exact_mut(2) {
        sample.copy_from_slice(&8192_i16.to_le_bytes());
    }
    std::fs::write(&path, wav).unwrap();
    for (fade_enabled, disable_after_pause) in [(true, false), (false, false), (true, true)] {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let settings = BackendAudioSettings {
            audio_output: Some("appsink".into()),
            fade_on_status_change: fade_enabled,
            volume: 0.5,
            volume_scale: VolumeScale::Linear,
            ..BackendAudioSettings::default()
        };
        let expected_level = 0.25 * settings.output_gain() as f32;
        engine.handle_command(BackendCommand::ConfigureAudio(settings.clone()));
        let run = RunId::new(1);
        let current = PreparedRun {
            run,
            stream: ResolvedStream::new(gst::glib::filename_to_uri(&path, None).unwrap().as_str())
                .into(),
        };
        engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.output_gain(),
                settings.muted,
                gst::State::Null,
                None,
                None,
            )
            .unwrap();
        let output = engine.primary.audio_output().unwrap();
        let output_bin = output.parent().unwrap().downcast::<gst::Bin>().unwrap();
        let gain = output_bin.by_name("slopsonic-output-gain").unwrap();
        let downstream = gain
            .static_pad("src")
            .unwrap()
            .peer()
            .unwrap()
            .parent_element()
            .unwrap();
        let queue = gst::ElementFactory::make("queue")
            .property("max-size-time", 400_000_000_u64)
            .property("max-size-bytes", 0_u32)
            .property("max-size-buffers", 0_u32)
            .build()
            .unwrap();
        output_bin.add(&queue).unwrap();
        gain.unlink(&downstream);
        gst::Element::link_many([&gain, &queue, &downstream]).unwrap();
        engine.handle_command(BackendCommand::Start {
            run,
            current: current.stream,
            next: None,
            start_position_millis: 3_000,
        });
        let sink = engine
            .primary
            .audio_output()
            .unwrap()
            .downcast::<gstreamer_app::AppSink>()
            .unwrap();
        let mut down = Vec::new();
        let mut up = Vec::new();
        let mut frames = 0;
        let mut pause_requested = false;
        let mut resumed = false;
        let mut paused_position = 0;
        let mut last_position = 0;
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline
            && (up.len() < 60
                || !engine.primary.fade_finished()
                || up
                    .last()
                    .is_none_or(|level: &f32| *level < expected_level * 0.996))
        {
            engine.poll_bus();
            engine.tick();
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let map = sample.buffer().unwrap().map_readable().unwrap();
                let values = map
                    .as_slice()
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                    .collect::<Vec<_>>();
                frames += values.len();
                let level = values.iter().sum::<f32>() / values.len() as f32;
                if resumed {
                    up.push(level);
                } else if pause_requested {
                    down.push(level);
                }
            }
            for event in lock_recover(&events).drain() {
                match event {
                    BackendEvent::Position { millis, .. } => {
                        assert!(
                            millis.saturating_add(20) >= last_position,
                            "pause/resume reversed playback position: {last_position} -> {millis}"
                        );
                        last_position = millis;
                    }
                    BackendEvent::Error { error, .. }
                    | BackendEvent::OperationFailed { error, .. } => panic!("{error:?}"),
                    _ => {}
                }
            }
            if !pause_requested
                && frames >= 3_200
                && queue.property::<u64>("current-level-time") >= 300_000_000
            {
                engine.handle_command(BackendCommand::Pause { run });
                pause_requested = true;
            } else if pause_requested && !resumed && !engine.primary.is_playing() {
                let position = engine.primary.position().unwrap();
                paused_position = position.mseconds();
                std::thread::sleep(Duration::from_millis(120));
                engine.poll_bus();
                engine.tick();
                assert!(
                    engine
                        .primary
                        .position()
                        .unwrap()
                        .absdiff(position)
                        .mseconds()
                        <= 2
                );
                assert!(sink.try_pull_sample(gst::ClockTime::ZERO).is_none());
                if disable_after_pause {
                    let mut changed = settings.clone();
                    changed.fade_on_status_change = false;
                    engine.handle_command(BackendCommand::ConfigureAudio(changed));
                }
                if !fade_enabled {
                    engine.handle_command(BackendCommand::ConfigureAudio(settings.clone()));
                    engine.handle_command(BackendCommand::Seek {
                        run,
                        position_millis: paused_position,
                    });
                    let preroll_deadline = Instant::now() + Duration::from_secs(3);
                    while engine.primary.seek_in_progress() || !engine.primary.has_prerolled() {
                        engine.poll_bus();
                        engine.tick();
                        assert!(
                            Instant::now() < preroll_deadline,
                            "paused seek did not preroll"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
                engine.handle_command(BackendCommand::Play { run });
                resumed = true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let final_position = engine.primary.position().unwrap().mseconds();
        engine.shutdown();
        assert!(
            pause_requested,
            "fade test did not queue enough PCM ahead of playback"
        );
        assert!(
            resumed && up.len() >= 30,
            "resume did not render sustained audio"
        );
        assert!(final_position >= paused_position + 300);
        if fade_enabled {
            assert!(
                down.len() >= 10
                    && down[0] > expected_level * 0.8
                    && *down.last().unwrap() < expected_level * 0.2,
                "pause did not render a fade: {down:?}"
            );
        }
        if fade_enabled && !disable_after_pause {
            assert!(
                up[0] < expected_level * 0.2 && *up.last().unwrap() > expected_level * 0.96,
                "resume did not render a fade: {up:?}"
            );
            for levels in [&down, &up] {
                assert!(
                    levels
                        .windows(2)
                        .all(|pair| (pair[1] - pair[0]).abs() < expected_level * 0.16),
                    "fade contained an audible amplitude jump: {levels:?}"
                );
            }
        } else {
            let levels = if disable_after_pause {
                &up[up.len().saturating_sub(30)..]
            } else {
                &up[..]
            };
            assert!(
                levels
                    .iter()
                    .all(|level| (*level - expected_level).abs() < 0.00001),
                "fade-off playback stayed muted after queued PCM drained: {up:?}"
            );
        }
    }
}

fn write_silent_mono_wave(path: &std::path::Path, frames: u32) {
    const SAMPLE_RATE: u32 = 8_000;
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    let bytes_per_frame = u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
    let data_len = frames * bytes_per_frame;
    let mut file = File::create(path).expect("create playback fixture");
    file.write_all(b"RIFF").expect("write RIFF");
    file.write_all(&(36 + data_len).to_le_bytes())
        .expect("write RIFF size");
    file.write_all(b"WAVEfmt ").expect("write WAVE format");
    file.write_all(&16_u32.to_le_bytes())
        .expect("write format size");
    file.write_all(&1_u16.to_le_bytes())
        .expect("write PCM format");
    file.write_all(&CHANNELS.to_le_bytes())
        .expect("write channel count");
    file.write_all(&SAMPLE_RATE.to_le_bytes())
        .expect("write sample rate");
    file.write_all(&(SAMPLE_RATE * bytes_per_frame).to_le_bytes())
        .expect("write byte rate");
    file.write_all(&(bytes_per_frame as u16).to_le_bytes())
        .expect("write block alignment");
    file.write_all(&BITS_PER_SAMPLE.to_le_bytes())
        .expect("write sample size");
    file.write_all(b"data").expect("write data tag");
    file.write_all(&data_len.to_le_bytes())
        .expect("write data size");
    for _ in 0..frames {
        file.write_all(&0_i16.to_le_bytes()).expect("write sample");
    }
}

#[test]
fn seek_cancels_each_unconfirmed_handoff_and_late_confirmation_is_inert() {
    let mut separate = HandoffFixture::separate(NextTransition::Gapless);
    separate.begin_separate();

    let restored = separate
        .engine
        .cancel_handoff_for_seek()
        .expect("separate gapless handoff restores the current run");

    assert_eq!(restored.run, separate.old_run);
    assert!(separate.engine.activating().is_none());
    assert!(
        !separate
            .engine
            .confirm_handoff(Slot::Secondary, INCOMING_PIPELINE)
    );
    assert_eq!(separate.current_run(), Some(separate.old_run));
    assert!(separate.drain().is_empty());
}

#[test]
fn failed_incoming_pipeline_keeps_the_current_and_reserved_next() {
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    let current_run = RunId::new(1);
    let next = PreparedNext::new(
        RunId::new(2),
        ResolvedStream::new("https://music.example/next.flac"),
        NextTransition::Crossfade {
            duration_millis: 5_000,
        },
    );
    let incoming_id = PipelineId(8);
    {
        let mut shared = lock_recover(&engine.shared);
        shared.current = Some(PreparedRun {
            run: current_run,
            stream: ResolvedStream::new("https://music.example/current.flac").into(),
        });
        shared.next = Some(next.clone());
        shared.set_pipeline_id(Slot::Secondary, Some(incoming_id));
    }
    engine.transition = Transition::Preparing(IncomingPipeline {
        id: incoming_id,
        slot: Slot::Secondary,
        item: next.clone(),
        phase: IncomingPhase::Prerolling,
    });

    engine.fail_incoming(Slot::Secondary, incoming_id, "next stream failed".into());

    let shared = lock_recover(&engine.shared);
    assert_eq!(
        shared.current.as_ref().map(|item| item.run),
        Some(current_run)
    );
    assert_eq!(shared.next, Some(next));
    assert_eq!(shared.pipeline_id(Slot::Secondary), None);
    drop(shared);
    assert!(matches!(
        lock_recover(&events).drain().as_slice(),
        [BackendEvent::NextPreparationFailed {
            current_run: failed_current,
            next_run: failed_next,
            ..
        }] if *failed_current == current_run && *failed_next == RunId::new(2)
    ));
}

#[test]
fn synchronous_next_preparation_failure_is_not_a_current_playback_error() {
    ensure_gstreamer_initialized().expect("initialize GStreamer");
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    let current_run = RunId::new(1);
    let current_pipeline = PipelineId(7);
    let next = PreparedNext::new(
        RunId::new(2),
        ResolvedStream::new("https://music.example/next.flac"),
        NextTransition::Crossfade {
            duration_millis: 5_000,
        },
    );
    {
        let mut shared = lock_recover(&engine.shared);
        shared.current = Some(PreparedRun {
            run: current_run,
            stream: ResolvedStream::new("https://music.example/current.flac").into(),
        });
        shared.set_pipeline_id(Slot::Primary, Some(current_pipeline));
        shared.settings.audio_output = Some("slopsonic-test-missing-output".to_string());
    }

    engine.handle_command(BackendCommand::PrepareNext {
        current_run,
        next: Some(next.clone()),
    });

    let shared = lock_recover(&engine.shared);
    assert_eq!(
        shared.current.as_ref().map(|item| item.run),
        Some(current_run)
    );
    assert_eq!(shared.next, Some(next));
    assert_eq!(shared.pipeline_id(Slot::Primary), Some(current_pipeline));
    drop(shared);
    let events = lock_recover(&events).drain();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, BackendEvent::Error { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        BackendEvent::NextPreparationFailed {
            current_run: failed_current,
            next_run: failed_next,
            ..
        } if *failed_current == current_run && *failed_next == RunId::new(2)
    )));
}

#[test]
fn failed_uri_preload_does_not_claim_current_or_unattributed_errors() {
    ensure_gstreamer_initialized().expect("initialize GStreamer");
    let current_uri = "https://music.example/current.flac";
    let next_uri = "https://music.example/next.flac";
    for (failed_uri, pending_uri, next_failed) in [
        (Some(next_uri), next_uri, true),
        (Some(current_uri), next_uri, false),
        (Some(current_uri), current_uri, false),
        (None, next_uri, false),
    ] {
        let events = Arc::new(Mutex::new(EventMailbox::default()));
        let mut engine = GstEngine::new(Arc::clone(&events));
        let current_run = RunId::new(1);
        let current = PreparedRun {
            run: current_run,
            stream: ResolvedStream::new(current_uri).into(),
        };
        let settings = BackendAudioSettings {
            audio_output: Some("fakesink".to_string()),
            ..BackendAudioSettings::default()
        };
        let pipeline = engine
            .start_pipeline(
                Slot::Primary,
                &current,
                &settings,
                settings.volume,
                settings.muted,
                gst::State::Null,
                None,
                None,
            )
            .expect("create inactive session");
        let next = PreparedNext::new(
            RunId::new(2),
            ResolvedStream::new(pending_uri),
            NextTransition::Gapless,
        );
        {
            let mut shared = lock_recover(&engine.shared);
            shared.current = Some(current);
            lock_recover(engine.primary.gapless().unwrap()).pending = Some(next);
        }
        let details = gst::Structure::builder("message-details")
            .field_if_some("uri", failed_uri)
            .build();
        let message = gst::message::Error::builder(gst::ResourceError::Read, "stream failed")
            .details(details)
            .build();
        engine.handle_message(Slot::Primary, pipeline, &message);

        let reported = lock_recover(&events).drain();
        if next_failed {
            // The current run keeps its session and ends through the drained EOS.
            assert!(engine.primary.has_session() && engine.primary.draining());
            assert!(matches!(
                reported.as_slice(),
                [BackendEvent::NextPreparationFailed {
                    current_run: failed_current,
                    next_run: failed_next,
                    ..
                }] if *failed_current == current_run && *failed_next == RunId::new(2)
            ));
            engine.handle_message(Slot::Primary, pipeline, &message);
            assert!(lock_recover(&events).drain().is_empty());
        } else {
            assert!(!engine.primary.has_session());
            let events = reported;
            assert!(matches!(
                events.as_slice(),
                [BackendEvent::Error { run, .. }] if *run == current_run
            ));
        }
    }
}

#[test]
fn replaced_pipeline_cannot_consume_next_work() {
    let old = PipelineId(4);
    let current = PipelineId(5);
    let current_run = RunId::new(10);
    let next = PreparedNext::new(
        RunId::new(11),
        ResolvedStream::new("file:///music/next.flac"),
        NextTransition::Gapless,
    );
    let mut shared = SharedBackendState::new();
    shared.active = Slot::Primary;
    shared.current = Some(PreparedRun {
        run: current_run,
        stream: ResolvedStream::new("file:///music/current.flac").into(),
    });
    shared.next = Some(next.clone());
    shared.set_pipeline_id(Slot::Primary, Some(current));

    assert_eq!(
        about_to_finish_action_for_pipeline(
            &mut shared,
            &mut GaplessPlayback::default(),
            Slot::Primary,
            old,
            true
        ),
        AboutToFinishAction::Ignore
    );
    assert_eq!(shared.next, Some(next));
}

#[test]
fn about_to_finish_before_playback_does_not_consume_the_next_track() {
    let mut gapless = GaplessPlayback::default();
    let pipeline = PipelineId(5);
    let current_run = RunId::new(10);
    let next = PreparedNext::new(
        RunId::new(11),
        ResolvedStream::new("file:///music/next.flac"),
        NextTransition::Gapless,
    );
    let mut shared = SharedBackendState::new();
    shared.active = Slot::Primary;
    shared.current = Some(PreparedRun {
        run: current_run,
        stream: ResolvedStream::new("file:///music/current.mod").into(),
    });
    shared.next = Some(next.clone());
    shared.set_pipeline_id(Slot::Primary, Some(pipeline));

    assert!(about_to_finish_may_query(&shared, Slot::Primary, pipeline));
    assert_eq!(
        about_to_finish_action_for_pipeline(
            &mut shared,
            &mut gapless,
            Slot::Primary,
            pipeline,
            false
        ),
        AboutToFinishAction::Ignore
    );
    assert_eq!(shared.next, Some(next.clone()));
    assert!(gapless.pending.is_none());

    assert!(matches!(
        about_to_finish_action_for_pipeline(&mut shared, &mut gapless, Slot::Primary, pipeline, true),
        AboutToFinishAction::Preload(preloaded) if *preloaded == next
    ));
}

#[test]
fn streams_without_preloading_keep_the_next_reserved() {
    let mut gapless = GaplessPlayback::default();
    let pipeline = PipelineId(5);
    let current_run = RunId::new(10);
    let next = PreparedNext::new(
        RunId::new(11),
        ResolvedStream::new("file:///music/next.flac"),
        NextTransition::Gapless,
    );
    let mut shared = SharedBackendState::new();
    shared.active = Slot::Primary;
    shared.current = Some(PreparedRun {
        run: current_run,
        stream: PreparedStream::from(ResolvedStream::new("file:///music/current.mod"))
            .without_preloading(),
    });
    shared.next = Some(next.clone());
    shared.set_pipeline_id(Slot::Primary, Some(pipeline));

    assert!(!about_to_finish_may_query(&shared, Slot::Primary, pipeline));
    assert_eq!(
        about_to_finish_action_for_pipeline(
            &mut shared,
            &mut gapless,
            Slot::Primary,
            pipeline,
            true
        ),
        AboutToFinishAction::Ignore
    );
    assert_eq!(shared.next, Some(next));
    assert!(gapless.pending.is_none());
}

#[test]
fn processing_changes_and_new_events_wake_their_owners() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .unwrap();
    let _runtime = runtime.enter();
    let (requests, receiver) = tokio::sync::watch::channel(ProcessingRequest::default());
    let mut backend = GStreamerPlaybackBackend::new(receiver).unwrap();
    let (woke, wakes) = std::sync::mpsc::channel();
    backend.set_event_waker(Arc::new(move || {
        let _ = woke.send(());
    }));
    backend.drain_events();
    while wakes.try_recv().is_ok() {}

    requests.send_modify(|request| request.meter = !request.meter);
    wakes
        .recv_timeout(Duration::from_secs(5))
        .expect("a processing change reaches the idle worker and wakes core");
    assert!(
        backend
            .drain_events()
            .iter()
            .any(|event| matches!(event, BackendEvent::ProcessingApplied { .. }))
    );
    backend.shutdown().unwrap();
}

#[test]
fn event_mailbox_coalesces_telemetry_without_crossing_ordered_events() {
    let first = RunId::new(1);
    let second = RunId::new(2);
    let mut mailbox = EventMailbox::default();

    mailbox.push(BackendEvent::Position {
        run: first,
        millis: 10,
    });
    mailbox.push(BackendEvent::Position {
        run: first,
        millis: 20,
    });
    mailbox.push(BackendEvent::Duration {
        run: first,
        millis: 90,
    });
    mailbox.push(BackendEvent::Position {
        run: second,
        millis: 30,
    });
    mailbox.push(BackendEvent::State {
        run: first,
        state: BackendState::Playing,
    });
    mailbox.push(BackendEvent::Position {
        run: first,
        millis: 40,
    });
    mailbox.push(BackendEvent::Position {
        run: first,
        millis: 50,
    });
    mailbox.push(BackendEvent::Buffering {
        run: first,
        percent: 10,
    });
    mailbox.push(BackendEvent::Buffering {
        run: first,
        percent: 100,
    });
    mailbox.push(BackendEvent::Ended { run: first });

    assert_eq!(
        mailbox.drain(),
        vec![
            BackendEvent::Position {
                run: first,
                millis: 20,
            },
            BackendEvent::Duration {
                run: first,
                millis: 90,
            },
            BackendEvent::Position {
                run: second,
                millis: 30,
            },
            BackendEvent::State {
                run: first,
                state: BackendState::Playing,
            },
            BackendEvent::Position {
                run: first,
                millis: 50,
            },
            BackendEvent::Buffering {
                run: first,
                percent: 100,
            },
            BackendEvent::Ended { run: first },
        ]
    );
}

fn constant_tone(directory: &std::path::Path, name: &str, frames: u32) -> ResolvedStream {
    let path = directory.join(name);
    write_silent_mono_wave(&path, frames);
    let mut wav = std::fs::read(&path).unwrap();
    for sample in wav[44..].chunks_exact_mut(2) {
        sample.copy_from_slice(&8192_i16.to_le_bytes());
    }
    std::fs::write(&path, wav).unwrap();
    ResolvedStream::new(gst::glib::filename_to_uri(&path, None).unwrap().as_str())
}

/// Records levels on the streaming thread as buffers render. Reading them from the
/// test loop lets the appsink queue fill, and retiring a pipeline flushes it unread.
fn record_levels(
    pipeline: &PlayerPipeline,
    levels: &Arc<Mutex<Vec<f32>>>,
    recording: &mut Option<gstreamer_app::AppSink>,
) {
    let Some(sink) = pipeline
        .audio_output()
        .and_then(|output| output.downcast::<gstreamer_app::AppSink>().ok())
    else {
        return;
    };
    if recording.as_ref() == Some(&sink) {
        return;
    }
    let drain = {
        let levels = Arc::clone(levels);
        move |sink: &gstreamer_app::AppSink| {
            let mut levels = lock_recover(&levels);
            while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
                let map = sample.buffer().unwrap().map_readable().unwrap();
                let values = map
                    .as_slice()
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                    .collect::<Vec<_>>();
                levels.push(values.iter().sum::<f32>() / values.len() as f32);
            }
        }
    };
    drain(&sink);
    sink.set_callbacks(
        gstreamer_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                drain(sink);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    *recording = Some(sink);
}

#[test]
fn crossfade_envelopes_follow_rendered_audio_through_a_refill() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
        audio_output: Some("appsink".into()),
        volume: 1.0,
        volume_scale: VolumeScale::Linear,
        fade_on_status_change: false,
        ..BackendAudioSettings::default()
    }));
    engine.handle_command(BackendCommand::Start {
        run: RunId::new(1),
        current: constant_tone(directory.path(), "current.wav", 24_000).into(),
        next: Some(PreparedNext::new(
            RunId::new(2),
            constant_tone(directory.path(), "next.wav", 32_000),
            NextTransition::Crossfade {
                duration_millis: 1_000,
            },
        )),
        start_position_millis: 0,
    });
    let full = 0.25_f32;
    let outgoing = Arc::new(Mutex::new(Vec::new()));
    let incoming = Arc::new(Mutex::new(Vec::new()));
    let (mut outgoing_sink, mut incoming_sink) = (None, None);
    let rendered = || lock_recover(&outgoing).len() + lock_recover(&incoming).len();
    let mut held: Option<(Instant, PipelineId, usize)> = None;
    let mut released = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline
        && !(released
            && engine.overlap().is_none()
            && lock_recover(&incoming)
                .last()
                .is_some_and(|level| *level >= full * 0.99))
    {
        record_levels(&engine.primary, &outgoing, &mut outgoing_sink);
        record_levels(&engine.secondary, &incoming, &mut incoming_sink);
        engine.poll_bus();
        engine.tick();
        for event in lock_recover(&events).drain() {
            if let BackendEvent::Error { error, .. } = event {
                panic!("{error:?}");
            }
        }
        if held.is_none() && engine.overlap().is_some() && lock_recover(&incoming).len() >= 10 {
            let id = lock_recover(&engine.shared)
                .pipeline_id(Slot::Secondary)
                .unwrap();
            // A refilling incoming stream holds both halves of the crossfade.
            engine.handle_buffering(Slot::Secondary, id, 50, gst::BufferingMode::Stream);
            held = Some((Instant::now(), id, rendered()));
        }
        if let Some((started, id, before)) = held
            && !released
            && started.elapsed() >= Duration::from_millis(400)
        {
            assert!(rendered() <= before + 20, "a held crossfade kept rendering");
            engine.handle_buffering(Slot::Secondary, id, 100, gst::BufferingMode::Stream);
            released = true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    engine.shutdown();
    assert!(released, "the crossfade never started");
    let outgoing: Vec<f32> = lock_recover(&outgoing).clone();
    let incoming: Vec<f32> = lock_recover(&incoming).clone();
    let smooth = |levels: &[f32], falling: bool| {
        levels.windows(2).all(|pair| {
            let step = if falling {
                pair[0] - pair[1]
            } else {
                pair[1] - pair[0]
            };
            (-0.001..full * 0.08).contains(&step)
        })
    };
    assert!(
        smooth(&outgoing, true) && *outgoing.last().unwrap() < full * 0.15,
        "outgoing fade jumped or ended loud: {outgoing:?}"
    );
    assert!(
        smooth(&incoming, false) && incoming[0] < full * 0.15,
        "incoming fade jumped or started loud: {incoming:?}"
    );
}

#[test]
fn failed_queued_uri_drains_the_current_track_and_ends_it() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
        audio_output: Some("appsink".into()),
        ..BackendAudioSettings::default()
    }));
    let current_run = RunId::new(1);
    let missing = directory.path().join("missing.wav");
    engine.handle_command(BackendCommand::Start {
        run: current_run,
        current: constant_tone(directory.path(), "current.wav", 48_000).into(),
        next: Some(PreparedNext::new(
            RunId::new(2),
            ResolvedStream::new(gst::glib::filename_to_uri(&missing, None).unwrap().as_str()),
            NextTransition::Gapless,
        )),
        start_position_millis: 0,
    });
    let rendered = Arc::new(Mutex::new(Vec::new()));
    let mut rendering = None;
    let mut rendered_at_failure = None;
    let mut ended = false;
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline && !ended {
        record_levels(&engine.primary, &rendered, &mut rendering);
        engine.poll_bus();
        engine.tick();
        for event in lock_recover(&events).drain() {
            match event {
                BackendEvent::NextPreparationFailed { error, .. } => {
                    assert_eq!(error.kind(), FailureKind::UnavailableMedia);
                    rendered_at_failure = Some(lock_recover(&rendered).len());
                }
                BackendEvent::Ended { run } => {
                    assert_eq!(run, current_run);
                    ended = true;
                }
                BackendEvent::Error { error, .. } => panic!("{error:?}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    engine.shutdown();
    let rendered_at_failure = rendered_at_failure.expect("the queued URI did not fail");
    assert!(ended, "the drained track never ended");
    assert!(
        lock_recover(&rendered).len() > rendered_at_failure,
        "no queued audio played after the failure"
    );
}

#[test]
fn clearing_preloaded_next_prevents_its_audio_from_playing() {
    ensure_gstreamer_initialized().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let paths = [
        directory.path().join("current.wav"),
        directory.path().join("removed.wav"),
    ];
    for (index, path) in paths.iter().enumerate() {
        write_silent_mono_wave(path, 16_000);
        let mut wav = std::fs::read(path).unwrap();
        for bytes in wav[44..].chunks_exact_mut(2) {
            bytes.copy_from_slice(&(if index == 0 { -8192_i16 } else { 8192_i16 }).to_le_bytes());
        }
        std::fs::write(path, wav).unwrap();
    }
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    engine.handle_command(BackendCommand::ConfigureAudio(BackendAudioSettings {
        audio_output: Some("appsink".into()),
        fade_on_status_change: false,
        ..BackendAudioSettings::default()
    }));
    engine.handle_command(BackendCommand::Start {
        run: RunId::new(1),
        current: ResolvedStream::new(
            gst::glib::filename_to_uri(&paths[0], None)
                .unwrap()
                .as_str(),
        )
        .into(),
        next: Some(PreparedNext::new(
            RunId::new(2),
            ResolvedStream::new(
                gst::glib::filename_to_uri(&paths[1], None)
                    .unwrap()
                    .as_str(),
            ),
            NextTransition::Gapless,
        )),
        start_position_millis: 0,
    });
    let sink = engine
        .active_pipeline()
        .audio_output()
        .unwrap()
        .downcast::<gstreamer_app::AppSink>()
        .unwrap();
    let mut cleared = false;
    let mut ended = false;
    let mut samples = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !ended {
        engine.poll_bus();
        engine.tick();
        if !cleared && engine.primary.has_pending_gapless() && !samples.is_empty() {
            engine.handle_command(BackendCommand::PrepareNext {
                current_run: RunId::new(1),
                next: None,
            });
            cleared = true;
        }
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
            let map = sample.buffer().unwrap().map_readable().unwrap();
            samples.extend(
                map.as_slice()
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())),
            );
        }
        for event in lock_recover(&events).drain() {
            match event {
                BackendEvent::Transitioned { .. } => {
                    assert!(cleared, "next track started before the test issued removal");
                    panic!("removed track started");
                }
                BackendEvent::Ended { run } => {
                    assert_eq!(run, RunId::new(1));
                    ended = true;
                }
                BackendEvent::Error { error, .. } => panic!("{error:?}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    engine.shutdown();
    assert!(cleared, "next track was not preloaded");
    assert!(ended, "current track did not finish");
    assert_eq!(samples.len(), 16_000);
    assert!(
        samples
            .iter()
            .all(|sample| (*sample + 0.25).abs() < 0.00001)
    );
}

#[test]
fn gapless_preload_cannot_relabel_the_next_duration_as_current() {
    ensure_gstreamer_initialized().expect("initialize GStreamer");
    let events = Arc::new(Mutex::new(EventMailbox::default()));
    let mut engine = GstEngine::new(Arc::clone(&events));
    let current_run = RunId::new(1);
    let next_run = RunId::new(2);
    let current = PreparedRun {
        run: current_run,
        stream: ResolvedStream::new("file:///music/current.flac").into(),
    };
    let settings = BackendAudioSettings {
        audio_output: Some("fakesink".to_string()),
        ..BackendAudioSettings::default()
    };
    engine
        .start_pipeline(
            Slot::Primary,
            &current,
            &settings,
            settings.volume,
            settings.muted,
            gst::State::Null,
            None,
            None,
        )
        .expect("create inactive session");
    {
        let mut shared = lock_recover(&engine.shared);
        shared.current = Some(current);
        lock_recover(engine.primary.gapless().unwrap()).pending = Some(PreparedNext::new(
            next_run,
            ResolvedStream::new("file:///music/next.flac"),
            NextTransition::Gapless,
        ));
    }

    engine.push_duration(240_000);
    assert!(lock_recover(&events).drain().is_empty());

    engine.handle_stream_start();
    lock_recover(&events).drain();
    engine.push_duration(240_000);
    assert_eq!(
        lock_recover(&events).drain(),
        vec![BackendEvent::Duration {
            run: next_run,
            millis: 240_000,
        }]
    );
    engine.shutdown();
}
