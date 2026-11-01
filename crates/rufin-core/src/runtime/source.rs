//! User intent and presentation for configured music sources.
//!
//! Provider configuration and lifecycle policy stay in Rufin and Sources. UI
//! sees only form values, settings-derived summaries, and one operation state.

use std::sync::Arc;

use sources::{CredentialPreset, SourceId};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SourceSummary {
    pub id: SourceId,
    pub kind: String,
    pub name: String,
    pub half_stars_enabled: bool,
    pub supports_playlist_public: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SourceCacheSummary {
    pub source_id: SourceId,
    pub selected_music_folder_name: Option<String>,
    pub album_count: usize,
    pub track_count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ConfiguredSources {
    pub sources: Vec<SourceSummary>,
    pub selected_source_id: Option<SourceId>,
    pub caches: Vec<SourceCacheSummary>,
}

impl ConfiguredSources {
    pub fn half_stars_enabled(&self, media_uri: &str, source_id: Option<&str>) -> bool {
        let uri_source = library::source_entity_parts(media_uri).map(|(source, _, _)| source);
        let source_id = source_id
            .or_else(|| uri_source.as_ref().map(SourceId::as_str))
            .or_else(|| self.selected_source_id.as_ref().map(SourceId::as_str));
        self.sources
            .iter()
            .find(|source| Some(source.id.as_str()) == source_id)
            .is_some_and(|source| source.half_stars_enabled)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct EditableSource {
    pub source: SourceSummary,
    pub credentials: CredentialPreset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceProgressStage {
    Connecting,
    Albums,
    Tracks,
    Artists,
    Genres,
    Playlists,
    Home,
    Artwork,
    Finalizing,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct SourceProgress {
    pub stage: SourceProgressStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SourceOperation {
    Idle,
    Adding {
        progress: SourceProgress,
    },
    Switching {
        target: SourceId,
        progress: SourceProgress,
    },
    Refreshing {
        source_id: SourceId,
        progress: SourceProgress,
    },
    Downloading {
        source_id: SourceId,
        progress: SourceProgress,
    },
    Failed {
        source_id: Option<SourceId>,
        message: String,
        add_form: bool,
    },
}

impl SourceOperation {
    pub fn blocks_library(&self) -> bool {
        matches!(self, Self::Adding { .. } | Self::Switching { .. })
    }
}

pub fn source_progress_text(progress: &SourceProgress) -> String {
    let stage = match progress.stage {
        SourceProgressStage::Connecting => {
            return "Connecting to music server...".to_string();
        }
        SourceProgressStage::Finalizing => return "Preparing library...".to_string(),
        SourceProgressStage::Albums => "Fetching albums".to_string(),
        SourceProgressStage::Tracks => "Scanning tracks".to_string(),
        SourceProgressStage::Artists => "Fetching artists".to_string(),
        SourceProgressStage::Genres => "Fetching genres".to_string(),
        SourceProgressStage::Playlists => "Fetching playlists".to_string(),
        SourceProgressStage::Home => "Fetching Home".to_string(),
        SourceProgressStage::Artwork => "Fetching artwork".to_string(),
    };
    let completed = progress.completed.to_string();
    match progress.total {
        Some(total) if total > 0 => crate::text::fill(
            "{stage}: {completed} / {total}",
            &[
                ("stage", &stage),
                ("completed", &completed),
                ("total", &total.to_string()),
            ],
        ),
        _ => crate::text::fill(
            "{stage}: {completed}",
            &[("stage", &stage), ("completed", &completed)],
        ),
    }
}

pub fn source_download_text(progress: &SourceProgress) -> String {
    let message = match progress.stage {
        SourceProgressStage::Artwork => "Downloading artwork... {count} checked",
        _ => return source_progress_text(progress),
    };
    crate::text::fill(message, &[("count", &progress.completed.to_string())])
}

pub fn source_operation_text(operation: &SourceOperation) -> Option<String> {
    match operation {
        SourceOperation::Idle => None,
        SourceOperation::Downloading { progress, .. } => Some(source_download_text(progress)),
        SourceOperation::Adding { progress } | SourceOperation::Refreshing { progress, .. } => {
            Some(source_progress_text(progress))
        }
        SourceOperation::Switching { .. } => Some("Switching library...".to_string()),
        SourceOperation::Failed { message, .. } => Some(message.clone()),
    }
}

pub type SourceHandle = Arc<crate::source::SourceOwner>;

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> SourceProgress {
        SourceProgress {
            stage: SourceProgressStage::Connecting,
            completed: 0,
            total: None,
        }
    }

    #[test]
    fn adding_and_switching_gate_the_selected_library() {
        assert!(
            SourceOperation::Adding {
                progress: progress()
            }
            .blocks_library()
        );
        assert!(
            SourceOperation::Switching {
                target: SourceId::new("target"),
                progress: progress(),
            }
            .blocks_library()
        );
        assert!(
            !SourceOperation::Refreshing {
                source_id: SourceId::new("selected"),
                progress: progress(),
            }
            .blocks_library()
        );
    }
}

pub fn source_kind_title(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "navidrome" => "Navidrome",
        "subsonic" => "OpenSubsonic",
        _ => return None,
    })
}

pub fn source_kind_icon_name(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "navidrome" => "io.github.kmarius.slopsonic.source.navidrome",
        "subsonic" => "io.github.kmarius.slopsonic.source.opensubsonic",
        _ => return None,
    })
}
