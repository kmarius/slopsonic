pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    PLAYBACK_SETTINGS_POPOVER_RESOURCE,
    TRANSITION_SETTINGS_RESOURCE,
    RIGHT_PANEL_RESOURCE,
    QUEUE_SETTINGS_RESOURCE,
    EQUALIZER_RESOURCE,
    EQUALIZER_BAND_RESOURCE,
    EQUALIZER_BAND_ITEM_RESOURCE,
    EQUALIZER_FILTER_RESOURCE,
    EQUALIZER_DIALOGS_RESOURCE,
    FULLSCREEN_PLAYER_RESOURCE,
    FULLSCREEN_SETTINGS_RESOURCE,
    BOTTOM_PLAYER_RESOURCE,
];
pub const BOTTOM_PLAYER_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/bottom.ui";

pub const FULLSCREEN_PLAYER_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/fullscreen.ui";
pub const FULLSCREEN_SETTINGS_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/fullscreen_settings.ui";

pub const EQUALIZER_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/equalizer.ui";

pub const RIGHT_PANEL_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/right_panel.ui";
pub const QUEUE_SETTINGS_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/queue_settings.ui";

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

pub const EQUALIZER_BAND_RESOURCE: &str = "/io/github/kmarius/Slopsonic/ui/player/equalizer-band.ui";
pub const EQUALIZER_FILTER_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/equalizer-filter.ui";
pub const EQUALIZER_DIALOGS_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/equalizer-dialogs.ui";

pub const EQUALIZER_BAND_ITEM_RESOURCE: &str =
    "/io/github/kmarius/Slopsonic/ui/player/equalizer-band-item.ui";
