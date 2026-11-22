use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::default_true;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SidebarRouteItem {
    Home,
    Search,
    Favorites,
    Albums,
    Tracks,
    Artists,
    AlbumArtists,
    Genres,
    Moods,
    Folders,
    Playlists,
}
impl SidebarRouteItem {
    pub fn from_stable_id(stable_id: &str) -> Option<Self> {
        toml::Value::String(stable_id.to_owned()).try_into().ok()
    }

    pub fn all() -> [Self; 11] {
        [
            Self::Home,
            Self::Search,
            Self::Favorites,
            Self::Albums,
            Self::Tracks,
            Self::Artists,
            Self::AlbumArtists,
            Self::Genres,
            Self::Moods,
            Self::Folders,
            Self::Playlists,
        ]
    }

    fn default_visible(self) -> bool {
        !matches!(self, Self::Search | Self::Moods)
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SidebarRouteItemSettings {
    pub item: SidebarRouteItem,
    pub visible: bool,
}
pub use library::SidebarPin;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SidebarSettings {
    #[serde(default = "default_sidebar_route_items")]
    pub route_items: Vec<SidebarRouteItemSettings>,
    #[serde(default = "default_true")]
    pub pins_visible: bool,
    #[serde(default)]
    pub pins: Vec<SidebarPin>,
    #[serde(default)]
    pub playlist_pins_imported: bool,
}
impl Default for SidebarSettings {
    fn default() -> Self {
        Self {
            route_items: default_sidebar_route_items(),
            pins_visible: true,
            pins: Vec::new(),
            playlist_pins_imported: false,
        }
    }
}
impl SidebarSettings {
    pub fn set_route_visible(&mut self, item: SidebarRouteItem, visible: bool) -> bool {
        let Some(stored) = self
            .route_items
            .iter_mut()
            .find(|stored| stored.item == item)
        else {
            return false;
        };
        if stored.visible == visible {
            return false;
        }
        stored.visible = visible;
        self.sanitize();
        true
    }

    pub fn move_route(&mut self, item: SidebarRouteItem, delta: isize) -> bool {
        let Some(index) = self.route_items.iter().position(|entry| entry.item == item) else {
            return false;
        };
        let new_index = if delta < 0 {
            index.saturating_sub(1)
        } else {
            (index + 1).min(self.route_items.len().saturating_sub(1))
        };
        if index == new_index {
            return false;
        }
        self.route_items.swap(index, new_index);
        self.sanitize();
        true
    }

    pub fn reorder_route(
        &mut self,
        source: SidebarRouteItem,
        target: SidebarRouteItem,
        after: bool,
    ) -> bool {
        if source == target {
            return false;
        }
        let before = self.route_items.clone();
        let Some(source_index) = self
            .route_items
            .iter()
            .position(|entry| entry.item == source)
        else {
            return false;
        };
        let entry = self.route_items.remove(source_index);
        let Some(mut target_index) = self
            .route_items
            .iter()
            .position(|entry| entry.item == target)
        else {
            self.route_items
                .insert(source_index.min(self.route_items.len()), entry);
            return false;
        };
        if after {
            target_index += 1;
        }
        self.route_items
            .insert(target_index.min(self.route_items.len()), entry);
        let changed = self.route_items != before;
        if changed {
            self.sanitize();
        }
        changed
    }

    pub fn sanitize(&mut self) {
        let mut sanitized = Vec::with_capacity(SidebarRouteItem::all().len());
        for entry in &self.route_items {
            if !SidebarRouteItem::all().contains(&entry.item)
                || sanitized
                    .iter()
                    .any(|existing: &SidebarRouteItemSettings| existing.item == entry.item)
            {
                continue;
            }
            sanitized.push(entry.clone());
        }
        for item in SidebarRouteItem::all() {
            if !sanitized.iter().any(|entry| entry.item == item) {
                insert_sidebar_route_item_in_default_order(
                    &mut sanitized,
                    SidebarRouteItemSettings {
                        item,
                        visible: item.default_visible(),
                    },
                );
            }
        }
        if !sanitized.iter().any(|entry| entry.visible)
            && let Some(home) = sanitized
                .iter_mut()
                .find(|entry| entry.item == SidebarRouteItem::Home)
        {
            home.visible = true;
        }
        self.route_items = sanitized;
        let mut seen = HashSet::new();
        self.pins.retain(|pin| seen.insert(pin.clone()));
    }

    pub fn is_pinned(&self, pin: &SidebarPin) -> bool {
        self.pins.contains(pin)
    }

    pub fn set_pinned(&mut self, pin: SidebarPin, pinned: bool) -> bool {
        if pinned {
            if self.pins.contains(&pin) {
                return false;
            }
            self.pins.push(pin);
            return true;
        }
        let previous_len = self.pins.len();
        self.pins.retain(|stored| stored != &pin);
        self.pins.len() != previous_len
    }

    pub fn reorder_pin(&mut self, moved: &SidebarPin, target: &SidebarPin, after: bool) -> bool {
        if moved == target {
            return false;
        }
        let Some(moved_index) = self.pins.iter().position(|pin| pin == moved) else {
            return false;
        };
        let mut reordered = self.pins.clone();
        let moved = reordered.remove(moved_index);
        let Some(target_index) = reordered.iter().position(|pin| pin == target) else {
            return false;
        };
        reordered.insert(target_index + usize::from(after), moved);
        if reordered == self.pins {
            return false;
        }
        self.pins = reordered;
        true
    }

    pub fn pin_drop_after(&self, moved: &SidebarPin, target: &SidebarPin) -> Option<bool> {
        let moved_index = self.pins.iter().position(|pin| pin == moved)?;
        let target_index = self.pins.iter().position(|pin| pin == target)?;
        (moved_index != target_index).then_some(moved_index < target_index)
    }
    pub fn import_playlist_pins_once(
        &mut self,
        playlist_ids: impl IntoIterator<Item = String>,
    ) -> bool {
        if self.playlist_pins_imported {
            return false;
        }
        for playlist_id in playlist_ids {
            self.set_pinned(SidebarPin::Playlist { playlist_id }, true);
        }
        self.playlist_pins_imported = true;
        true
    }
}
fn insert_sidebar_route_item_in_default_order(
    items: &mut Vec<SidebarRouteItemSettings>,
    entry: SidebarRouteItemSettings,
) {
    let default_order = SidebarRouteItem::all();
    let Some(entry_index) = default_order.iter().position(|item| *item == entry.item) else {
        items.push(entry);
        return;
    };
    let insert_index = items
        .iter()
        .position(|existing| {
            default_order
                .iter()
                .position(|item| *item == existing.item)
                .is_some_and(|existing_index| existing_index > entry_index)
        })
        .unwrap_or(items.len());
    items.insert(insert_index, entry);
}
fn default_sidebar_route_items() -> Vec<SidebarRouteItemSettings> {
    SidebarRouteItem::all()
        .into_iter()
        .map(|item| SidebarRouteItemSettings {
            item,
            visible: item.default_visible(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_defaults_and_sanitization_preserve_saved_behavior() {
        let defaults = SidebarSettings::default();
        assert_eq!(
            defaults
                .route_items
                .iter()
                .map(|entry| entry.item)
                .collect::<Vec<_>>(),
            SidebarRouteItem::all()
        );
        assert_eq!(
            defaults
                .route_items
                .iter()
                .filter(|entry| !entry.visible)
                .map(|entry| entry.item)
                .collect::<Vec<_>>(),
            [SidebarRouteItem::Search, SidebarRouteItem::Moods]
        );

        let mut saved = defaults;
        saved.route_items.rotate_left(4);
        saved.route_items[0].visible = false;
        let expected = saved.route_items.clone();
        saved.sanitize();
        assert_eq!(saved.route_items, expected);
    }

    fn pin(id: &str) -> SidebarPin {
        SidebarPin::Playlist {
            playlist_id: id.to_string(),
        }
    }

    #[test]
    fn playlist_import_happens_once_and_preserves_unpinning() {
        let mut settings = SidebarSettings::default();
        assert!(settings.import_playlist_pins_once(["mix".into(), "other".into()]));
        let removed = settings.pins[0].clone();
        let retained = settings.pins[1].clone();
        settings.set_pinned(removed, false);
        let mut restored: SidebarSettings =
            toml::from_str(&toml::to_string(&settings).unwrap()).unwrap();
        assert!(!restored.import_playlist_pins_once(["mix".into()]));
        assert_eq!(restored.pins, [retained]);
    }

    #[test]
    fn reorder_pin_places_relative_to_the_target_and_ignores_no_ops() {
        let first = pin("first");
        let second = pin("second");
        let third = pin("third");
        let mut settings = SidebarSettings {
            pins: vec![first.clone(), second.clone(), third.clone()],
            ..SidebarSettings::default()
        };

        assert!(settings.reorder_pin(&first, &second, true));
        assert_eq!(
            settings.pins,
            [second.clone(), first.clone(), third.clone()]
        );
        assert!(settings.reorder_pin(&third, &second, false));
        assert_eq!(settings.pins, [third, second.clone(), first]);
        assert!(!settings.reorder_pin(&second, &second, false));
    }

    #[test]
    fn pin_drop_direction_follows_the_existing_order() {
        let first = pin("first");
        let second = pin("second");
        let third = pin("third");
        let settings = SidebarSettings {
            pins: vec![first.clone(), second.clone(), third.clone()],
            ..SidebarSettings::default()
        };

        assert_eq!(settings.pin_drop_after(&first, &second), Some(true));
        assert_eq!(settings.pin_drop_after(&third, &second), Some(false));
        assert_eq!(settings.pin_drop_after(&second, &second), None);
        assert_eq!(settings.pin_drop_after(&pin("missing"), &second), None);
    }
}
