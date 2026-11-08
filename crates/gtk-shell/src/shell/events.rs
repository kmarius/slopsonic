use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::WidgetExt;
use slopsonic_core::text::fill;
use tracing::warn;

use crate::shell::playlist_picker::refresh_context_playlist_picker;
use gtk_player::fullscreen::{FullscreenPlaybackRefresh, fullscreen_playback_refresh};
use gtk_player::state::NowPlayingPresentation;
use gtk_widgets::route::Route;
use slopsonic_core::runtime::source::source_progress_text;
use slopsonic_core::runtime::source::{SourceOperation, SourceProgress};
use slopsonic_core::runtime::{
    CatalogChange, CatalogPublication, ProductReceivers, SourceEvent, SourceNotice,
    SourceNoticeKind,
};

use super::Shell;
use super::navigation::update_sidebar_pin_playback;
use gtk_widgets::mounted_route::route_current_track;

pub(crate) fn install_product_event_receivers(shell: &Rc<Shell>, receivers: ProductReceivers) {
    let ProductReceivers { source, downloads } = receivers;

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(event) = source.recv().await {
            apply_source_event(&event_shell, event);
        }
    });

    let event_shell = Rc::clone(shell);
    glib::spawn_future_local(async move {
        while let Ok(event) = downloads.recv().await {
            event_shell.apply_download_event(event);
        }
    });

    let event_shell = Rc::clone(shell);
    let mut playback = shell.products.playback.updates.subscribe();
    glib::spawn_future_local(async move {
        while let Ok(publication) = playback.recv().await {
            if let Some(publication) = publication {
                apply_playback_projection(&event_shell, publication);
            } else {
                event_shell.player_ui.clear_player();
                event_shell.player_ui.refresh_queue_window();
                event_shell.player_ui.update_bottom_player();
                event_shell
                    .player_ui
                    .update_fullscreen_player_with(&NowPlayingPresentation::new(None));
                event_shell.refresh_current_route_now_playing_selections();
                update_sidebar_pin_playback(&event_shell);
                event_shell.update_media_controls_after(None);
            }
        }
    });
}

fn apply_source_event(shell: &Rc<Shell>, event: SourceEvent) {
    match event {
        SourceEvent::Configured(configured) => apply_source_status(shell, configured),
        SourceEvent::Selected {
            configured,
            selected,
        } => {
            let added = matches!(
                *shell.source.operation.borrow(),
                SourceOperation::Adding { .. }
            );
            apply_selected_source(shell, configured, selected);
            if added {
                shell.preferences.complete_add_server_dialog();
            }
        }
        SourceEvent::Operation(operation) => apply_source_operation(shell, operation),
        SourceEvent::CatalogPublished(publication) => apply_catalog_publication(shell, publication),
        SourceEvent::Notice(notice) => apply_source_notice(shell, notice),
        SourceEvent::PlaylistSourceMismatch(source_id) => {
            let source_name = shell
                .source
                .configured
                .borrow()
                .iter()
                .map(|status| &status.source)
                .find(|source| source.id == source_id)
                .map(gtk_widgets::source_labels::configured_source_display_name)
                .unwrap_or_else(|| source_id.as_str().to_string());
            shell.control_feedback.show_feedback_toast(fill(
                "This playlist only accepts {source} tracks",
                &[("source", &source_name)],
            ));
        }
        SourceEvent::ReleaseSelected => release_selected_source(shell),
    }
}

fn release_selected_source(shell: &Rc<Shell>) {
    shell.search_session.set_selected(None);
    let released = shell.selected_ui.take();
    drop(released);
    let route = shell.navigation.routes.borrow().current().clone();
    if shell.route_uses_selected_source(&route) {
        shell.replace_current_route_when_ready();
    }
    shell.player_ui.update_bottom_player();
}

fn apply_source_status(
    shell: &Rc<Shell>,
    configured: Option<slopsonic_core::runtime::source::SourceStatus>,
) {
    *shell.source.configured.borrow_mut() = configured;
    refresh_context_playlist_picker(shell);
    shell.player_ui.update_bottom_player();
}

fn apply_selected_source(
    shell: &Rc<Shell>,
    configured: Option<slopsonic_core::runtime::source::SourceStatus>,
    selected: slopsonic_core::runtime::SelectedLibrary,
) {
    let changed = shell.selected_library().as_deref().is_none_or(|previous| {
        previous.source_key != selected.source_key
            || previous.music_folder_key != selected.music_folder_key
            || previous.music_folder_object_id != selected.music_folder_object_id
    });
    *shell.source.configured.borrow_mut() = configured;
    shell
        .selected_ui
        .install(super::selected_ui::SelectedUiSession::new(selected.clone()));
    shell.search_session.set_selected(Some(selected));
    refresh_context_playlist_picker(shell);
    let route = shell.navigation.routes.borrow().current().clone();
    if changed && shell.route_uses_selected_source(&route) {
        shell.replace_current_route_when_ready();
    }
    if !shell.startup.route_revealed.get() && !shell.source.operation.borrow().blocks_library() {
        shell.prepare_startup_route_content();
    }
}

fn apply_catalog_publication(shell: &Rc<Shell>, publication: CatalogPublication) {
    let mut scope_changed = false;
    if matches!(
        publication.change,
        CatalogChange::Broad | CatalogChange::Scope
    ) && let Some(selected) = shell.products.source.selected_library()
        && publication.source_key == Some(selected.source_key)
        && shell
            .selected_library()
            .is_some_and(|current| current.source_key == selected.source_key)
    {
        {
            let mut session = shell.selected_ui.session_mut().unwrap();
            scope_changed = session.library.music_folder_key != selected.music_folder_key
                || session.library.music_folder_object_id != selected.music_folder_object_id;
            session.library.music_folder_key = selected.music_folder_key;
            session.library.music_folder_object_id = selected.music_folder_object_id.clone();
            session.library.music_folders = Arc::clone(&selected.music_folders);
        }
        shell.search_session.set_selected(Some(selected));
    }
    if matches!(
        publication.change,
        CatalogChange::Broad | CatalogChange::Playlists(_)
    ) {
        shell.import_remote_playlist_pins_once();
    }
    if let Some(favorite) = publication.favorite {
        shell.apply_favorite_settlement(
            favorite.target.clone(),
            favorite.requested,
            favorite.effective,
        );
        if favorite.requested != favorite.effective {
            shell
                .control_feedback
                .show_feedback_toast("Could not update favorites".to_string());
        }
        shell.apply_favorite_settlement_to_mounted_route(favorite);
        return;
    }
    let current = shell.navigation.routes.borrow().current().clone();
    let affects_current = publication.source_key.is_none()
        || shell
            .selected_library()
            .as_deref()
            .is_some_and(|selected| Some(selected.source_key) == publication.source_key);
    let source_scoped =
        current.requires_selected_source() && shell.route_uses_selected_source(&current);
    match publication.change {
        CatalogChange::Broad => {
            refresh_context_playlist_picker(shell);
            super::navigation::refresh_sidebar_pins(shell);
            if !affects_current && source_scoped {
                return;
            }
            if scope_changed && shell.route_uses_selected_source(&current) {
                shell.replace_current_route_when_ready();
            } else if current == Route::Home {
                shell.refresh_mounted_home();
            } else {
                shell.refresh_mounted_catalog();
            }
        }
        CatalogChange::Activity => {
            if !affects_current && source_scoped {
                return;
            }
            let settings = shell.settings.current.borrow();
            let category = if current == Route::Favorites {
                shell.navigation.favorite_category.get()
            } else {
                slopsonic_core::route::CollectionCategory::Tracks
            };
            let sensitive = [category, slopsonic_core::route::CollectionCategory::Albums]
                .into_iter()
                .take(
                    if matches!(
                        current,
                        Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
                    ) {
                        2
                    } else {
                        1
                    },
                )
                .any(|category| {
                    slopsonic_core::browse::uses_activity(
                        &current,
                        &settings.library_list(slopsonic_core::browse::library_list_key(
                            &current, category,
                        )),
                    )
                });
            drop(settings);
            if sensitive {
                if current == Route::Home {
                    shell.refresh_mounted_home();
                } else {
                    shell.refresh_mounted_catalog();
                }
            }
        }
        CatalogChange::Scope => {
            if affects_current {
                refresh_context_playlist_picker(shell);
                if shell.route_uses_selected_source(&current) {
                    shell.replace_current_route_when_ready();
                }
            }
        }
        CatalogChange::Home => {
            if affects_current && current == Route::Home {
                shell.refresh_mounted_home();
            }
        }
        CatalogChange::Playlists(playlist) => {
            refresh_context_playlist_picker(shell);
            super::navigation::refresh_sidebar_pins(shell);
            if matches!(current, Route::Playlists)
                || matches!(current, Route::PlaylistDetail(key) if playlist.is_none_or(|playlist| playlist == key))
            {
                shell.refresh_mounted_catalog();
            }
        }
        CatalogChange::Album(album) => {
            super::navigation::refresh_sidebar_pins(shell);
            if current == Route::AlbumDetail(album)
                || matches!(
                    current,
                    Route::ArtistDetail(_)
                        | Route::AlbumArtistDetail(_)
                        | Route::ArtistDiscography(_)
                        | Route::AlbumArtistDiscography(_)
                )
            {
                shell.refresh_mounted_catalog();
            }
        }
    }
}

fn apply_source_notice(shell: &Rc<Shell>, notice: SourceNotice) {
    let matches_selected = shell
        .selected_library()
        .as_deref()
        .is_some_and(|selected| selected.source_key == notice.source_key);
    if !matches_selected {
        return;
    }
    shell
        .control_feedback
        .show_feedback_toast(match notice.kind {
            SourceNoticeKind::ServerUnreachable => "Server is unreachable".to_string(),
            SourceNoticeKind::FavoriteRejected => "Could not update favorites".to_string(),
        });
}

fn apply_source_operation(shell: &Rc<Shell>, operation: SourceOperation) {
    let previous_operation = shell.source.operation.borrow().clone();
    let previously_blocked = previous_operation.blocks_library();
    let started_blocking = source_operation_started_blocking(&previous_operation, &operation);
    *shell.source.operation.borrow_mut() = operation.clone();
    if operation.blocks_library() {
        shell.chrome.source_refresh_feedback.set_visible(false);
    }
    apply_source_refresh_feedback(shell, &previous_operation, &operation);

    match &operation {
        SourceOperation::Adding { .. } | SourceOperation::Switching { .. } => {
            if started_blocking && matches!(operation, SourceOperation::Adding { .. }) {
                shell.preferences.close_preferences_dialog();
            }
            if !shell.has_active_mounted_route() {
                shell.startup.route_revealed.set(false);
                shell.render_startup_loading_view();
            }
        }
        SourceOperation::Refreshing { .. } | SourceOperation::Downloading { .. } => {}
        SourceOperation::Failed {
            message, add_form, ..
        } => {
            warn!(error = %message, "source operation failed");
            if *add_form {
                if !shell.preferences.restore_add_server_dialog_after_failure() {
                    shell.preferences.update_add_server_dialog();
                }
                if previously_blocked {
                    shell.prepare_startup_route_content();
                }
            } else if previously_blocked {
                shell.prepare_startup_route_content();
            }
        }
        SourceOperation::Idle => {
            shell.preferences.update_add_server_dialog();
            if shell.selected_library().is_some() && !shell.startup.route_revealed.get() {
                shell.prepare_startup_route_content();
            }
        }
    }
}

fn apply_source_refresh_feedback(
    shell: &Shell,
    previous: &SourceOperation,
    operation: &SourceOperation,
) {
    let refreshing = matches!(previous, SourceOperation::Refreshing { .. });
    match operation {
        SourceOperation::Refreshing {
            source_id,
            progress,
        } => {
            let continues = matches!(
                previous,
                SourceOperation::Refreshing {
                    source_id: previous_source_id,
                    progress: previous_progress,
                } if previous_source_id == source_id && previous_progress.stage == progress.stage
            );
            let generation = shell
                .source
                .refresh_feedback_generation
                .get()
                .wrapping_add(1);
            shell.source.refresh_feedback_generation.set(generation);
            shell
                .chrome
                .source_refresh_feedback
                .remove_css_class("error");
            shell
                .chrome
                .source_refresh_feedback_label
                .set_text(&source_progress_text(progress));
            let next_fraction = source_progress_fraction(progress);
            let fraction = if continues {
                shell
                    .chrome
                    .source_refresh_feedback_progress
                    .fraction()
                    .max(next_fraction)
            } else {
                next_fraction
            };
            shell
                .chrome
                .source_refresh_feedback_progress
                .set_fraction(fraction);
            shell
                .chrome
                .source_refresh_feedback_progress
                .set_visible(progress.total.is_some_and(|total| total > 0));
            shell.chrome.source_refresh_feedback.set_visible(true);
        }
        SourceOperation::Idle | SourceOperation::Downloading { .. } if refreshing => {
            finish_source_refresh_feedback(shell, None, Duration::from_millis(1_200));
        }
        SourceOperation::Failed {
            message,
            add_form: false,
            ..
        } if refreshing => {
            finish_source_refresh_feedback(shell, Some(message), Duration::from_secs(5));
        }
        SourceOperation::Failed {
            message,
            add_form: false,
            ..
        } => {
            shell.control_feedback.show_feedback_toast(message.clone());
        }
        SourceOperation::Adding { .. } | SourceOperation::Switching { .. } if refreshing => {
            shell.source.refresh_feedback_generation.set(
                shell
                    .source
                    .refresh_feedback_generation
                    .get()
                    .wrapping_add(1),
            );
            shell.chrome.source_refresh_feedback.set_visible(false);
        }
        _ => {}
    }
}

fn finish_source_refresh_feedback(shell: &Shell, error: Option<&str>, delay: Duration) {
    let generation = shell
        .source
        .refresh_feedback_generation
        .get()
        .wrapping_add(1);
    shell.source.refresh_feedback_generation.set(generation);
    if let Some(error) = error {
        shell.chrome.source_refresh_feedback.add_css_class("error");
        shell.chrome.source_refresh_feedback_label.set_text(error);
    }
    shell
        .chrome
        .source_refresh_feedback_progress
        .set_fraction(1.0);
    shell.chrome.source_refresh_feedback.set_visible(true);
    let feedback = shell.chrome.source_refresh_feedback.clone();
    let active_generation = Rc::clone(&shell.source.refresh_feedback_generation);
    glib::timeout_add_local_once(delay, move || {
        if active_generation.get() == generation {
            feedback.set_visible(false);
        }
    });
}

fn source_progress_fraction(progress: &SourceProgress) -> f64 {
    match progress.total {
        Some(0) => 1.0,
        Some(total) => progress.completed.min(total) as f64 / total as f64,
        None => 0.0,
    }
}

fn source_operation_started_blocking(previous: &SourceOperation, next: &SourceOperation) -> bool {
    !previous.blocks_library() && next.blocks_library()
}

fn apply_playback_projection(shell: &Rc<Shell>, projection: playback::PlaybackProjection) {
    let previous_player = shell.player_ui.selected_playback().as_deref().cloned();
    let playback::PlaybackProjection { view, notices } = projection;
    let queue_window_changed = previous_player.as_ref().is_none_or(|previous| {
        previous.queue.revision != view.queue.revision
            || previous.queue_loading != view.queue_loading
            || previous.queue.total != view.queue.total
            || previous.queue_window != view.queue_window
    });
    shell.player_ui.replace_player(view.clone());
    finish_playback_projection(shell, previous_player, view, notices, queue_window_changed);
}

fn finish_playback_projection(
    shell: &Rc<Shell>,
    previous_player: Option<playback::PlaybackView>,
    next_player: playback::PlaybackView,
    notices: Vec<playback::PlaybackNotice>,
    queue_window_changed: bool,
) {
    let previous_media = previous_player
        .as_ref()
        .and_then(|player| player.transport.current.as_ref())
        .map(|current| &current.id);
    let next_media = next_player
        .transport
        .current
        .as_ref()
        .map(|current| &current.id);
    let media_changed = previous_media != next_media;
    let fullscreen_refresh = fullscreen_playback_refresh(previous_player.as_ref(), &next_player);
    let static_playback_changed = matches!(fullscreen_refresh, FullscreenPlaybackRefresh::Static);
    let now_playing =
        static_playback_changed.then(|| NowPlayingPresentation::new(Some(&next_player)));
    let position_only =
        bottom_player_can_update_position_only(previous_player.as_ref(), &next_player);
    let media_controls_static_changed =
        media_controls_static_state_changed(previous_player.as_ref(), &next_player);
    let previous_queue_current = previous_player
        .as_ref()
        .and_then(|player| player.queue.current_index.as_ref());
    let next_queue_current = next_player.queue.current_index.as_ref();
    let queue_panel_changed = queue_panel_refresh_needed(
        queue_window_changed,
        previous_queue_current,
        next_queue_current,
    );

    if queue_window_changed {
        shell.player_ui.refresh_queue_window();
    }

    let previous_route_track = route_current_track(previous_player.as_ref());
    let next_route_track = route_current_track(Some(&next_player));
    if previous_route_track != next_route_track {
        shell.refresh_current_route_now_playing_selections();
        update_sidebar_pin_playback(shell);
    }
    shell
        .player_ui
        .maybe_clear_player_seek_preview(&next_player, media_changed);
    if let Some(now_playing) = now_playing.as_ref() {
        shell
            .player_ui
            .update_bottom_player_with(&next_player, now_playing);
    }
    if position_only {
        shell.player_ui.update_bottom_player_position();
    } else if !static_playback_changed {
        shell.player_ui.update_bottom_player_transport();
    }

    let mut media_controls_discontinuity = None;
    for notice in notices {
        match notice {
            playback::PlaybackNotice::PositionDiscontinuity(discontinuity) => {
                media_controls_discontinuity = Some(discontinuity);
            }
            playback::PlaybackNotice::OperationFailed(error) => {
                show_playback_failure(shell, error);
            }
        }
    }

    if let Some(now_playing) = now_playing.as_ref() {
        shell.player_ui.update_fullscreen_player_with(now_playing);
    }
    if media_controls_static_changed {
        shell.update_media_controls_after(media_controls_discontinuity);
    } else {
        shell.update_media_controls_position_after(
            next_player
                .transport
                .current
                .as_ref()
                .map(|_| next_player.transport.position_millis),
            media_controls_discontinuity,
        );
    }
    if queue_panel_changed && !queue_window_changed {
        shell.player_ui.schedule_queue_panel_render();
    }
}

fn show_playback_failure(shell: &Rc<Shell>, error: String) {
    shell
        .control_feedback
        .show_feedback_toast(playback_error_toast(&error));
}

fn queue_panel_refresh_needed(
    page_changed: bool,
    previous_current: Option<&usize>,
    next_current: Option<&usize>,
) -> bool {
    page_changed || previous_current != next_current
}

fn bottom_player_can_update_position_only(
    previous: Option<&playback::PlaybackView>,
    next: &playback::PlaybackView,
) -> bool {
    previous.is_some_and(|previous| {
        previous.transport.current == next.transport.current
            && previous.transport.effective_state() == next.transport.effective_state()
            && previous.transport.duration_millis == next.transport.duration_millis
            && previous.transport.can_seek == next.transport.can_seek
            && previous.transport.buffering_percent == next.transport.buffering_percent
            && previous.transport.error == next.transport.error
            && previous.controls == next.controls
    })
}

fn media_controls_static_state_changed(
    previous: Option<&playback::PlaybackView>,
    next: &playback::PlaybackView,
) -> bool {
    previous.is_none_or(|previous| {
        previous.transport.current != next.transport.current
            || previous.transport.effective_state() != next.transport.effective_state()
            || previous.transport.duration_millis != next.transport.duration_millis
            || previous.transport.can_seek != next.transport.can_seek
            || previous.controls.repeat_mode != next.controls.repeat_mode
            || previous.controls.shuffle_enabled != next.controls.shuffle_enabled
            || previous.controls.auto_dj_enabled != next.controls.auto_dj_enabled
            || previous.controls.volume != next.controls.volume
            || previous.queue.next_index != next.queue.next_index
            || previous.queue.can_next != next.queue.can_next
    })
}

fn playback_error_toast(error: &str) -> String {
    match error {
        "Not Found"
        | "File not found"
        | "Resource not found"
        | "Resource not found."
        | "the configured source no longer exists"
        | "Could not play this track" => "Could not play this track".to_string(),
        "Track unavailable" => "Track unavailable".to_string(),
        "Format not supported" => "Format not supported".to_string(),
        "Couldn't decode this track" => "Couldn't decode this track".to_string(),
        "Access denied" => "Access denied".to_string(),
        "Couldn't connect to the server" => "Couldn't connect to the server".to_string(),
        "Audio output failed" => "Audio output failed".to_string(),
        _ => error.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use playback::{
        ControlsView, PlaybackView, QueueSummaryView, RepeatMode, TransportStatus, TransportView,
    };
    use sources::SourceId;

    use super::{
        bottom_player_can_update_position_only, media_controls_static_state_changed,
        queue_panel_refresh_needed, source_operation_started_blocking, source_progress_fraction,
    };
    use slopsonic_core::runtime::source::{SourceOperation, SourceProgress, SourceProgressStage};

    #[test]
    fn playback_toasts_omit_backend_diagnostics() {
        assert_eq!(
            super::playback_error_toast("the configured source no longer exists"),
            "Could not play this track".to_string()
        );
        assert_eq!(
            super::playback_error_toast("Resource not found"),
            "Could not play this track".to_string()
        );
        assert_eq!(
            super::playback_error_toast("Could not play this track"),
            "Could not play this track".to_string()
        );
    }

    fn adding() -> SourceOperation {
        SourceOperation::Adding {
            progress: SourceProgress {
                stage: SourceProgressStage::Connecting,
                completed: 0,
                total: None,
            },
        }
    }

    #[test]
    fn source_progress_does_not_restart_the_blocking_transition() {
        assert!(source_operation_started_blocking(
            &SourceOperation::Idle,
            &adding()
        ));
        assert!(!source_operation_started_blocking(&adding(), &adding()));
        assert!(source_operation_started_blocking(
            &SourceOperation::Idle,
            &SourceOperation::Switching {
                target: SourceId::new("target"),
                progress: SourceProgress {
                    stage: SourceProgressStage::Connecting,
                    completed: 0,
                    total: None,
                },
            }
        ));
    }

    #[test]
    fn source_progress_follows_the_visible_stage_ratio() {
        let fraction = |stage, completed, total| {
            source_progress_fraction(&SourceProgress {
                stage,
                completed,
                total,
            })
        };
        assert_eq!(fraction(SourceProgressStage::Connecting, 0, None), 0.0);
        assert_eq!(fraction(SourceProgressStage::Tracks, 200, None), 0.0);
        assert_eq!(fraction(SourceProgressStage::Tracks, 10, Some(10)), 1.0);
        assert_eq!(fraction(SourceProgressStage::Tracks, 0, Some(10)), 0.0);
        assert_eq!(fraction(SourceProgressStage::Tracks, 5, Some(10)), 0.5);
        assert_eq!(
            fraction(SourceProgressStage::Tracks, 52, Some(2_584)),
            52.0 / 2_584.0
        );
        assert_eq!(fraction(SourceProgressStage::Finalizing, 1, Some(1)), 1.0);
    }

    #[test]
    fn queue_panel_refresh_ignores_unchanged_playback_ticks() {
        let current = 0;
        let next = 1;

        assert!(!queue_panel_refresh_needed(
            false,
            Some(&current),
            Some(&current)
        ));
        assert!(queue_panel_refresh_needed(
            false,
            Some(&current),
            Some(&next)
        ));
        assert!(queue_panel_refresh_needed(
            true,
            Some(&current),
            Some(&current)
        ));
    }

    #[test]
    fn playback_ticks_only_update_position_owned_surfaces() {
        let previous = idle_playback_view();
        let mut tick = previous.clone();
        tick.transport.position_millis = 500;

        assert!(bottom_player_can_update_position_only(
            Some(&previous),
            &tick
        ));
        assert!(!media_controls_static_state_changed(Some(&previous), &tick));

        let mut seekability_change = tick.clone();
        seekability_change.transport.can_seek = true;
        for (previous, next) in [(&tick, &seekability_change), (&seekability_change, &tick)] {
            assert!(!bottom_player_can_update_position_only(
                Some(previous),
                next
            ));
            assert!(media_controls_static_state_changed(Some(previous), next));
        }

        let mut state_change = tick.clone();
        state_change.transport.state = TransportStatus::Playing;
        state_change.transport.desired_playing = true;
        assert!(!bottom_player_can_update_position_only(
            Some(&tick),
            &state_change
        ));
        assert!(media_controls_static_state_changed(
            Some(&tick),
            &state_change
        ));

        let mut duration_change = tick.clone();
        duration_change.transport.duration_millis = 2_000;
        assert!(media_controls_static_state_changed(
            Some(&tick),
            &duration_change
        ));

        let mut queue_change = tick.clone();
        queue_change.queue.next_index = Some(1);
        assert!(bottom_player_can_update_position_only(
            Some(&tick),
            &queue_change
        ));
        assert!(media_controls_static_state_changed(
            Some(&tick),
            &queue_change
        ));
    }

    fn idle_playback_view() -> PlaybackView {
        PlaybackView {
            queue_loading: false,
            queue_window: Vec::new(),
            queue: QueueSummaryView {
                revision: 0,
                total: 0,
                current_index: None,
                next_index: None,
                can_next: false,
            },
            transport: TransportView {
                current: None,
                state: TransportStatus::Stopped,
                desired_playing: false,
                position_millis: 0,
                position_observed_at_millis: 0,
                duration_millis: 0,
                can_seek: false,
                buffering_percent: None,
                error: None,
            },
            controls: ControlsView {
                repeat_mode: RepeatMode::Off,
                shuffle_enabled: false,
                auto_dj_enabled: false,
                volume: 1.0,
                muted: false,
            },
        }
    }
}
