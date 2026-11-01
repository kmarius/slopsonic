use super::{SettingsError, SettingsOwner, app};

impl SettingsOwner {
    pub fn application_preferences(&self) -> app::ApplicationPreferences {
        self.file
            .value
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ui
            .application
            .clone()
    }

    pub fn random_preferences(&self) -> super::RandomPlaySettings {
        self.load().random_play
    }

    pub fn set_random_preferences(
        &self,
        value: super::RandomPlaySettings,
    ) -> Result<super::RandomPlaySettings, SettingsError> {
        self.update_preferences(move |ui| {
            ui.random_play = value;
            Ok(())
        })?;
        Ok(self.random_preferences())
    }
}
