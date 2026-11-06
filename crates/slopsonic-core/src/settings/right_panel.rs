use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RightPanelSettings {
    pub queue_visible: bool,
}

impl Default for RightPanelSettings {
    fn default() -> Self {
        Self {
            queue_visible: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_visibility_defaults_on_and_round_trips() {
        let mut settings: RightPanelSettings = toml::from_str("").unwrap();
        assert!(settings.queue_visible);
        settings.queue_visible = false;
        let restored: RightPanelSettings =
            toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
    }
}
