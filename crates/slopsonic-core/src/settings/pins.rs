use super::SettingsOwner;

impl SettingsOwner {
    pub(crate) fn import_playlist_pins_once(&self, ids: Vec<String>) -> Result<bool, String> {
        if self.load().sidebar.playlist_pins_imported {
            return Ok(false);
        }
        let mut changed = false;
        self.restore(
            |stored| {
                changed = stored.ui.sidebar.import_playlist_pins_once(ids);
                Ok(())
            },
            false,
        )?;
        Ok(changed)
    }
}
