//! Preferences, source setup and desktop appearance presentation.
pub mod appearance;
mod download_quality;
pub mod preferences;
pub mod ui_resource;

use slopsonic_core::settings::*;
use std::rc::Rc;

pub struct Preferences {
    pub settings: Rc<gtk_widgets::settings::SettingsState>,
    pub source: Rc<preferences::source::SourceState>,
    pub state: preferences::PreferencesState,
    pub products: slopsonic_core::runtime::ProductHandles,
    pub player_ui: Rc<gtk_player::PlayerUi>,
    pub downloads: Rc<gtk_widgets::downloads::DownloadsState>,
    pub artwork: Rc<gtk_widgets::artwork::ArtworkState>,
    pub appearance: Rc<appearance::ApplicationAppearance>,
    pub control_feedback: Rc<gtk_widgets::feedback::ControlFeedbackState>,
    pub window: gtk::ApplicationWindow,
    pub effects: Box<dyn Fn(Effect)>,
}

/// Work owned by the mounted desktop window after a preference changes.
pub enum Effect {
    LayoutChanged,
    SidebarChanged,
    CatalogChanged,
    RefreshHome,
}

pub fn register_resources() -> Result<(), String> {
    gtk_widgets::register_resources()?;
    gtk_player::register_resources()?;
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("gtk-preferences.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}
