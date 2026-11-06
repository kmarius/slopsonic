//! Live processing-spectrum sampling and bounded spectrum analysis.

use super::engine::{EventMailbox, PipelineId, SharedBackendState, Slot, push_event};
use super::lock_recover;
use audio_processing::SpectrumFft;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_audio as gst_audio;
use playback::{BackendEvent, RunId};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::warn;

const MEASUREMENT_CHANNEL_CAPACITY: usize = 2;
const MEASUREMENT_COPY_FRAMES: usize = 4_096;

#[derive(Clone)]
pub(super) struct MeasurementTap {
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
    sender: SyncSender<MeasurementFrame>,
    output: String,
}

impl MeasurementTap {
    pub(super) fn measurement_key(&self) -> (PipelineId, RunId, String) {
        (self.pipeline_id, self.run, self.output.clone())
    }

    pub(super) fn install(&self, pad: &gst::Pad) -> Option<gst::PadProbeId> {
        let tap = self.clone();
        let info = pad
            .current_caps()
            .and_then(|caps| gst_audio::AudioInfo::from_caps(&caps).ok());
        let pool = Arc::new(Mutex::new(Vec::<Vec<f32>>::new()));
        if let Some(info) = &info {
            *lock_recover(&pool) = (0..3)
                .map(|_| Vec::with_capacity(MEASUREMENT_COPY_FRAMES * info.channels() as usize))
                .collect();
        }
        let info = Mutex::new(info);
        pad.add_probe(
            gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
            move |_, probe| {
                let mut info = lock_recover(&info);
                if let Some(event) = probe.event()
                    && let gst::EventView::Caps(caps) = event.view()
                {
                    *info = gst_audio::AudioInfo::from_caps(caps.caps()).ok();
                    if let Some(info) = info.as_ref() {
                        *lock_recover(&pool) = (0..3)
                            .map(|_| {
                                Vec::with_capacity(
                                    MEASUREMENT_COPY_FRAMES * info.channels() as usize,
                                )
                            })
                            .collect();
                    }
                }
                let Some(buffer) = probe.buffer() else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(info) = info.as_ref() else {
                    return gst::PadProbeReturn::Ok;
                };
                let Some(mut samples) = pool.try_lock().ok().and_then(|mut pool| pool.pop()) else {
                    return gst::PadProbeReturn::Ok;
                };
                if samples.capacity() < MEASUREMENT_COPY_FRAMES * info.channels() as usize {
                    return gst::PadProbeReturn::Ok;
                }
                samples.clear();
                if let Ok(map) = buffer.map_readable() {
                    let sample_size = (info.width() / 8) as usize;
                    let count = (map.len() / sample_size)
                        .min(MEASUREMENT_COPY_FRAMES * info.channels() as usize);
                    for bytes in map.as_slice()[..count * sample_size].chunks_exact(sample_size) {
                        let value = match info.format() {
                            gst_audio::AudioFormat::F64le => {
                                f64::from_le_bytes(bytes.try_into().unwrap()) as f32
                            }
                            gst_audio::AudioFormat::F64be => {
                                f64::from_be_bytes(bytes.try_into().unwrap()) as f32
                            }
                            gst_audio::AudioFormat::F32le => {
                                f32::from_le_bytes(bytes.try_into().unwrap())
                            }
                            gst_audio::AudioFormat::F32be => {
                                f32::from_be_bytes(bytes.try_into().unwrap())
                            }
                            _ => 0.0,
                        };
                        samples.push(if value.is_finite() { value } else { 0.0 });
                    }
                }
                let frame = MeasurementFrame {
                    slot: tap.slot,
                    pipeline_id: tap.pipeline_id,
                    run: tap.run,
                    samples,
                    sample_rate: info.rate(),
                    output: tap.output.clone(),
                    channels: info.channels() as usize,
                    pool: Arc::clone(&pool),
                };
                if let Err(error) = tap.sender.try_send(frame) {
                    let frame = match error {
                        TrySendError::Full(frame) | TrySendError::Disconnected(frame) => frame,
                    };
                    lock_recover(&pool).push(frame.samples);
                }
                gst::PadProbeReturn::Ok
            },
        )
    }
}

struct MeasurementFrame {
    slot: Slot,
    pipeline_id: PipelineId,
    run: RunId,
    samples: Vec<f32>,
    sample_rate: u32,
    output: String,
    channels: usize,
    pool: Arc<Mutex<Vec<Vec<f32>>>>,
}

pub(super) struct MeasurementAnalyzer {
    sender: SyncSender<MeasurementFrame>,
}

impl MeasurementAnalyzer {
    pub(super) fn new(
        events: Arc<Mutex<EventMailbox>>,
        shared: Arc<Mutex<SharedBackendState>>,
    ) -> Self {
        let (sender, receiver) = sync_channel(MEASUREMENT_CHANNEL_CAPACITY);
        let _ = thread::Builder::new()
            .name("slopsonic-spectrum-fft".to_string())
            .spawn(move || run_measurement_worker(receiver, events, shared))
            .inspect_err(|error| warn!(%error, "failed to start spectrum FFT worker"));
        Self { sender }
    }

    pub(super) fn tap(
        &self,
        slot: Slot,
        pipeline_id: PipelineId,
        run: RunId,
        output: String,
    ) -> MeasurementTap {
        MeasurementTap {
            slot,
            pipeline_id,
            run,
            sender: self.sender.clone(),
            output,
        }
    }
}

fn run_measurement_worker(
    receiver: Receiver<MeasurementFrame>,
    events: Arc<Mutex<EventMailbox>>,
    shared: Arc<Mutex<SharedBackendState>>,
) {
    let mut measured_stream = None;
    let mut channel_ffts = Vec::<SpectrumFft>::new();
    let mut last_spectrum = std::time::Instant::now();
    while let Ok(frame) = receiver.recv() {
        let MeasurementFrame {
            slot,
            pipeline_id,
            run,
            samples,
            sample_rate,
            output,
            channels,
            pool,
        } = frame;
        let live = {
            let shared = lock_recover(&shared);
            shared.pipeline_is_current(slot, pipeline_id)
                && shared
                    .current
                    .as_ref()
                    .is_some_and(|current| current.run == run)
                && shared
                    .processing
                    .as_ref()
                    .is_some_and(|p| p.spectrum && p.output == output)
        };
        if live {
            let stream = (pipeline_id, run, sample_rate, channels);
            if measured_stream != Some(stream) {
                channel_ffts = (0..channels).map(|_| SpectrumFft::new()).collect();
                measured_stream = Some(stream);
            }
            for (channel, fft) in channel_ffts.iter_mut().enumerate() {
                fft.push_channel_samples(&samples, channels, channel);
            }
            if last_spectrum.elapsed() >= std::time::Duration::from_millis(33) {
                let mut powers = Vec::<f64>::new();
                for fft in &mut channel_ffts {
                    if let Some(channel) = fft.spectrum_power() {
                        if powers.is_empty() {
                            powers = channel;
                        } else {
                            for (sum, value) in powers.iter_mut().zip(channel) {
                                *sum += value;
                            }
                        }
                    }
                }
                if !powers.is_empty() {
                    let spectrum_db = powers
                        .into_iter()
                        .map(|p| (10.0 * (p / channels as f64).max(1e-16).log10()).max(-160.0))
                        .collect();
                    push_event(
                        &events,
                        BackendEvent::ProcessingMeter {
                            output,
                            peak_db: Vec::new(),
                            spectrum_db,
                        },
                    );
                    last_spectrum = std::time::Instant::now();
                }
            }
        }
        let mut available = lock_recover(&pool);
        if available.len() < 3 {
            available.push(samples);
        }
    }
}
