use super::{SettingsError, SettingsOwner, app};

#[cfg_attr(feature = "bindings", uniffi::export)]
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

    pub fn set_backup_preferences(
        &self,
        mut value: crate::runtime::BackupSettings,
    ) -> Result<(), SettingsError> {
        self.update_preferences(move |ui| {
            value.schedule.schedule_id = ui.application.backup.schedule.schedule_id.clone();
            value.schedule.last_successful_at = ui.application.backup.schedule.last_successful_at;
            ui.application.backup = value;
            Ok(())
        })
        .map_err(Into::into)
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
