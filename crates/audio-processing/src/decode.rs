use crate::{ResolvedStream, configure_sources, ensure_gstreamer_initialized};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app::AppSink;

pub(crate) const SAMPLE_POLL: gst::ClockTime = gst::ClockTime::from_mseconds(100);

pub(crate) struct DecodePipeline {
    pub pipeline: gst::Element,
    pub bus: gst::Bus,
    pub sink: gst::Element,
}

impl Drop for DecodePipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl DecodePipeline {
    pub fn new(stream: &ResolvedStream, output: &str) -> Result<Self, String> {
        ensure_gstreamer_initialized()?;
        let pipeline = gst::parse::launch(&format!(
            "uridecodebin name=decoder ! audioconvert ! {output}"
        ))
        .map_err(|error| error.to_string())?;
        let bin = pipeline
            .downcast_ref::<gst::Bin>()
            .expect("analysis pipeline");
        let decoder = bin.by_name("decoder").expect("analysis decoder");
        let trust = stream.trust_invalid_certificate();
        configure_sources(&decoder, move || trust);
        decoder.set_property("uri", stream.uri());
        let sink = bin.by_name("sink").expect("analysis sink");
        let bus = pipeline.bus().expect("analysis bus");
        Ok(Self {
            pipeline,
            bus,
            sink,
        })
    }

    pub fn start(&self, check: impl Fn() -> Result<(), String>) -> Result<(), String> {
        check()?;
        self.pipeline
            .set_state(gst::State::Playing)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn sample(
        &self,
        sink: &AppSink,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Option<gst::Sample>, String> {
        loop {
            check()?;
            if let Some(sample) = sink.try_pull_sample(SAMPLE_POLL) {
                return Ok(Some(sample));
            }
            while let Some(message) = self.bus.pop() {
                if let gst::MessageView::Error(error) = message.view() {
                    return Err(error.error().to_string());
                }
            }
            if sink.is_eos() {
                return Ok(None);
            }
        }
    }
}
