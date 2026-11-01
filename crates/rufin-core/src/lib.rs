//! Rufin application composition and concrete product operations.
pub mod app;
pub mod browse;
pub mod detail_links;
pub mod diagnostics;
pub mod favorites;
mod loudness;
pub mod paths;
pub mod playback;
pub mod radio;
pub mod route;
pub mod runtime;
pub mod scrobbling;
pub mod settings;
pub mod source;
pub mod text;
pub mod themes;
pub use settings::SettingsHandle;

pub mod metadata;
pub mod playlists;

pub use playback::equalizer::{
    EqEdit, EqPresetFormat, EqPresetSummary, EqSubscription, EqView, EqualizerPreferences,
};
