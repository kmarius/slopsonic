use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SidebarPanel {
    Queue,
    Visualizer,
}

impl SidebarPanel {
    pub const ALL: [Self; 2] = [Self::Queue, Self::Visualizer];

    pub fn index(self) -> usize {
        match self {
            Self::Queue => 0,
            Self::Visualizer => 1,
        }
    }
}

/// Panels that no longer exist are dropped instead of failing to load the settings.
fn known_panels<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<SidebarPanel>, D::Error> {
    Ok(Vec::<String>::deserialize(deserializer)?
        .iter()
        .filter_map(|name| match name.as_str() {
            "queue" => Some(SidebarPanel::Queue),
            "visualizer" => Some(SidebarPanel::Visualizer),
            _ => None,
        })
        .collect())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RightPanelSettings {
    #[serde(deserialize_with = "known_panels")]
    pub order: Vec<SidebarPanel>,
    pub queue_visible: bool,
    pub visualizer_height: Option<i32>,
}

impl Default for RightPanelSettings {
    fn default() -> Self {
        Self {
            order: SidebarPanel::ALL.to_vec(),
            queue_visible: true,
            visualizer_height: None,
        }
    }
}

impl RightPanelSettings {
    pub fn sanitize(&mut self) {
        let mut order = Vec::with_capacity(SidebarPanel::ALL.len());
        for panel in self.order.iter().copied().chain(SidebarPanel::ALL) {
            if !order.contains(&panel) {
                order.push(panel);
            }
        }
        self.order = order;
    }

    pub fn visible_panels(&self, visualizer: bool) -> Vec<SidebarPanel> {
        self.order
            .iter()
            .copied()
            .filter(|panel| match panel {
                SidebarPanel::Queue => self.queue_visible,
                SidebarPanel::Visualizer => visualizer,
            })
            .collect()
    }

    pub fn move_panel(&mut self, panel: SidebarPanel, direction: isize, visualizer: bool) -> bool {
        let visible = self.visible_panels(visualizer);
        let Some(index) = visible.iter().position(|candidate| *candidate == panel) else {
            return false;
        };
        let Some(neighbor) = index
            .checked_add_signed(direction)
            .and_then(|index| visible.get(index))
        else {
            return false;
        };
        let from = self
            .order
            .iter()
            .position(|candidate| *candidate == panel)
            .unwrap();
        let to = self
            .order
            .iter()
            .position(|candidate| candidate == neighbor)
            .unwrap();
        self.order.swap(from, to);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_visibility_defaults_on_and_preserves_panel_order() {
        use SidebarPanel::*;
        let mut settings: RightPanelSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.visible_panels(false), [Queue]);
        settings.queue_visible = false;
        assert!(settings.visible_panels(false).is_empty());
        assert_eq!(settings.visible_panels(true), [Visualizer]);
        settings.queue_visible = true;
        assert!(settings.move_panel(Visualizer, -1, true));
        let restored: RightPanelSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
        assert_eq!(settings.visible_panels(true), [Visualizer, Queue]);
    }

    #[test]
    fn moving_skips_hidden_panels_without_changing_their_sizes() {
        use SidebarPanel::*;
        let mut settings = RightPanelSettings {
            visualizer_height: Some(170),
            ..Default::default()
        };
        assert!(!settings.move_panel(Visualizer, -1, false));
        assert!(settings.move_panel(Visualizer, -1, true));
        assert_eq!(settings.visible_panels(false), [Queue]);
        assert_eq!(settings.visible_panels(true), [Visualizer, Queue]);
        assert_eq!(settings.visualizer_height, Some(170));
        let restored: RightPanelSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
    }

    #[test]
    fn stored_panels_that_no_longer_exist_are_dropped() {
        let mut settings: RightPanelSettings =
            serde_json::from_str(r#"{"order":["removed","visualizer"],"combined":true}"#).unwrap();
        settings.sanitize();
        assert_eq!(settings.order, [SidebarPanel::Visualizer, SidebarPanel::Queue]);
    }
}
