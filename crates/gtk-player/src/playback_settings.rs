pub const CROSSFADE_DURATION_TITLE: &str = "Crossfade duration";
use gtk_widgets::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};
use std::rc::Rc;

use adw::prelude::*;
use playback::{MAX_CROSSFADE_SECONDS, MIN_CROSSFADE_SECONDS, PlaybackTransitionMode};

use crate::PlayerUi;

const PLAYBACK_SETTINGS_SCALE_WIDTH: i32 = 180;

pub fn configure_playback_settings_popover(button: &gtk::MenuButton, shell: &Rc<PlayerUi>) {
    let shell = Rc::clone(shell);
    button.set_create_popup_func(move |button| {
        button.set_popover(Some(&playback_settings_popover(&shell)));
    });
}

fn playback_settings_popover(shell: &Rc<PlayerUi>) -> gtk::Popover {
    let settings = shell.settings.current.borrow().clone();
    let playback = settings.playback;

    let resource = crate::ui_resource::PLAYBACK_SETTINGS_POPOVER_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        popover: gtk::Popover,
        settings_group: adw::PreferencesGroup,
        queue: adw::SwitchRow,
    });
    let (transition_row, crossfade_options) =
        transition_rows(shell, &playback, PLAYBACK_SETTINGS_SCALE_WIDTH);
    settings_group.add(&transition_row);
    settings_group.add(&crossfade_options);

    queue.set_active(settings.right_panel.queue_visible);
    let queue_shell = Rc::downgrade(shell);
    queue.connect_active_notify(move |row| {
        if let Some(shell) = queue_shell.upgrade() {
            shell.set_sidebar_queue_visible(row.is_active());
        }
    });
    settings_group.add(&queue);

    popover
}

pub fn transition_rows(
    shell: &Rc<PlayerUi>,
    settings: &playback::PlaybackSettings,
    scale_width: i32,
) -> (adw::ActionRow, adw::PreferencesRow) {
    let resource = crate::ui_resource::TRANSITION_SETTINGS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    gtk_widgets::objects!(builder, resource, {
        mode: adw::ActionRow,
        crossfade: gtk::ToggleButton,
        options: adw::PreferencesRow,
        revealer: gtk::Revealer,
        rows: gtk::ListBox,
        skip_same_album: adw::SwitchRow,
    });
    rows.prepend(&crossfade_duration_row(
        shell,
        settings.crossfade_seconds,
        scale_width,
    ));
    let active = settings.transition_mode == PlaybackTransitionMode::Crossfade;
    crossfade.set_active(active);
    options.set_visible(active);
    revealer.set_reveal_child(active);
    skip_same_album.set_active(settings.skip_same_album_crossfade);
    let weak = Rc::downgrade(shell);
    skip_same_album.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.update_playback_settings(|settings| {
                settings.skip_same_album_crossfade = row.is_active()
            });
        }
    });
    let row = options.downgrade();
    revealer.connect_child_revealed_notify(move |revealer| {
        if !revealer.reveals_child()
            && !revealer.is_child_revealed()
            && let Some(row) = row.upgrade()
        {
            row.set_visible(false);
        }
    });
    let row = options.downgrade();
    let revealer = revealer.downgrade();
    let weak = Rc::downgrade(shell);
    crossfade.connect_active_notify(move |button| {
        let active = button.is_active();
        if let (Some(row), Some(revealer)) = (row.upgrade(), revealer.upgrade()) {
            if active {
                row.set_visible(true);
            }
            revealer.set_reveal_child(active);
        }
        if let Some(shell) = weak.upgrade() {
            shell.update_playback_settings(|settings| {
                settings.transition_mode = if active {
                    PlaybackTransitionMode::Crossfade
                } else {
                    PlaybackTransitionMode::Gapless
                };
            });
        }
    });
    (mode, options)
}

fn crossfade_duration_row(
    shell: &Rc<PlayerUi>,
    initial_seconds: u8,
    scale_width: i32,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(String::from(CROSSFADE_DURATION_TITLE))
        .build();
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        f64::from(MIN_CROSSFADE_SECONDS),
        f64::from(MAX_CROSSFADE_SECONDS),
        1.0,
    );
    scale.add_css_class("playback-setting-scale");
    install_scale_scroll_forwarding(&scale);
    scale.set_width_request(scale_width);
    scale.set_valign(gtk::Align::Center);
    install_sliding_value_bubble(&scale, |seconds| format!("{seconds:.0}"));
    scale.add_mark(1.0, gtk::PositionType::Bottom, Some("1"));
    scale.add_mark(5.0, gtk::PositionType::Bottom, Some("5"));
    scale.add_mark(10.0, gtk::PositionType::Bottom, Some("10"));
    scale.add_mark(15.0, gtk::PositionType::Bottom, Some("15"));
    scale.add_mark(20.0, gtk::PositionType::Bottom, Some("20"));
    scale.add_mark(25.0, gtk::PositionType::Bottom, Some("25"));
    scale.add_mark(30.0, gtk::PositionType::Bottom, Some("30"));
    scale.set_value(f64::from(initial_seconds));
    let crossfade_shell = Rc::clone(shell);
    scale.connect_value_changed(move |scale| {
        let seconds = scale.value().round() as u8;
        crossfade_shell.update_playback_settings(|settings| settings.crossfade_seconds = seconds);
    });
    row.add_suffix(&scale);
    row.set_activatable_widget(Some(&scale));
    row
}

use playback::VolumeScale;
pub fn volume_scale_index(scale: VolumeScale) -> u32 {
    match scale {
        VolumeScale::Perceptual => 0,
        VolumeScale::Linear => 1,
    }
}
pub fn volume_scale_from_index(index: u32) -> VolumeScale {
    match index {
        1 => VolumeScale::Linear,
        _ => VolumeScale::Perceptual,
    }
}
