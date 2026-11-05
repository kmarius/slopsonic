use super::SettingsOwner;
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, PlaybackSettings, PlaybackTransitionMode,
    StreamQuality, VolumeScale,
};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{reason}")]
    Message { reason: String },
}

impl From<String> for SettingsError {
    fn from(message: String) -> Self {
        Self::Message { reason: message }
    }
}

#[derive(Clone, Debug)]
pub struct PlayerPreferences {
    pub application: super::ApplicationPreferences,
    pub playback: PlaybackSettings,
    pub display: super::PlayerDisplayPreferences,
    pub right_panel: super::right_panel::RightPanelSettings,
}

pub struct PlayerPreferenceOptions {
    pub min_crossfade_seconds: u8,
    pub max_crossfade_seconds: u8,
    pub min_auto_dj_refill_threshold: u8,
    pub max_auto_dj_refill_threshold: u8,
}

impl SettingsOwner {
    pub fn player_preferences(&self) -> PlayerPreferences {
        let stored = self
            .file
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ui = &stored.ui;
        let playback = ui.playback.clone();
        PlayerPreferences {
            application: ui.application.clone(),
            playback,
            display: ui.player_display.clone(),
            right_panel: ui.right_panel.clone(),
        }
    }

    pub fn player_preference_options(&self) -> PlayerPreferenceOptions {
        PlayerPreferenceOptions {
            min_crossfade_seconds: playback::MIN_CROSSFADE_SECONDS,
            max_crossfade_seconds: playback::MAX_CROSSFADE_SECONDS,
            min_auto_dj_refill_threshold: playback::MIN_AUTO_DJ_REFILL_THRESHOLD,
            max_auto_dj_refill_threshold: playback::MAX_AUTO_DJ_REFILL_THRESHOLD,
        }
    }

    pub fn set_volume_scale(&self, value: VolumeScale) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            ui.playback.set_volume_scale_preserving_gain(value);
            Ok(())
        })
        .map_err(Into::into)
    }
}

macro_rules! preference_target {
    ($ui:ident ($($path:ident).+)) => { $ui.$($path).+ };
}

macro_rules! native_preference_setters {
    ([$ui:ident $path:tt $after:block]
        $( $(#[$attribute:meta])* $field:ident: $ty:ty = $default:expr $(=> $setter:ident)?; )*) => {
        impl SettingsOwner {
            $($(pub fn $setter(&self, value: $ty) -> Result<(), SettingsError> {
                self.update_preferences(move |$ui| {
                    (preference_target!($ui $path)).$field = value;
                    $after;
                    Ok(())
                }).map_err(Into::into)
            })?)*
        }
    };
    ([$($args:tt)*] direct { $($direct:tt)* } migrated { $($migrated:tt)* }) => {
        native_preference_setters! { [$($args)*] $($direct)* $($migrated)* }
    };
}

playback::playback_preference_fields!(native_preference_setters, ui, (playback), {});
