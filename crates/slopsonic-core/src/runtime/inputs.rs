use crate::SettingsHandle;
use library::{FolderChoice, FolderKey, PlaylistKey};
use std::sync::Arc;

use super::source::{ServerOperation, ServerStatus};
use super::{DiagnosticsHandle, ProductHandles, ProductReceivers};

#[derive(Clone)]
pub struct SelectedLibrary {
    pub music_folder_key: Option<FolderKey>,
    pub music_folder_object_id: Option<String>,
    pub music_folders: Arc<[FolderChoice]>,
}

pub enum ServerEvent {
    Configured(Option<ServerStatus>),
    Selected {
        configured: Option<ServerStatus>,
        selected: SelectedLibrary,
    },
    Operation(ServerOperation),
    CatalogPublished(CatalogPublication),
    Notice(ServerNotice),
    ReleaseSelected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogPublication {
    pub favorite: Option<FavoriteSettlement>,
    pub change: CatalogChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogChange {
    Activity,
    Scope,
    Broad,
    Home,
    Playlists(Option<PlaylistKey>),
    Album(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogRevision {
    pub catalog: u64,
    pub activity: u64,
    pub change: Option<CatalogChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FavoriteSettlement {
    pub target: library::FavoriteTarget,
    pub requested: bool,
    pub effective: bool,
}

#[derive(Clone, Debug)]
pub struct ServerNotice {
    pub kind: ServerNoticeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerNoticeKind {
    ServerUnreachable,
    FavoriteRejected,
}

pub struct RuntimeInputs {
    pub temporary_store: bool,
    pub diagnostics: DiagnosticsHandle,
    pub products: ProductHandles,
    pub settings: SettingsHandle,
    pub receivers: ProductReceivers,
    pub server_status: Option<ServerStatus>,
    pub source_operation: ServerOperation,
}
