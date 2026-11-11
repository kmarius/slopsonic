use crate::CatalogUi;
use gtk::prelude::*;
use gtk_media_menus::media_menus::{
    present_playlist_entry_menu, present_playlist_entry_selection_menu, present_track_menu,
    present_track_selection_menu,
};
use std::rc::Rc;

/// Shows a collection menu, first acquiring the collection from the server when `prepare` is set.
#[expect(
    clippy::too_many_arguments,
    reason = "menu target, request and callbacks"
)]
pub fn present_collection_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    prepare: bool,
    selection: slopsonic_core::playback::PlayRequest,
    media_uri: String,
    position: Option<(f64, f64)>,
    is_current: Rc<dyn Fn() -> bool>,
    present: impl FnOnce(&gtk::Widget) + 'static,
) {
    if !prepare {
        present(target);
        return;
    }
    if gtk_media_menus::media_menus::present_collection_selection_menu(
        target,
        &shell.media_menus,
        selection,
        position,
    ) {
        return;
    }
    let receiver = shell.source.prepare_collection(media_uri);
    let target = target.downgrade();
    gtk::glib::spawn_future_local(async move {
        if let Ok(Err(error)) = receiver.recv().await {
            tracing::warn!(%error, "could not acquire collection");
        }
        if let Some(target) = target.upgrade()
            && target.is_mapped()
            && is_current()
        {
            present(&target);
        }
    });
}

pub fn present_row_context_menu<T: 'static>(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    row: &T,
    media_uri: Option<String>,
    position: Option<(f64, f64)>,
) {
    if let Some(entry) = (row as &dyn std::any::Any).downcast_ref::<library::PlaylistEntryView>() {
        present_playlist_entry_context_menu(target, shell, entry.clone(), position);
    } else if let Some(media_uri) = media_uri {
        if shell.current_route_track_selection(&media_uri).is_none()
            && let Some(track) = (row as &dyn std::any::Any).downcast_ref::<library::TrackView>()
            && gtk_media_menus::media_menus::present_collection_selection_menu(
                target,
                &shell.media_menus,
                gtk_widgets::selection::track_play_request(track.track_key),
                position,
            )
        {
            return;
        }
        present_track_context_menu(target, shell, media_uri, position);
    }
}
pub fn present_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    media_uri: String,
    position: Option<(f64, f64)>,
) {
    if let Some(selection) = shell.current_route_track_selection(&media_uri) {
        present_track_selection_menu(target, &shell.media_menus, selection, position);
        return;
    }
    let database = shell.library.clone();
    let task = shell.runtime.spawn(async move {
        database
            .track(&media_uri, &library::ReadCancellation::new())
            .await
    });
    let target = target.clone();
    let menus = Rc::downgrade(&shell.media_menus);
    gtk::glib::spawn_future_local(async move {
        if let (Some(menus), Ok(Ok(Some(song)))) = (menus.upgrade(), task.await) {
            present_track_menu(&target, &menus, song, position, None, None);
        }
    });
}
fn present_playlist_entry_context_menu(
    target: &gtk::Widget,
    shell: &Rc<CatalogUi>,
    entry_row: library::PlaylistEntryView,
    position: Option<(f64, f64)>,
) {
    let entry = entry_row.playlist_entry_key;
    if let Some(selection) = shell.current_playlist_entry_selection(entry) {
        present_playlist_entry_selection_menu(target, &shell.media_menus, selection, position);
        return;
    }
    present_playlist_entry_menu(
        target,
        &shell.media_menus,
        entry_row,
        position,
        shell
            .current_playlist_entry_selection_owner()
            .map(|selection| selection.single_entry(entry)),
    );
}
