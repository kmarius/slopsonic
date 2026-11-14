mod player;

pub use app::{AppearancePreferences, ApplicationPreferences, PlayerDisplayPreferences};
pub use player::SettingsError;
pub mod app;
pub mod context_menu;
pub mod layout;
mod library_lists;
mod pins;
pub mod presentation;
pub mod sidebar;

pub use app::{
    ExternalSiteLinkSettings, HomeBlockKind, HomeSectionKind, RandomPlayGenreSelection,
    RandomPlaySettings, Settings, SettingsHandle, default_home_blocks,
};
pub use context_menu::{ContextMenuItem, ContextMenuItemSettings, ContextMenuSettings};
pub use downloads::DownloadRule;
pub use downloads::{DownloadRules, DownloadSettings};
pub use layout::{
    AccentPreference, DEFAULT_LEFT_SIDEBAR_WIDTH, DEFAULT_RIGHT_SIDEBAR_WIDTH,
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, LayoutProfile, LayoutSettings, LeftSidebarMode,
    LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings, LibraryListSettingsEntry,
    MAX_LEFT_SIDEBAR_WIDTH, MAX_NARROW_LAYOUT_THRESHOLD, MAX_RESTORED_WINDOW_HEIGHT,
    MAX_RESTORED_WINDOW_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH, MAX_TABLE_COLUMN_WIDTH,
    MIN_LEFT_SIDEBAR_WIDTH, MIN_NARROW_LAYOUT_THRESHOLD, MIN_RIGHT_SIDEBAR_WIDTH,
    MIN_TABLE_COLUMN_WIDTH, RightSidebarMode, ThemePreference, available_detail_track_fields,
    available_grid_fields, available_row_fields, available_sort_fields,
    default_library_list_settings, sanitized_window_size,
};
pub use sidebar::{SidebarPin, SidebarRouteItem, SidebarRouteItemSettings, SidebarSettings};

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use app::Settings as UiSettings;
use scrobbling::Settings as ScrobblingSettings;
use serde::{Deserialize, Serialize};
use sources::SourceConfiguration;
use tracing::warn;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfiguredServer {
    #[serde(flatten)]
    pub(crate) configuration: SourceConfiguration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) music_folder_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ServerSettings {
    #[serde(default)]
    pub(crate) configured: Option<ConfiguredServer>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StoredSettings {
    #[serde(flatten)]
    pub ui: UiSettings,
    #[serde(default)]
    pub(crate) equalizer: crate::playback::EqualizerPreferences,
    #[serde(default)]
    pub(crate) scrobbling: ScrobblingSettings,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) scrobbling_credentials: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) sources: ServerSettings,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            ui: UiSettings::default(),
            equalizer: Default::default(),
            scrobbling: ScrobblingSettings::default(),
            scrobbling_credentials: Default::default(),
            sources: ServerSettings::default(),
        }
    }
}

impl StoredSettings {
    pub(crate) fn migrate_defaults(&mut self) {
        self.scrobbling.lastfm.api_key.clear();
        self.scrobbling.sanitize();
        app::migrate_library_lists(&mut self.ui.library_lists);
        self.ui.sanitize();
    }

    pub(crate) fn scrobbling_runtime_settings(&self) -> ScrobblingSettings {
        let mut settings = self.scrobbling.clone();
        settings.lastfm.api_key = self.ui.lastfm_api_key.clone();
        settings
    }
}

#[derive(Clone)]
pub struct SettingsFile {
    sidebar: tokio::sync::watch::Sender<SidebarSettings>,
    appearance: tokio::sync::watch::Sender<AppearancePreferences>,
    path: Option<PathBuf>,
    value: Arc<Mutex<StoredSettings>>,
    persistence: Arc<Mutex<StoredSettings>>,
    edits: Arc<OnceLock<Result<mpsc::SyncSender<Self>, String>>>,
    persistence_error: tokio::sync::watch::Sender<Option<String>>,
}

impl SettingsFile {
    pub(crate) fn sidebar_changes(&self) -> tokio::sync::watch::Receiver<SidebarSettings> {
        self.sidebar.subscribe()
    }

    pub(crate) fn open(path: PathBuf) -> Result<Self, String> {
        let mut value = read_startup_settings(&path)?;
        let mut changed = false;
        value.migrate_defaults();
        for recent in &mut value.ui.recent_search_results {
            if let Some(binding) = &mut recent.artwork_binding {
                let normalized = library::normalize_artwork_binding(binding)
                    .map_err(|error| error.to_string())?;
                if *binding != normalized {
                    *binding = normalized;
                    changed = true;
                }
            }
        }
        if changed {
            if let Err(error) = write_settings(&path, &value) {
                warn!(%error, "could not save startup settings");
            }
        }
        Ok(Self {
            sidebar: tokio::sync::watch::channel(value.ui.sidebar.clone()).0,
            appearance: tokio::sync::watch::channel(value.ui.appearance.clone()).0,
            path: Some(path),
            value: Arc::new(Mutex::new(value.clone())),
            persistence: Arc::new(Mutex::new(value)),
            edits: Arc::new(OnceLock::new()),
            persistence_error: tokio::sync::watch::channel(None).0,
        })
    }

    pub(crate) fn memory() -> Self {
        let mut value = StoredSettings::default();
        value.migrate_defaults();
        Self {
            path: None,
            sidebar: tokio::sync::watch::channel(value.ui.sidebar.clone()).0,
            appearance: tokio::sync::watch::channel(value.ui.appearance.clone()).0,
            value: Arc::new(Mutex::new(value.clone())),
            persistence: Arc::new(Mutex::new(value)),
            edits: Arc::new(OnceLock::new()),
            persistence_error: tokio::sync::watch::channel(None).0,
        }
    }

    pub fn load(&self) -> StoredSettings {
        self.value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn equalizer_preset(
        &self,
        id: &str,
    ) -> Option<crate::playback::equalizer::SavedPreset> {
        self.value
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .equalizer
            .presets
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub(crate) fn download_artwork_automatically(&self) -> bool {
        let stored = self.value.lock().unwrap_or_else(|p| p.into_inner());
        stored.ui.application.download_artwork_automatically
    }

    /// Apply preferences immediately. The shared writer reports durable failures separately.
    pub(crate) fn edit<T>(
        &self,
        operation: impl FnOnce(&mut StoredSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let writer = if self.path.is_some() {
            Some(
                self.edits
                    .get_or_init(|| {
                        let (sender, receiver) = mpsc::sync_channel::<Self>(1);
                        std::thread::Builder::new()
                            .name("slopsonic-settings".into())
                            .spawn(move || {
                                while let Ok(file) = receiver.recv() {
                                    if let Err(error) = file.flush() {
                                        warn!(%error, "could not save settings");
                                    }
                                }
                            })
                            .map_err(|error| error.to_string())?;
                        Ok(sender)
                    })
                    .as_ref()
                    .map_err(Clone::clone)?,
            )
        } else {
            None
        };
        let mut current = self.value.lock().unwrap_or_else(|p| p.into_inner());
        let mut next = current.clone();
        let output = operation(&mut next)?;
        next.prepare()?;
        if next != *current {
            if let Some(writer) = writer {
                match writer.try_send(self.clone()) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        return Err("Settings writer stopped".into());
                    }
                }
            }
            *current = next;
            self.publish_changes(&current);
        }
        Ok(output)
    }

    pub(crate) fn update<T>(
        &self,
        operation: impl FnOnce(&mut StoredSettings) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut saved = self.persistence.lock().unwrap_or_else(|p| p.into_inner());
        let previous = self.load();
        let mut next = previous.clone();
        let output = operation(&mut next)?;
        next.prepare()?;
        if next != *saved {
            self.persist(&next)?;
        }
        *saved = next.clone();
        let mut current = self.value.lock().unwrap_or_else(|p| p.into_inner());
        // Preference edits can arrive while a background transaction writes to disk.
        // Keep those newer edits when publishing the committed source or configuration.
        merge_preference_edits(&mut next.ui, &previous.ui, &current.ui)?;
        merge_preference_edits(
            &mut next.scrobbling,
            &previous.scrobbling,
            &current.scrobbling,
        )?;
        *current = next;
        self.publish_changes(&current);
        Ok(output)
    }

    pub(crate) fn flush(&self) -> Result<(), String> {
        let mut saved = self.persistence.lock().unwrap_or_else(|p| p.into_inner());
        let result = (|| {
            let next = self.load();
            if next != *saved {
                self.persist(&next)?;
                *saved = next;
            }
            Ok(())
        })();
        self.persistence_error
            .send_replace(result.as_ref().err().cloned());
        result
    }

    fn persist(&self, value: &StoredSettings) -> Result<(), String> {
        self.path
            .as_ref()
            .map_or(Ok(()), |path| write_settings(path, value))
    }
}

fn merge_preference_edits<T: Serialize + serde::de::DeserializeOwned + PartialEq>(
    next: &mut T,
    previous: &T,
    current: &T,
) -> Result<(), String> {
    if previous != current {
        let mut merged = toml::Value::try_from(&*next).map_err(|e| e.to_string())?;
        let previous = toml::Value::try_from(previous).map_err(|e| e.to_string())?;
        let current = toml::Value::try_from(current).map_err(|e| e.to_string())?;
        merge_settings(&mut merged, &previous, &current);
        *next = merged.try_into().map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl StoredSettings {
    fn prepare(&mut self) -> Result<(), String> {
        self.ui.sanitize();
        Ok(())
    }
}

#[derive(Clone)]
pub struct SettingsOwner {
    file: SettingsFile,
    on_change: Arc<dyn Fn(&StoredSettings, &StoredSettings, bool) + Send + Sync>,
}

impl SettingsOwner {
    pub(crate) fn new(
        file: SettingsFile,
        on_change: impl Fn(&StoredSettings, &StoredSettings, bool) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            file,
            on_change: Arc::new(on_change),
        })
    }

    pub(crate) fn restore(
        &self,
        restore: impl FnOnce(&mut StoredSettings) -> Result<(), String>,
        credentials_changed: bool,
    ) -> Result<(), String> {
        let previous = self.file.load();
        self.file.update(restore)?;
        (self.on_change)(&previous, &self.file.load(), credentials_changed);
        Ok(())
    }
}

impl SettingsFile {
    fn publish_changes(&self, stored: &StoredSettings) {
        self.appearance.send_if_modified(|current| {
            if *current == stored.ui.appearance {
                return false;
            }
            *current = stored.ui.appearance.clone();
            true
        });
        self.sidebar.send_if_modified(|current| {
            if *current == stored.ui.sidebar {
                return false;
            }
            *current = stored.ui.sidebar.clone();
            true
        });
    }
}

impl SettingsOwner {
    pub fn persistence_errors(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.file.persistence_error.subscribe()
    }

    /// Finish pending preference saves before shutdown or export.
    pub fn flush(&self) -> Result<(), String> {
        self.file.flush()
    }

    pub fn load(&self) -> UiSettings {
        self.file
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ui
            .clone()
    }

    pub fn sidebar_changes(&self) -> tokio::sync::watch::Receiver<SidebarSettings> {
        self.file.sidebar_changes()
    }

    pub fn set_pinned(&self, pin: SidebarPin, pinned: bool) -> Result<bool, String> {
        self.file
            .update(|stored| Ok(stored.ui.sidebar.set_pinned(pin, pinned)))
    }

    pub fn set_route_visible(&self, item: SidebarRouteItem, visible: bool) -> Result<bool, String> {
        self.file
            .update(|stored| Ok(stored.ui.sidebar.set_route_visible(item, visible)))
    }

    pub fn move_route(&self, item: SidebarRouteItem, delta: isize) -> Result<bool, String> {
        self.file
            .update(|stored| Ok(stored.ui.sidebar.move_route(item, delta)))
    }

    pub fn remember_search_result(&self, result: app::RecentSearchResult) -> Result<bool, String> {
        if self.file.load().ui.recent_search_results.first() == Some(&result) {
            return Ok(false);
        }
        self.file
            .update(|stored| Ok(stored.ui.remember_search_result(result)))
    }

    pub fn reorder_pin(&self, moved: &SidebarPin, target: &SidebarPin) -> Result<bool, String> {
        self.file.update(|stored| {
            let Some(after) = stored.ui.sidebar.pin_drop_after(moved, target) else {
                return Ok(false);
            };
            Ok(stored.ui.sidebar.reorder_pin(moved, target, after))
        })
    }

    /// Apply an edit now; durable failures are published through `persistence_errors`.
    pub fn update_preferences(
        &self,
        edit: impl FnOnce(&mut UiSettings) -> Result<(), String>,
    ) -> Result<(), String> {
        let previous = self.file.load();
        self.file.edit(move |stored| edit(&mut stored.ui))?;
        (self.on_change)(&previous, &self.file.load(), false);
        Ok(())
    }
}

pub(crate) fn read_scrobbling(stored: &StoredSettings) -> ScrobblingSettings {
    let mut settings = stored.scrobbling_runtime_settings();
    for descriptor in scrobbling::secret_descriptors() {
        if let Some(credential) = stored.scrobbling_credentials.get(descriptor.kind()) {
            descriptor.value_mut(&mut settings).clone_from(credential);
        }
    }
    settings.sanitize();
    settings
}

pub(crate) fn store_scrobbling(stored: &mut StoredSettings, input: &ScrobblingSettings) {
    let mut input = input.clone();
    input.sanitize();
    for descriptor in scrobbling::secret_descriptors() {
        let value = descriptor.value(&input);
        if value.is_empty() {
            stored.scrobbling_credentials.remove(descriptor.kind());
        } else {
            stored
                .scrobbling_credentials
                .insert(descriptor.kind().to_owned(), value.to_owned());
        }
        descriptor.value_mut(&mut input).clear();
    }
    stored.ui.lastfm_api_key = input.lastfm.api_key.clone();
    input.lastfm.api_key.clear();
    stored.scrobbling = input;
}

pub(crate) fn persist_scrobbling_settings(
    file: &SettingsFile,
    input: &ScrobblingSettings,
) -> Result<ScrobblingSettings, String> {
    let mut prepared = file.load();
    store_scrobbling(&mut prepared, input);
    let mut committed = input.clone();
    file.update(|stored| {
        prepared.scrobbling.lastfm.enabled = stored.scrobbling.lastfm.enabled;
        prepared.scrobbling.lastfm.now_playing_enabled =
            stored.scrobbling.lastfm.now_playing_enabled;
        prepared.scrobbling.librefm.enabled = stored.scrobbling.librefm.enabled;
        prepared.scrobbling.librefm.now_playing_enabled =
            stored.scrobbling.librefm.now_playing_enabled;
        prepared.scrobbling.listenbrainz.enabled = stored.scrobbling.listenbrainz.enabled;
        prepared.scrobbling.listenbrainz.now_playing_enabled =
            stored.scrobbling.listenbrainz.now_playing_enabled;
        stored.scrobbling = prepared.scrobbling;
        stored.scrobbling_credentials = prepared.scrobbling_credentials;
        stored.ui.lastfm_api_key = prepared.ui.lastfm_api_key;
        committed.lastfm.enabled = stored.scrobbling.lastfm.enabled;
        committed.lastfm.now_playing_enabled = stored.scrobbling.lastfm.now_playing_enabled;
        committed.librefm.enabled = stored.scrobbling.librefm.enabled;
        committed.librefm.now_playing_enabled = stored.scrobbling.librefm.now_playing_enabled;
        committed.listenbrainz.enabled = stored.scrobbling.listenbrainz.enabled;
        committed.listenbrainz.now_playing_enabled =
            stored.scrobbling.listenbrainz.now_playing_enabled;
        Ok(())
    })?;
    Ok(committed)
}

pub(crate) fn load_scrobbling_settings(file: &SettingsFile) -> ScrobblingSettings {
    read_scrobbling(&file.load())
}

fn read_settings_file(path: &Path) -> Result<Option<toml::Value>, String> {
    match fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => toml::from_str(&raw)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn replace_setting(
    root: &mut toml::Value,
    path: &[String],
    value: Option<toml::Value>,
) -> Option<toml::Value> {
    let (key, parents) = path.split_last().unwrap();
    let parent = parents.iter().fold(root, |parent, key| &mut parent[key]);
    let fields = parent.as_table_mut().unwrap();
    match value {
        Some(value) => fields.insert(key.clone(), value),
        None => fields.remove(key),
    }
}

fn recover_setting(
    accepted: &mut toml::Value,
    path: Vec<String>,
    value: &toml::Value,
    unsupported: &mut Vec<Vec<String>>,
) {
    let previous = replace_setting(accepted, &path, Some(value.clone()));
    if accepted.clone().try_into::<StoredSettings>().is_ok() {
        return;
    }
    let recover_children = previous.as_ref().is_some_and(toml::Value::is_table);
    replace_setting(accepted, &path, previous);
    if recover_children && let Some(fields) = value.as_table() {
        for (key, value) in fields {
            let mut child = path.clone();
            child.push(key.clone());
            recover_setting(accepted, child, value, unsupported);
        }
    } else {
        unsupported.push(path);
    }
}

/// Entries for the removed History page would otherwise reset the whole list they sit in.
fn drop_removed_entries(raw: &mut toml::Value) {
    let strip = |list: Option<&mut toml::Value>, field: &str| {
        if let Some(items) = list.and_then(toml::Value::as_array_mut) {
            items.retain(|item| item.get(field).and_then(toml::Value::as_str) != Some("History"));
        }
    };
    strip(
        raw.get_mut("sidebar")
            .and_then(|sidebar| sidebar.get_mut("route_items")),
        "item",
    );
    strip(raw.get_mut("library_lists"), "key");
    // Former per-server pins and import markers drop the server id.
    if let Some(sidebar) = raw.get_mut("sidebar").and_then(toml::Value::as_table_mut) {
        if let Some(imported) = sidebar.remove("playlist_pin_imported_sources") {
            let imported = imported.as_array().is_some_and(|list| !list.is_empty());
            sidebar
                .entry("playlist_pins_imported")
                .or_insert(toml::Value::Boolean(imported));
        }
        for pin in sidebar
            .get_mut("pins")
            .and_then(toml::Value::as_array_mut)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_table_mut)
        {
            for (_, fields) in pin.iter_mut() {
                if let Some(fields) = fields.as_table_mut() {
                    fields.remove("source_id");
                }
            }
        }
    }
    // The former per-server download list keeps only the configured server's entry.
    let configured = raw
        .get("sources")
        .and_then(|sources| sources.get("configured"))
        .and_then(|configured| configured.get("source_id"))
        .cloned();
    if let Some(table) = raw.as_table_mut()
        && let Some(list) = table.get("downloads").and_then(toml::Value::as_array)
    {
        match list
            .iter()
            .find(|entry| configured.is_some() && entry.get("source_id") == configured.as_ref())
            .cloned()
        {
            Some(entry) => table.insert("downloads".into(), entry),
            None => table.remove("downloads"),
        };
    }
}

pub(crate) fn decode_settings(
    raw: &toml::Value,
) -> Result<(StoredSettings, Vec<Vec<String>>), String> {
    let mut raw = raw.clone();
    drop_removed_entries(&mut raw);
    let raw = &raw;
    if let Ok(stored) = raw.clone().try_into::<StoredSettings>() {
        stored.equalizer.validate()?;
        return Ok((stored, Vec::new()));
    }
    let fields = raw.as_table().ok_or("settings must be a table")?;
    let mut accepted =
        toml::Value::try_from(StoredSettings::default()).map_err(|error| error.to_string())?;
    let mut unsupported = Vec::new();
    for (key, value) in fields {
        recover_setting(&mut accepted, vec![key.clone()], value, &mut unsupported);
    }
    accepted
        .try_into()
        .map(|stored| (stored, unsupported))
        .map_err(|error| error.to_string())
}

fn read_startup_settings(path: &Path) -> Result<StoredSettings, String> {
    let Some(raw) = read_settings_file(path)? else {
        return Ok(StoredSettings::default());
    };
    let (stored, unsupported) = decode_settings(&raw)?;
    if !unsupported.is_empty() {
        warn!(?unsupported, path = %path.display(),
            "using defaults for incompatible preferences; saved values remain intact");
    }
    Ok(stored)
}

// Apply typed changes without dropping fields this version does not understand.
// Arrays are whole preferences: replacing their contents is an explicit change.
fn merge_settings(saved: &mut toml::Value, previous: &toml::Value, next: &toml::Value) {
    if previous == next {
        return;
    }
    if let (Some(saved), Some(previous), Some(next)) =
        (saved.as_table_mut(), previous.as_table(), next.as_table())
    {
        for key in previous.keys() {
            if !next.contains_key(key) {
                saved.remove(key);
            }
        }
        for (key, value) in next {
            match (saved.get_mut(key), previous.get(key)) {
                (Some(saved), Some(previous)) => merge_settings(saved, previous, value),
                _ => {
                    saved.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *saved = next.clone();
    }
}

pub(crate) fn write_settings(path: &Path, value: &StoredSettings) -> Result<(), String> {
    let next = toml::Value::try_from(value).map_err(|error| error.to_string())?;
    let merged = match read_settings_file(path)? {
        None => next,
        Some(mut saved) => {
            let (mut previous, unsupported) = decode_settings(&saved)?;
            let mut before = toml::Value::try_from(&previous).map_err(|error| error.to_string())?;
            if !unsupported.is_empty() {
                previous.migrate_defaults();
                let defaults =
                    toml::Value::try_from(&previous).map_err(|error| error.to_string())?;
                // Sanitizing a runtime default must not overwrite an unsupported saved value.
                for path in unsupported {
                    let default = path
                        .iter()
                        .try_fold(&defaults, |parent, key| parent.get(key));
                    replace_setting(&mut before, &path, default.cloned());
                }
            }
            merge_settings(&mut saved, &before, &next);
            saved
        }
    };
    write_private(path, &encode_settings(&merged)?)
}

pub(crate) fn encode_settings(value: &toml::Value) -> Result<Vec<u8>, String> {
    toml::to_string_pretty(value)
        .map(String::into_bytes)
        .map_err(|error| error.to_string())
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    if let Err(error) = sync_settings_directory(parent) {
        warn!(%error,path=%path.display(),"could not sync the settings directory after saving");
    }
    Ok(())
}
fn sync_settings_directory(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent).and_then(|directory| directory.sync_all())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn former_pins_drop_their_server_and_keep_their_order() {
        let mut saved = toml::Value::try_from(StoredSettings::default()).unwrap();
        saved.as_table_mut().unwrap().insert(
            "sidebar".into(),
            toml::Value::Table(toml::toml! {
                playlist_pin_imported_sources = ["first"]
                [[pins]]
                Genre = { source_id = "first", genre_id = "rock" }
                [[pins]]
                Playlist = { playlist_id = "mix" }
                [[pins]]
                Genre = { source_id = "second", genre_id = "rock" }
            }),
        );
        let (mut stored, unsupported) = decode_settings(&saved).unwrap();
        stored.migrate_defaults();
        assert!(unsupported.is_empty());
        assert!(stored.ui.sidebar.playlist_pins_imported);
        assert_eq!(
            stored.ui.sidebar.pins,
            [
                SidebarPin::Genre {
                    genre_id: "rock".into()
                },
                SidebarPin::Playlist {
                    playlist_id: "mix".into()
                },
            ]
        );
    }

    #[test]
    fn former_download_list_keeps_the_configured_server_entry() {
        let mut saved = toml::Value::try_from(StoredSettings::default()).unwrap();
        let source = |id: &str| {
            toml::Value::Table(toml::toml! {
                source_id = id
                kind = "navidrome"
                name = "Server"
                provider_payload = "{}"
            })
        };
        saved["sources"]
            .as_table_mut()
            .unwrap()
            .insert("configured".into(), source("current"));
        saved.as_table_mut().unwrap().insert(
            "downloads".into(),
            toml::toml! {
                [[list]]
                source_id = "forgotten"
                entire_library = true
                directory = "/old"
                [[list]]
                source_id = "current"
                favorites = true
                directory = "/music"
            }["list"]
                .clone(),
        );
        let (stored, unsupported) = decode_settings(&saved).unwrap();
        assert!(unsupported.is_empty());
        assert!(stored.ui.downloads.rules.favorites);
        assert!(!stored.ui.downloads.rules.entire_library);
        assert_eq!(
            stored.ui.downloads.directory.as_deref(),
            Some(std::path::Path::new("/music"))
        );

        saved["sources"]
            .as_table_mut()
            .unwrap()
            .insert("configured".into(), source("other"));
        let (stored, _) = decode_settings(&saved).unwrap();
        assert!(stored.ui.downloads.is_default());
    }

    #[test]
    fn removed_history_entries_keep_the_rest_of_their_lists() {
        let mut saved = toml::Value::try_from(StoredSettings::default()).unwrap();
        let routes = saved["sidebar"]["route_items"].as_array_mut().unwrap();
        routes.retain(|item| item["item"].as_str() != Some("Moods"));
        routes.push(
            toml::toml! { item = "History"
            visible = true }
            .into(),
        );
        let lists = saved["library_lists"].as_array_mut().unwrap();
        let mut history = lists[0].clone();
        history["key"] = "History".into();
        lists.push(history);
        let (stored, unsupported) = decode_settings(&saved).unwrap();
        assert!(unsupported.is_empty());
        assert!(
            stored
                .ui
                .sidebar
                .route_items
                .iter()
                .all(|entry| entry.item != SidebarRouteItem::Moods)
        );
    }

    #[test]
    fn pending_settings_update_leaves_committed_values_readable_and_serializes_saves() {
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory()
        };
        let initial = file.load();
        let (entered, pending) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer_file = file.clone();
        let writer = std::thread::spawn(move || {
            writer_file.update(|stored| {
                stored.ui.shuffle_enabled = true;
                entered.send(()).unwrap();
                resume.recv().unwrap();
                Ok(())
            })
        });
        pending.recv().unwrap();
        let (read, result) = mpsc::channel();
        let reader_file = file.clone();
        let reader = std::thread::spawn(move || read.send(reader_file.load()).unwrap());
        let observed = result.recv_timeout(Duration::from_secs(5));
        let next_file = file.clone();
        let next_writer = std::thread::spawn(move || {
            next_file.update(|stored| {
                let previous_shuffle = stored.ui.shuffle_enabled;
                stored.ui.application.download_artwork_automatically = true;
                Ok(previous_shuffle)
            })
        });
        // Editing preferences must not wait for a background save to finish.
        file.edit(|stored| {
            stored.ui.window_width = Some(1234);
            stored.ui.window_height = Some(800);
            Ok(())
        })
        .unwrap();
        let edited_width = file.load().ui.window_width;
        // Always release the writer, including when a blocked reader times out.
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        reader.join().unwrap();
        assert!(next_writer.join().unwrap().unwrap());
        assert_eq!(observed.unwrap(), initial);
        assert_eq!(edited_width, Some(1234));
        let committed = file.load();
        assert!(committed.ui.shuffle_enabled);
        assert!(committed.ui.application.download_artwork_automatically);
        assert_eq!(committed.ui.window_width, Some(1234));
        file.flush().unwrap();
        assert_eq!(read_startup_settings(&path).unwrap(), file.load());
    }

    #[test]
    fn failed_settings_save_does_not_publish_and_later_saves_succeed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        // A directory at the target path makes the atomic replacement fail.
        fs::create_dir(&path).unwrap();
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory()
        };
        let initial = file.load();
        let changes = file.sidebar_changes();
        assert!(
            file.update(|stored| {
                stored.ui.sidebar.pins_visible = !stored.ui.sidebar.pins_visible;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(file.load(), initial);
        assert!(!changes.has_changed().unwrap());

        fs::remove_dir(&path).unwrap();
        file.update(|stored| {
            stored.ui.sidebar.pins_visible = !stored.ui.sidebar.pins_visible;
            Ok(())
        })
        .unwrap();
        let committed = file.load();
        assert_ne!(committed.ui.sidebar, initial.ui.sidebar);
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow(), committed.ui.sidebar);
        assert_eq!(read_startup_settings(&path).unwrap(), committed);
    }

    #[test]
    fn settings_file_is_private_and_replaced_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory()
        };
        file.update(|stored| {
            stored.ui.application.reduce_motion = true;
            Ok(())
        })
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        fs::write(&path, b"old").unwrap();
        write_private(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn scrobbling_secrets_are_kept_apart_from_scrobbling_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.toml");
        let file = SettingsFile {
            path: Some(path.clone()),
            ..SettingsFile::memory()
        };
        let mut settings = load_scrobbling_settings(&file);
        settings.listenbrainz.user_token = "token".to_string();

        let committed =
            persist_scrobbling_settings(&file, &settings).expect("persist scrobbling secret");
        file.flush().unwrap();

        assert_eq!(committed.listenbrainz.user_token, "token");
        let saved = read_startup_settings(&path).unwrap();
        assert_eq!(saved.scrobbling_credentials["listenbrainz-token"], "token");
        assert!(saved.scrobbling.listenbrainz.user_token.is_empty());
        assert_eq!(read_scrobbling(&saved).listenbrainz.user_token, "token");
    }

    #[test]
    fn encrypted_credentials_from_older_settings_are_dropped() {
        let raw: toml::Value = toml::from_str(
            r#"
            [scrobbling_credentials.listenbrainz-token]
            encrypted = "age"
            "#,
        )
        .unwrap();
        let (stored, unsupported) = decode_settings(&raw).unwrap();
        assert!(stored.scrobbling_credentials.is_empty());
        assert!(!unsupported.is_empty());
    }
}
