use async_channel::Receiver;
use library::ReadCancellation;

use super::SourceOwner;
use crate::SettingsHandle;

impl SourceOwner {
    pub fn import_playlist_pins_once(
        &self,
        settings: SettingsHandle,
    ) -> Receiver<Result<bool, String>> {
        let pending =
            self.configuration().is_some() && !settings.load().sidebar.playlist_pins_imported;
        self.reply(move |_, database| async move {
            if !pending {
                return Ok(false);
            }
            let Some(source) = database
                .source_identity_key()
                .await
                .map_err(|error| error.to_string())?
            else {
                return Ok(false);
            };
            let ids = database
                .source_playlist_object_ids(source, &ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())?;
            if ids.is_empty() {
                return Ok(false);
            }
            settings.import_playlist_pins_once(ids)
        })
    }
}
