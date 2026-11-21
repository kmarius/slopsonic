//! User intent and presentation for configured music sources.
//!
//! Provider configuration and lifecycle policy stay in Slopsonic and Sources. UI
//! sees only form values, settings-derived summaries, and one operation state.

use std::sync::Arc;

use sources::CredentialPreset;

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ServerSummary {
    pub kind: sources::SubsonicFlavor,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ServerCacheSummary {
    pub selected_music_folder_name: Option<String>,
    pub album_count: usize,
    pub track_count: usize,
}

/// The configured source together with its cache health.
#[derive(Clone, Debug)]
pub struct ServerStatus {
    pub source: ServerSummary,
    pub cache: ServerCacheSummary,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct EditableServer {
    pub source: ServerSummary,
    pub credentials: CredentialPreset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerProgressStage {
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
pub struct ServerProgress {
    pub stage: ServerProgressStage,
    pub completed: usize,
    pub total: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerOperation {
    Idle,
    Adding { progress: ServerProgress },
    Switching { progress: ServerProgress },
    Refreshing { progress: ServerProgress },
    Downloading { progress: ServerProgress },
    Failed { message: String, add_form: bool },
}

impl ServerOperation {
    pub fn blocks_library(&self) -> bool {
        matches!(self, Self::Adding { .. } | Self::Switching { .. })
    }
}

pub fn server_progress_text(progress: &ServerProgress) -> String {
    let stage = match progress.stage {
        ServerProgressStage::Connecting => {
            return "Connecting to music server...".to_string();
        }
        ServerProgressStage::Finalizing => return "Preparing library...".to_string(),
        ServerProgressStage::Albums => "Fetching albums".to_string(),
        ServerProgressStage::Tracks => "Scanning tracks".to_string(),
        ServerProgressStage::Artists => "Fetching artists".to_string(),
        ServerProgressStage::Genres => "Fetching genres".to_string(),
        ServerProgressStage::Playlists => "Fetching playlists".to_string(),
        ServerProgressStage::Home => "Fetching Home".to_string(),
        ServerProgressStage::Artwork => "Fetching artwork".to_string(),
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

pub fn source_download_text(progress: &ServerProgress) -> String {
    let message = match progress.stage {
        ServerProgressStage::Artwork => "Downloading artwork... {count} checked",
        _ => return server_progress_text(progress),
    };
    crate::text::fill(message, &[("count", &progress.completed.to_string())])
}

pub fn server_operation_text(operation: &ServerOperation) -> Option<String> {
    match operation {
        ServerOperation::Idle => None,
        ServerOperation::Downloading { progress, .. } => Some(source_download_text(progress)),
        ServerOperation::Adding { progress } | ServerOperation::Refreshing { progress, .. } => {
            Some(server_progress_text(progress))
        }
        ServerOperation::Switching { .. } => Some("Switching library...".to_string()),
        ServerOperation::Failed { message, .. } => Some(message.clone()),
    }
}

pub type ServerHandle = Arc<crate::source::ServerOwner>;

pub fn source_kind_title(kind: sources::SubsonicFlavor) -> &'static str {
    kind.display_name()
}

pub fn source_kind_icon_name(kind: sources::SubsonicFlavor) -> &'static str {
    match kind {
        sources::SubsonicFlavor::Navidrome => "io.github.kmarius.slopsonic.source.navidrome",
        sources::SubsonicFlavor::Subsonic => "io.github.kmarius.slopsonic.source.opensubsonic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress() -> ServerProgress {
        ServerProgress {
            stage: ServerProgressStage::Connecting,
            completed: 0,
            total: None,
        }
    }

    #[test]
    fn adding_and_switching_gate_the_selected_library() {
        assert!(
            ServerOperation::Adding {
                progress: progress()
            }
            .blocks_library()
        );
        assert!(
            ServerOperation::Switching {
                progress: progress(),
            }
            .blocks_library()
        );
        assert!(
            !ServerOperation::Refreshing {
                progress: progress(),
            }
            .blocks_library()
        );
    }
}
