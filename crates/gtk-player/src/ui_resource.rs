pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    PLAYBACK_SETTINGS_POPOVER_RESOURCE,
    TRANSITION_SETTINGS_RESOURCE,
    RIGHT_PANEL_RESOURCE,
    QUEUE_SETTINGS_RESOURCE,
    FULLSCREEN_PLAYER_RESOURCE,
    FULLSCREEN_SETTINGS_RESOURCE,
    BOTTOM_PLAYER_RESOURCE,
];
pub const BOTTOM_PLAYER_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/bottom.ui";

pub const FULLSCREEN_PLAYER_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/fullscreen.ui";
pub const FULLSCREEN_SETTINGS_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/fullscreen_settings.ui";

pub const RIGHT_PANEL_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/right_panel.ui";
pub const QUEUE_SETTINGS_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/queue_settings.ui";

pub const TRANSITION_SETTINGS_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/transition_settings.ui";

pub const PLAYBACK_SETTINGS_POPOVER_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/playback_settings.ui";

#[cfg(test)]
mod resource_tests {
    #[test]
    fn owned_interface_resources_are_compiled() {
        crate::register_resources().expect("resource registration");
        for path in super::INTERFACE_RESOURCE_PATHS {
            gtk::gio::resources_lookup_data(path, gtk::gio::ResourceLookupFlags::NONE)
                .expect("compiled interface resource");
        }
    }
}
