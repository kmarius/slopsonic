use std::rc::Rc;

use adw::prelude::*;
use app_identity::{APP_ID, DISPLAY_NAME};
use gtk::{gio, glib};
use playback::PlaybackTransitionMode;
use slopsonic_core::playback::QueuePlacement;

use crate::player::play_saved_random;
use crate::preferences::present_preferences_dialog;
use gtk_widgets::popup::present_light_dismiss_dialog;

use super::{Shell, layout, navigation};

const KEY_SEEK_SECONDS: i32 = 10;
const KEY_VOLUME_STEP: f64 = 0.05;

pub(crate) fn connect_shell_actions(shell: &Rc<Shell>) {
    install_window_actions(shell);
    install_application_actions(shell);
    navigation::install_mouse_history_buttons(shell);
    layout::connect_shell_layout(shell);
}

pub(crate) fn install_window_actions(shell: &Rc<Shell>) {
    add_window_action(shell, "new-playlist", &[], {
        let shell = Rc::clone(shell);
        move || shell.new_playlist_dialog()
    });

    let go_back = gio::SimpleAction::new("go-back", None);
    let go_back_shell = Rc::clone(shell);
    go_back.connect_activate(move |_, _| go_back_shell.go_back());
    shell.chrome.window.add_action(&go_back);

    let go_forward = gio::SimpleAction::new("go-forward", None);
    let go_forward_shell = Rc::clone(shell);
    go_forward.connect_activate(move |_, _| go_forward_shell.go_forward());
    shell.chrome.window.add_action(&go_forward);

    let troubleshooting_accels = &["<Control><Shift>i"][..];
    add_window_action(shell, "troubleshooting", troubleshooting_accels, {
        let shell = Rc::clone(shell);
        move || super::diagnostics::present_diagnostics(&shell)
    });

    let left_sidebar_accels = &["<Control><Alt>s"][..];
    add_window_action(shell, "toggle-left-sidebar", left_sidebar_accels, {
        let shell = Rc::clone(shell);
        move || shell.toggle_active_left_sidebar_size()
    });

    let fullscreen = gio::SimpleAction::new("toggle-fullscreen", None);
    let fullscreen_shell = Rc::clone(shell);
    fullscreen.connect_activate(move |_, _| {
        if fullscreen_shell.chrome.window.is_fullscreen() {
            fullscreen_shell.chrome.window.unfullscreen();
        } else {
            fullscreen_shell.chrome.window.fullscreen();
        }
    });
    shell.chrome.window.add_action(&fullscreen);

    let play_pause_accels = &["<Control>space"][..];
    add_window_action(shell, "play-pause", play_pause_accels, {
        let transport = shell.products.playback.clone();
        move || transport.play_pause()
    });
    let navigate_sidebar =
        gio::SimpleAction::new("navigate-sidebar", Some(glib::VariantTy::UINT32));
    let navigate_shell = Rc::clone(shell);
    navigate_sidebar.connect_activate(move |_, parameter| {
        let Some(position) = parameter.and_then(|position| position.get::<u32>()) else {
            return;
        };
        if let Some(route) =
            navigation::sidebar_route_at_position(&navigate_shell, position as usize)
        {
            navigate_shell.navigate(route);
        }
    });
    shell.chrome.window.add_action(&navigate_sidebar);
    for position in 1..=10 {
        let target = (position as u32).to_variant();
        let action_name = gio::Action::print_detailed_name("win.navigate-sidebar", Some(&target));
        let accelerator_position = position % 10;
        let accelerator = format!("<Control>{accelerator_position}");
        shell
            .chrome
            .application
            .set_accels_for_action(&action_name, &[&accelerator]);
    }
    let previous_track_accels = &["<Control>b"][..];
    add_window_action(shell, "previous-track", previous_track_accels, {
        let transport = shell.products.playback.clone();
        move || transport.previous()
    });
    let next_track_accels = &["<Control>n"][..];
    add_window_action(shell, "next-track", next_track_accels, {
        let transport = shell.products.playback.clone();
        move || transport.next()
    });
    let seek_backward_accels = &["<Control>Left"][..];
    add_window_action(shell, "seek-backward", seek_backward_accels, {
        let shell = Rc::clone(shell);
        move || seek_by(&shell, -KEY_SEEK_SECONDS)
    });
    let seek_forward_accels = &["<Control>Right"][..];
    add_window_action(shell, "seek-forward", seek_forward_accels, {
        let shell = Rc::clone(shell);
        move || seek_by(&shell, KEY_SEEK_SECONDS)
    });
    let shuffle_accels = &["<Control>s"][..];
    add_window_action(shell, "toggle-shuffle", shuffle_accels, {
        let shell = Rc::clone(shell);
        move || toggle_shuffle_shortcut(&shell)
    });
    let repeat_accels = &["<Control>r"][..];
    add_window_action(shell, "cycle-repeat", repeat_accels, {
        let shell = Rc::clone(shell);
        move || cycle_repeat_shortcut(&shell)
    });
    let search_accels = &["<Control>f"][..];
    add_window_action(shell, "focus-search", search_accels, {
        let shell = Rc::clone(shell);
        move || shell.focus_current_route_search()
    });
    let navigate_search_accels = &["<Control>k"][..];
    add_window_action(shell, "navigate-search", navigate_search_accels, {
        let shell = Rc::clone(shell);
        move || shell.focus_topbar_search()
    });
    let cycle_layout_accels = &["<Control>j"][..];
    add_window_action(shell, "cycle-layout", cycle_layout_accels, {
        let shell = Rc::clone(shell);
        move || shell.cycle_current_route_layout()
    });
    add_window_action(shell, "cycle-tabs", &["<Control>Tab"], {
        let shell = Rc::clone(shell);
        move || shell.cycle_current_route_tabs()
    });
    let favorite_accels = &["<Control>l"][..];
    add_window_action(shell, "toggle-favorite", favorite_accels, {
        let shell = Rc::clone(shell);
        move || shell.toggle_current_track_favorite()
    });
    let auto_dj_accels = &["<Control>d"][..];
    add_window_action(shell, "toggle-auto-dj", auto_dj_accels, {
        let shell = Rc::clone(shell);
        move || toggle_auto_dj_shortcut(&shell)
    });
    let clear_queue_accels = &["<Control>period"][..];
    add_window_action(shell, "clear-queue", clear_queue_accels, {
        let shell = Rc::clone(shell);
        move || shell.player_ui.clear_queue()
    });
    let random_accels = &["<Control><Shift>r"][..];
    add_window_action(shell, "play-random", random_accels, {
        let shell = Rc::clone(shell);
        move || play_saved_random(&shell, QueuePlacement::Now)
    });
    let random_next_accels = &["<Control><Shift>n"][..];
    add_window_action(shell, "play-random-next", random_next_accels, {
        let shell = Rc::clone(shell);
        move || play_saved_random(&shell, QueuePlacement::Next)
    });
    let random_later_accels = &["<Control><Shift>t"][..];
    add_window_action(shell, "play-random-later", random_later_accels, {
        let shell = Rc::clone(shell);
        move || play_saved_random(&shell, QueuePlacement::Last)
    });
    let gapless_accels = &["<Control><Shift>g"][..];
    add_window_action(shell, "use-gapless", gapless_accels, {
        let shell = Rc::clone(shell);
        move || set_transition_mode_shortcut(&shell, PlaybackTransitionMode::Gapless)
    });
    let crossfade_accels = &["<Control><Shift>c"][..];
    add_window_action(shell, "use-crossfade", crossfade_accels, {
        let shell = Rc::clone(shell);
        move || set_transition_mode_shortcut(&shell, PlaybackTransitionMode::Crossfade)
    });
    add_window_action(shell, "mute", &[], {
        let shell = Rc::clone(shell);
        move || toggle_mute_shortcut(&shell)
    });
    let volume_up_accels = &["<Control>Up"][..];
    add_window_action(shell, "volume-up", volume_up_accels, {
        let shell = Rc::clone(shell);
        move || adjust_volume(&shell, KEY_VOLUME_STEP)
    });
    let volume_down_accels = &["<Control>Down"][..];
    add_window_action(shell, "volume-down", volume_down_accels, {
        let shell = Rc::clone(shell);
        move || adjust_volume(&shell, -KEY_VOLUME_STEP)
    });
    let queue_accels = &["F9"][..];
    add_window_action(shell, "toggle-queue", queue_accels, {
        let shell = Rc::clone(shell);
        move || shell.toggle_right_panel()
    });
    let refresh_library_accels = &["F5"][..];
    add_window_action(shell, "refresh-library", refresh_library_accels, {
        let shell = Rc::clone(shell);
        move || refresh_selected_library(&shell)
    });
    let fullscreen_player_accels = &["<Shift>F11"][..];
    add_window_action(
        shell,
        "toggle-fullscreen-player",
        fullscreen_player_accels,
        {
            let shell = Rc::clone(shell);
            move || {
                shell.player_ui.toggle_fullscreen_player();
            }
        },
    );
    let primary_menu_accels = &["F10"][..];
    add_window_action(shell, "show-primary-menu", primary_menu_accels, {
        let shell = Rc::clone(shell);
        move || navigation::popup_primary_menu(&shell)
    });

    shell
        .chrome
        .application
        .set_accels_for_action("win.go-back", &["<Alt>Left"]);
    shell
        .chrome
        .application
        .set_accels_for_action("win.go-forward", &["<Alt>Right"]);
    shell
        .chrome
        .application
        .set_accels_for_action("win.toggle-fullscreen", &["F11"]);
}

fn install_application_actions(shell: &Rc<Shell>) {
    let preferences = gio::SimpleAction::new("preferences", None);
    let preferences_shell = Rc::downgrade(shell);
    preferences.connect_activate(move |_, _| {
        if let Some(shell) = preferences_shell.upgrade() {
            present_preferences_dialog(&shell.preferences);
        }
    });
    shell.chrome.application.add_action(&preferences);

    let shortcuts = gio::SimpleAction::new("show-shortcuts", None);
    let shortcuts_shell = Rc::downgrade(shell);
    shortcuts.connect_activate(move |_, _| {
        if let Some(shell) = shortcuts_shell.upgrade() {
            show_shortcuts_dialog(&shell);
        }
    });
    shell.chrome.application.add_action(&shortcuts);

    let about = gio::SimpleAction::new("about", None);
    let about_shell = Rc::downgrade(shell);
    about.connect_activate(move |_, _| {
        if let Some(shell) = about_shell.upgrade() {
            show_about_dialog(&shell);
        }
    });
    shell.chrome.application.add_action(&about);

    shell
        .chrome
        .application
        .set_accels_for_action("app.preferences", &["<Control>comma"]);
    shell
        .chrome
        .application
        .set_accels_for_action("app.show-shortcuts", &["<Control>question"]);
}

fn add_window_action(
    shell: &Rc<Shell>,
    name: &str,
    accels: &[&str],
    activate: impl Fn() + 'static,
) {
    let action = gio::SimpleAction::new(name, None);
    action.connect_activate(move |_, _| activate());
    shell.chrome.window.add_action(&action);
    if !accels.is_empty() {
        shell
            .chrome
            .application
            .set_accels_for_action(&format!("win.{name}"), accels);
    }
}

fn seek_by(shell: &Shell, delta_seconds: i32) {
    let Some(seconds) = ({
        let player = shell.player_ui.selected_playback();
        let Some(player) = player.as_ref() else {
            return;
        };
        let duration_seconds =
            (player.transport.duration_millis / 1_000).min(u64::from(u32::MAX)) as u32;
        if player.transport.current.is_none() || duration_seconds == 0 {
            None
        } else {
            let position_seconds =
                (player.transport.position_millis / 1_000).min(u64::from(u32::MAX)) as u32;
            let target = position_seconds as i32 + delta_seconds;
            Some(target.clamp(0, duration_seconds as i32) as u32)
        }
    }) else {
        return;
    };
    shell.products.playback.seek_seconds(seconds);
}

fn adjust_volume(shell: &Rc<Shell>, delta: f64) {
    let Some(volume) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| (player.controls.volume + delta).clamp(0.0, 1.0))
    else {
        return;
    };
    shell.player_ui.apply_user_volume(volume);
}

fn set_transition_mode_shortcut(shell: &Rc<Shell>, mode: PlaybackTransitionMode) {
    shell
        .player_ui
        .update_playback_settings(|settings| settings.transition_mode = mode);
    shell
        .control_feedback
        .show_control_feedback_toast(match mode {
            PlaybackTransitionMode::Gapless => "Gapless".to_string(),
            PlaybackTransitionMode::Crossfade => "Crossfade".to_string(),
        });
}

fn refresh_selected_library(shell: &Shell) {
    if let Some(selected) = shell.selected_library() {
        let source = shell.products.source.clone();
        let source_id = selected.source_id.clone();
        gtk::glib::spawn_future_local(async move {
            let _ = source.refresh_source(source_id, false).await;
        });
    }
}

fn toggle_shuffle_shortcut(shell: &Shell) {
    let Some(enabled) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| !player.controls.shuffle_enabled)
    else {
        return;
    };
    shell.products.playback.set_shuffle(enabled);
}

fn cycle_repeat_shortcut(shell: &Shell) {
    if shell.player_ui.selected_playback().is_none() {
        return;
    }
    shell.products.playback.cycle_repeat();
}

fn toggle_auto_dj_shortcut(shell: &Shell) {
    if shell.player_ui.selected_playback().is_none() {
        return;
    }
    shell.products.playback.toggle_auto_dj();
}

pub(crate) fn toggle_mute_shortcut(shell: &Rc<Shell>) {
    let Some(muted) = shell
        .player_ui
        .selected_playback()
        .as_deref()
        .map(|player| !player.controls.muted)
    else {
        return;
    };
    shell.player_ui.apply_user_muted(muted);
    let title = if muted {
        "Muted".to_string()
    } else {
        "Unmuted".to_string()
    };
    shell.control_feedback.show_control_feedback_toast(title);
}

fn show_shortcuts_dialog(shell: &Shell) {
    let resource = crate::ui_resource::SHORTCUTS_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    let dialog: adw::ShortcutsDialog =
        gtk_widgets::ui_resource::object(&builder, resource, "shortcuts_dialog");
    let platform_accelerators = [
        ("shortcut_play_pause", "space <Control>space"),
        ("shortcut_multi_selection", "<Control>Pointer_Button1"),
        ("shortcut_select_all", "<Control>a"),
        ("shortcut_selection_play_next", "<Control>Return"),
        ("shortcut_selection_play_later", "<Control><Shift>Return"),
        ("shortcut_add_to_playlist", "<Control><Shift>p"),
        ("shortcut_download", "<Control><Shift>d"),
        ("shortcut_back", "Back <Alt>Left"),
        ("shortcut_forward", "Forward <Alt>Right"),
        ("shortcut_sidebar_position", "<Control>1...9 <Control>0"),
        (
            "shortcut_source_position",
            "<Control><Alt>1...9 <Control><Alt>0",
        ),
    ];
    for (id, accelerator) in platform_accelerators {
        let item: adw::ShortcutsItem = gtk_widgets::ui_resource::object(&builder, resource, id);
        item.set_accelerator(accelerator);
    }

    dialog.connect_map(|dialog| {
        let dialog = dialog.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(dialog) = dialog.upgrade() {
                replace_pointer_shortcut_labels(dialog.upcast_ref());
            }
        });
    });
    drop(builder);
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

fn replace_pointer_shortcut_labels(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(label) = widget.clone().downcast::<adw::ShortcutLabel>() {
            if label.accelerator().contains("Pointer_Button1") {
                replace_pointer_keycaps(label.upcast_ref());
            }
        }
        replace_pointer_shortcut_labels(&widget);
    }
}

fn replace_pointer_keycaps(container: &gtk::Widget) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(label) = widget.clone().downcast::<gtk::Label>()
            && label.has_css_class("keycap")
            && label.text().contains("Pointer")
            && label.text().contains("Button1")
        {
            label.set_text("🖱");
        }
        replace_pointer_keycaps(&widget);
    }
}

fn show_about_dialog(shell: &Shell) {
    let resource = crate::ui_resource::ABOUT_RESOURCE;
    let builder = gtk_widgets::ui_resource::builder(resource);
    let dialog: adw::AboutDialog = gtk_widgets::ui_resource::object(&builder, resource, "dialog");
    dialog.set_application_name(DISPLAY_NAME);
    dialog.set_application_icon(APP_ID);
    dialog.set_version(env!("CARGO_PKG_VERSION"));
    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}
