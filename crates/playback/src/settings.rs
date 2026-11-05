use serde::{Deserialize, Serialize};

pub const MIN_CROSSFADE_SECONDS: u8 = 1;
pub const MAX_CROSSFADE_SECONDS: u8 = 30;
pub const DEFAULT_AUTO_DJ_REFILL_THRESHOLD: u8 = 1;
pub const MIN_AUTO_DJ_REFILL_THRESHOLD: u8 = 1;
pub const MAX_AUTO_DJ_REFILL_THRESHOLD: u8 = 10;
const PERCEPTUAL_DB_RANGE: f64 = 50.0;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum StreamQuality {
    #[default]
    Original,
    MaxBitrateKbps(u32),
}

impl StreamQuality {
    pub const fn max_bitrate_kbps(self) -> Option<u32> {
        match self {
            Self::Original => None,
            Self::MaxBitrateKbps(kbps) => Some(kbps),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PlaybackTransitionMode {
    #[default]
    Gapless,
    Crossfade,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LoudnessNormalization {
    Off,
    /// Stored settings from versions with local EBU R128 analysis map here.
    #[default]
    #[serde(alias = "EbuR128")]
    ReplayGain,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LoudnessNormalizationScope {
    Track,
    #[default]
    Album,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum VolumeScale {
    #[default]
    Perceptual,
    Linear,
}

impl VolumeScale {
    pub fn gain(self, position: f64) -> f64 {
        let position = sanitize_volume(position);
        match self {
            Self::Perceptual if position == 0.0 => 0.0,
            Self::Perceptual => 10_f64.powf(PERCEPTUAL_DB_RANGE * (position - 1.0) / 20.0),
            Self::Linear => position,
        }
    }

    pub fn position_for_gain(self, gain: f64) -> f64 {
        let gain = sanitize_volume(gain);
        match self {
            Self::Perceptual if gain <= perceptual_gain_floor() => 0.0,
            Self::Perceptual => sanitize_volume(1.0 + 20.0 * gain.log10() / PERCEPTUAL_DB_RANGE),
            Self::Linear => gain,
        }
    }
}

fn perceptual_gain_floor() -> f64 {
    10_f64.powf(-PERCEPTUAL_DB_RANGE / 20.0)
}

#[macro_export]
macro_rules! playback_preference_fields {
    ($callback:ident $(, $args:tt)*) => {
        $callback! { [$($args)*]
            direct {
                #[serde(default)]
                transition_mode: PlaybackTransitionMode = PlaybackTransitionMode::Gapless => set_transition_mode;
                #[serde(default = "default_crossfade_seconds")]
                crossfade_seconds: u8 = default_crossfade_seconds() => set_crossfade_seconds;
                #[serde(default)]
                skip_same_album_crossfade: bool = false => set_skip_same_album_crossfade;
                #[serde(default = "default_true")]
                audio_fade_on_status_change: bool = true => set_audio_fade_on_status_change;
                #[serde(default = "default_true")]
                skip_unplayable_tracks: bool = true => set_skip_unplayable_tracks;
                #[serde(default)]
                stream_quality: StreamQuality = StreamQuality::Original => set_stream_quality;
                #[serde(default)]
                muted: bool = false;
            }
            migrated {
                loudness_normalization: LoudnessNormalization = LoudnessNormalization::ReplayGain => set_loudness_normalization;
                loudness_normalization_scope: LoudnessNormalizationScope = LoudnessNormalizationScope::Album => set_loudness_normalization_scope;
                volume: f64 = default_volume();
                volume_scale: VolumeScale = VolumeScale::Perceptual;
            }
        }
    };
}

macro_rules! define_playback_preferences {
    ([] direct { $( $(#[$attribute:meta])* $field:ident: $ty:ty = $default:expr $(=> $setter:ident)?; )* }
        migrated { $( $legacy:ident: $legacy_ty:ty = $legacy_default:expr $(=> $legacy_setter:ident)?; )* }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct PlaybackSettings {
            $($(#[$attribute])* pub $field: $ty,)*
            $(pub $legacy: $legacy_ty,)*
        }

        impl Default for PlaybackSettings {
            fn default() -> Self {
                Self { $($field: $default,)* $($legacy: $legacy_default,)* }
            }
        }
    };
}
playback_preference_fields!(define_playback_preferences);

impl PlaybackSettings {
    pub fn set_volume_scale_preserving_gain(&mut self, volume_scale: VolumeScale) {
        if self.volume_scale == volume_scale {
            return;
        }
        let gain = self.volume_scale.gain(self.volume);
        self.volume = volume_scale.position_for_gain(gain);
        self.volume_scale = volume_scale;
    }

    pub fn sanitize(&mut self) {
        self.crossfade_seconds = self
            .crossfade_seconds
            .clamp(MIN_CROSSFADE_SECONDS, MAX_CROSSFADE_SECONDS);
        self.volume = sanitize_volume(self.volume);
    }
}

fn default_true() -> bool {
    true
}

fn default_volume() -> f64 {
    1.0
}

fn sanitize_volume(volume: f64) -> f64 {
    if volume.is_finite() {
        volume.clamp(0.0, 1.0)
    } else {
        default_volume()
    }
}

fn default_crossfade_seconds() -> u8 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_settings_clamp_crossfade_range() {
        let mut settings = PlaybackSettings {
            crossfade_seconds: 0,
            ..PlaybackSettings::default()
        };
        settings.sanitize();
        assert_eq!(settings.crossfade_seconds, MIN_CROSSFADE_SECONDS);

        settings.crossfade_seconds = MAX_CROSSFADE_SECONDS + 1;
        settings.sanitize();
        assert_eq!(settings.crossfade_seconds, MAX_CROSSFADE_SECONDS);
    }

    #[test]
    fn replay_gain_is_the_default_without_enabling_analysis() {
        assert_eq!(
            LoudnessNormalization::default(),
            LoudnessNormalization::ReplayGain
        );
        assert_eq!(
            PlaybackSettings::default().loudness_normalization,
            LoudnessNormalization::ReplayGain
        );

        let restored = serde_json::from_str::<PlaybackSettings>("{}")
            .expect("restore playback settings without loudness normalization");
        assert_eq!(
            restored.loudness_normalization,
            LoudnessNormalization::ReplayGain
        );
        assert_eq!(
            restored.loudness_normalization_scope,
            LoudnessNormalizationScope::Album
        );
    }

    #[test]
    fn stored_ebu_mode_restores_as_replay_gain() {
        let restored = serde_json::from_str::<PlaybackSettings>(
            r#"{"loudness_normalization":"EbuR128","ebu_r128_target_lufs":-21.0}"#,
        )
        .expect("restore settings from a version with EBU R128 analysis");
        assert_eq!(
            restored.loudness_normalization,
            LoudnessNormalization::ReplayGain
        );
    }

    #[test]
    fn perceptual_volume_uses_half_decibel_steps() {
        assert_eq!(VolumeScale::Perceptual.gain(0.0), 0.0);
        assert_eq!(VolumeScale::Perceptual.gain(1.0), 1.0);

        let mut previous_db = 20.0 * VolumeScale::Perceptual.gain(0.01).log10();
        assert!((previous_db + 49.5).abs() < 1e-12);
        for percent in 2..=100 {
            let gain = VolumeScale::Perceptual.gain(f64::from(percent) / 100.0);
            let db = 20.0 * gain.log10();
            assert!((db - previous_db - 0.5).abs() < 1e-12);
            previous_db = db;
        }

        for position in [0.01, 0.1, 0.5, 1.0] {
            let gain = VolumeScale::Perceptual.gain(position);
            assert!((VolumeScale::Perceptual.position_for_gain(gain) - position).abs() < 1e-12);
        }
        assert_eq!(VolumeScale::Perceptual.position_for_gain(0.0), 0.0);
    }

    #[test]
    fn explicit_perceptual_volume_round_trips_as_the_same_position() {
        let settings = PlaybackSettings {
            volume: 0.5,
            volume_scale: VolumeScale::Perceptual,
            ..PlaybackSettings::default()
        };
        let restored = serde_json::from_value::<PlaybackSettings>(
            serde_json::to_value(settings).expect("serialize perceptual volume"),
        )
        .expect("restore perceptual volume");

        assert_eq!(restored.volume_scale, VolumeScale::Perceptual);
        assert_eq!(restored.volume, 0.5);
        assert!(
            (restored.volume_scale.gain(restored.volume) - 0.056_234_132_519_034_91).abs() < 1e-12
        );
    }

    #[test]
    fn explicit_linear_volume_round_trips_without_migration() {
        let settings = PlaybackSettings {
            volume: 0.5,
            volume_scale: VolumeScale::Linear,
            ..PlaybackSettings::default()
        };
        let restored = serde_json::from_value::<PlaybackSettings>(
            serde_json::to_value(settings).expect("serialize linear volume"),
        )
        .expect("restore linear volume");

        assert_eq!(restored.volume_scale, VolumeScale::Linear);
        assert_eq!(restored.volume, 0.5);
        assert_eq!(restored.volume_scale.gain(restored.volume), 0.5);
    }

    #[test]
    fn changing_volume_scale_preserves_output_gain() {
        let mut settings = PlaybackSettings {
            volume: 0.5,
            volume_scale: VolumeScale::Linear,
            ..PlaybackSettings::default()
        };

        settings.set_volume_scale_preserving_gain(VolumeScale::Perceptual);
        assert!((settings.volume_scale.gain(settings.volume) - 0.5).abs() < 1e-12);

        settings.set_volume_scale_preserving_gain(VolumeScale::Linear);
        assert!((settings.volume - 0.5).abs() < 1e-12);
        assert!((settings.volume_scale.gain(settings.volume) - 0.5).abs() < 1e-12);
    }
}
