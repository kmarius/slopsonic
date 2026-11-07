use super::SettingsOwner;
use playback::{
    LoudnessNormalization, LoudnessNormalizationScope, PlaybackTransitionMode, StreamQuality,
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
