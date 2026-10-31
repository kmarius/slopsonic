//! Sequential bounded loudness analysis for one missing Track or Album.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use audio_processing::{LoudnessAnalysis, album_loudness, analyze_loudness_cancellable};
use library::{Database, LoudnessMeasurement, ReadCancellation, SourceKey};
use playback::{LoudnessNormalization, LoudnessNormalizationScope, StreamQuality, StreamRequest};
use tracing::{info, warn};

use crate::playback::prepare_stream;
use crate::source::SourceOwner;

struct ActiveAnalysis {
    source_key: SourceKey,
    source_owner: Weak<SourceOwner>,
    cancelled: Arc<AtomicBool>,
    task: Option<tokio::task::AbortHandle>,
    restart: bool,
    scope: LoudnessNormalizationScope,
}

impl ActiveAnalysis {
    fn cancel(mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) struct LoudnessAnalysisOwner {
    runtime: tokio::runtime::Handle,
    active: Mutex<Option<ActiveAnalysis>>,
    blocked: Mutex<AnalysisFailureBlock>,
}

#[derive(Default)]
struct AnalysisFailureBlock(Option<SourceKey>);

impl AnalysisFailureBlock {
    fn blocks(&self, source: SourceKey) -> bool {
        self.0 == Some(source)
    }

    fn record(&mut self, source: SourceKey) {
        self.0 = Some(source);
    }

    fn clear(&mut self) {
        self.0 = None;
    }
}

impl LoudnessAnalysisOwner {
    pub(crate) fn new(runtime: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            active: Mutex::new(None),
            blocked: Mutex::new(AnalysisFailureBlock::default()),
        })
    }

    pub(crate) fn settings_changed(
        self: &Arc<Self>,
        normalization: LoudnessNormalization,
        scope: LoudnessNormalizationScope,
            source_owner: Option<Arc<SourceOwner>>,
    ) {
        if normalization != LoudnessNormalization::EbuR128 {
            self.cancel();
            return;
        }
        let Some(source_owner) = source_owner else {
            self.cancel();
            return;
        };
        let Some(selected) = source_owner.selected_library() else {
            self.cancel();
            return;
        };
        let source_key = selected.source_key;
        if source_owner.configuration(&selected.source_id).is_none() {
            self.cancel();
            return;
        }
        if self
            .blocked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .blocks(source_key)
        {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
            if active.as_ref().is_some_and(|current| {
                current.source_key == source_key
                    && current.scope == scope
            }) {
                return;
            }
            if let Some(previous) = active.take() {
                previous.cancel();
            }
            *active = Some(ActiveAnalysis {
                source_key,
                source_owner: Arc::downgrade(&source_owner),
                cancelled: Arc::clone(&cancelled),
                task: None,
                restart: false,
                scope,
            });
        }
        let owner = Arc::downgrade(self);
        let task_cancelled = Arc::clone(&cancelled);
        let task = self.runtime.spawn(async move {
            info!(%source_key, "analyzing missing loudness data");
            let failed = analyze_selected(
                source_owner,
                source_key,
                scope,
                Arc::clone(&task_cancelled),
            )
            .await;
            if let Some(owner) = owner.upgrade() {
                owner.finish(&task_cancelled, normalization, scope, failed);
            }
        });
        let abort = task.abort_handle();
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(current) = active
            .as_mut()
            .filter(|current| Arc::ptr_eq(&current.cancelled, &cancelled))
        {
            current.task = Some(abort);
        } else {
            abort.abort();
        }
    }

    pub(crate) fn library_changed(
        self: &Arc<Self>,
        normalization: LoudnessNormalization,
        scope: LoudnessNormalizationScope,
            source_owner: Option<Arc<SourceOwner>>,
    ) {
        let Some(source_owner) = source_owner else {
            self.cancel();
            return;
        };
        let Some(selected) = source_owner.selected_library() else {
            self.cancel();
            return;
        };
        self.blocked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(current) = active
            .as_mut()
            .filter(|current| current.source_key == selected.source_key)
        {
            current.restart = true;
            return;
        }
        drop(active);
        self.settings_changed(normalization, scope, Some(source_owner));
    }

    pub(crate) fn cancel(&self) {
        if let Some(active) = self.active.lock().unwrap_or_else(|p| p.into_inner()).take() {
            active.cancel();
        }
    }

    fn finish(
        self: &Arc<Self>,
        cancelled: &Arc<AtomicBool>,
        normalization: LoudnessNormalization,
        scope: LoudnessNormalizationScope,
            failed: bool,
    ) {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        let completed = active
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.cancelled, cancelled))
            .then(|| active.take())
            .flatten();
        let restart = completed.as_ref().and_then(|current| {
            current
                .restart
                .then(|| current.source_owner.upgrade())
                .flatten()
        });
        if failed && let Some(current) = completed.as_ref() {
            self.blocked
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .record(current.source_key);
        }
        drop(active);
        if !failed && let Some(source_owner) = restart {
            self.settings_changed(normalization, scope, Some(source_owner));
        }
    }
}

async fn analyze_selected(
    source_owner: Arc<SourceOwner>,
    source_key: SourceKey,
    scope: LoudnessNormalizationScope,
    cancelled: Arc<AtomicBool>,
) -> bool {
    let database = Arc::clone(&source_owner.shared.database);
    let mut album_after = None;
    let mut track_after = None;
    let mut albums_done = scope != LoudnessNormalizationScope::Album;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return false;
        }
        if !albums_done {
            match database
                .next_missing_album_loudness(source_key, album_after, &ReadCancellation::new())
                .await
            {
                Ok(Some(work)) => {
                    let mut analyses = Vec::with_capacity(work.tracks.len());
                    for track in &work.tracks {
                        match analyze_uri(
                            &source_owner,
                            &database,
                            &track.media_uri,
                            &cancelled,
                        )
                        .await
                        {
                            Ok(analysis) => {
                                let value = analysis.measurement();
                                let measurement = LoudnessMeasurement {
                                    analysis_key: track.expected_analysis_key,
                                    integrated_lufs: value.integrated_lufs,
                                    true_peak: value.true_peak,
                                    replay_gain_db: None,
                                    replay_gain_peak: None,
                                };
                                if let Err(error) = database
                                    .write_track_analyzed_loudness(
                                        source_key,
                                        track.track_key,
                                        &measurement,
                                    )
                                    .await
                                {
                                    warn!(%error, "could not store Track loudness");
                                    return true;
                                }
                                analyses.push(analysis);
                            }
                            Err(error) => {
                                warn!(%error, track_key=%track.track_key, "could not analyze Track loudness");
                                analyses.clear();
                                return true;
                            }
                        }
                    }
                    if !analyses.is_empty() {
                        match album_loudness(&analyses) {
                            Ok(value) => {
                                let measurement = LoudnessMeasurement {
                                    analysis_key: work.expected_analysis_key,
                                    integrated_lufs: value.integrated_lufs,
                                    true_peak: value.true_peak,
                                    replay_gain_db: None,
                                    replay_gain_peak: None,
                                };
                                if let Err(error) = database
                                    .write_album_analyzed_loudness(
                                        source_key,
                                        work.album_key,
                                        &measurement,
                                    )
                                    .await
                                {
                                    warn!(%error, "could not store Album loudness");
                                    return true;
                                }
                            }
                            Err(error) => warn!(%error, "could not combine Album loudness"),
                        }
                    }
                    album_after = Some(work.album_key);
                    continue;
                }
                Ok(None) => albums_done = true,
                Err(error) => {
                    warn!(%error, "could not select Album loudness work");
                    return true;
                }
            }
        }
        match database
            .next_missing_track_loudness(source_key, track_after, &ReadCancellation::new())
            .await
        {
            Ok(Some(work)) => match analyze_uri(
                &source_owner,
                &database,
                &work.media_uri,
                &cancelled,
            )
            .await
            {
                Ok(analysis) => {
                    let value = analysis.measurement();
                    let measurement = LoudnessMeasurement {
                        analysis_key: work.expected_analysis_key,
                        integrated_lufs: value.integrated_lufs,
                        true_peak: value.true_peak,
                        replay_gain_db: None,
                        replay_gain_peak: None,
                    };
                    if let Err(error) = database
                        .write_track_analyzed_loudness(source_key, work.track_key, &measurement)
                        .await
                    {
                        warn!(%error, "could not store Track loudness");
                        return true;
                    }
                    track_after = Some(work.track_key);
                }
                Err(error) => {
                    warn!(%error, track_key=%work.track_key, "could not analyze Track loudness");
                    return true;
                }
            },
            Ok(None) => {
                if album_after.is_none() && track_after.is_none() {
                    return false;
                }
                // Access and metadata changes can invalidate already visited keys.
                // Recheck from the beginning once both forward passes are exhausted.
                album_after = None;
                track_after = None;
                albums_done = scope != LoudnessNormalizationScope::Album;
            }
            Err(error) => {
                warn!(%error, "could not select Track loudness work");
                return true;
            }
        }
    }
}

async fn analyze_uri(
    source_owner: &Arc<SourceOwner>,
    database: &Database,
    media_uri: &str,
    cancelled: &Arc<AtomicBool>,
) -> Result<LoudnessAnalysis, String> {
    let request = StreamRequest::new(media_uri, StreamQuality::Original);
    let owner = Arc::clone(source_owner);
    let stream = prepare_stream(
        database,
        request,
        move |source_id| async move {
            tokio::task::spawn_blocking(move || owner.client(&source_id))
                .await
                .map_err(|error| error.to_string())?
        },
    )
    .await?;
    let cancelled = Arc::clone(cancelled);
    tokio::task::spawn_blocking(move || {
        analyze_loudness_cancellable(&stream, || cancelled.load(Ordering::Acquire))
    })
    .await
    .map_err(|_| "loudness analysis worker stopped".to_string())?
}

#[cfg(test)]
mod tests {
    use super::AnalysisFailureBlock;
    use library::SourceKey;

    #[test]
    fn one_failed_analysis_blocks_immediate_restart_until_library_change() {
        let source = SourceKey::from_raw(1);
        let mut blocked = AnalysisFailureBlock::default();
        assert!(!blocked.blocks(source));
        blocked.record(source);
        for _ in 0..10_000 {
            assert!(blocked.blocks(source));
        }
        blocked.clear();
        assert!(!blocked.blocks(source));
    }
}
