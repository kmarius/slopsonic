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
