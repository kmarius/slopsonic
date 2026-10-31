//! Checks media availability for cleanup and playback failure recovery.
use super::{SourceOwner, string_error};
use std::collections::HashSet;

impl SourceOwner {
    pub(crate) async fn missing_media(&self, uris: &[String]) -> Result<HashSet<String>, String> {
        let database = &self.shared.database;
        let source_uris: Vec<_> = uris
            .iter()
            .filter_map(|uri| {
                library::source_entity_parts(uri)
                    .filter(|(_, kind, _)| kind == "track")
                    .map(|(source, _, _)| (uri.clone(), source))
            })
            .collect();
        Ok(database
            .catalog_missing_media(&source_uris)
            .await
            .map_err(string_error)?
            .into_iter()
            .collect())
    }
}
