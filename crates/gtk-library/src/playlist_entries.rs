use gtk_widgets::media_drag::{
    MediaDragPreviewBinding, MediaDragSource, media_drag_content_provider,
};
use gtk_widgets::sparse_model::item_at_from_item;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use library::{PlaylistEntryView, PlaylistKey};
use std::sync::Arc;

use crate::CatalogUi;
use crate::{LibraryField, LibraryListKey, LibraryListSettings};

use super::collections::{
    CollectionTableProjection, LibraryCollectionProjection, LibraryPresentationProjection,
    dynamic_collection_table, library_route_inset,
};
use super::columns::{TrackRowPlayingIndicator, track_column_fit_width, track_column_for_key};
use crate::playlist_entry_model::{PlaylistEntryModel, PlaylistEntryProjectionRequest};

use super::route_shell::LibraryToolbarProjection;
use super::table_sizing::column_view_initial_width;
use crate::track_selection::PlaylistEntrySelection;

#[derive(Clone)]
pub struct PlaylistEntriesView {
    widget: gtk::Widget,
    collection: LibraryCollectionProjection,
    toolbar: LibraryToolbarProjection,
    model: PlaylistEntryModel,
    search: gtk::SearchEntry,
    stack: gtk::Stack,
    toolbar_widget: gtk::Widget,
}

impl PlaylistEntriesView {
    pub fn play(
        &self,
        queue: std::sync::Arc<slopsonic_core::playback::PlaybackOwner>,
        placement: slopsonic_core::playback::QueuePlacement,
        shuffled: bool,
    ) {
        self.model.play(0, queue, placement, true, shuffled);
    }

    pub fn widget(&self) -> gtk::Widget {
        self.widget.clone()
    }

    pub fn item_navigation(&self) -> gtk_widgets::mounted_route::MountedRouteItemNavigation {
        self.collection.item_navigation()
    }

    pub fn search(&self) -> gtk::SearchEntry {
        self.search.clone()
    }

    pub fn layout_cycle(&self) -> gtk_widgets::mounted_route::MountedRouteCommand {
        self.toolbar.layout_cycle()
    }

    pub fn resume_initial_demand(&self) {
        self.model.resume_initial_demand();
    }

    pub fn update_downloaded(&self, media_uri: &str, downloaded: bool) {
        self.model.update_downloaded(media_uri, downloaded);
    }

    pub fn connect_search_request(
        &self,
        callback: impl Fn(PlaylistEntryProjectionRequest) + 'static,
    ) {
        let model = self.model.clone();
        self.search.connect_search_changed(move |search| {
            if model.set_query(&search.text()) {
                callback(model.projection_request());
            }
        });
    }

    pub fn projection_request(&self) -> PlaylistEntryProjectionRequest {
        self.model.projection_request()
    }

    pub fn replace_prepared(
        &self,
        request: PlaylistEntryProjectionRequest,
        count: usize,
        rows: Vec<PlaylistEntryView>,
    ) {
        let empty = count == 0;
        let filtered = !request.query.filter.is_empty();
        self.model.replace_prepared(count, request, rows);
        self.toolbar_widget.set_visible(!empty || filtered);
        self.stack
            .set_visible_child_name(if empty { "empty" } else { "content" });
    }

    pub fn apply_library_list_settings(
        &self,
        key: LibraryListKey,
        settings: &LibraryListSettings,
    ) -> bool {
        if key != LibraryListKey::PlaylistTracks {
            return false;
        }
        let previous = self.model.projection_request();
        self.model.apply_settings(settings.clone());
        self.collection.apply_settings(settings);
        self.toolbar.apply(key, settings);
        previous.settings.sort_key != settings.sort_key
            || previous.settings.descending != settings.descending
    }
}

impl CatalogUi {
    pub fn playlist_entries_view(
        self: &Rc<Self>,
        playlist: PlaylistKey,
        playlist_name: String,
        writable: bool,
        count: usize,
        first_row_position: usize,
        first_rows: Vec<PlaylistEntryView>,
        read: library::MemberRead,
    ) -> PlaylistEntriesView {
        let settings = self
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::PlaylistTracks);
        let model = PlaylistEntryModel::new(
            self.library.clone(),
            self.runtime.clone(),
            playlist,
            count,
            first_row_position,
            first_rows,
            settings,
            read,
        );
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 8);
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);

        let search = gtk::SearchEntry::new();
        gtk_widgets::controls::configure_filter_entry(&search);
        search.set_placeholder_text(Some("Filter"));
        search.set_hexpand(true);
        search.set_width_request(1);
        let toolbar =
            self.library_toolbar_projection(LibraryListKey::PlaylistTracks, search.clone());
        let toolbar_widget = library_route_inset(toolbar.widget());
        toolbar_widget.set_visible(!model.source_is_empty());
        wrapper.append(&toolbar_widget);

        let collection =
            playlist_entry_collection(self, model.clone(), playlist, playlist_name, writable);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(
            &library_route_inset(crate::route_layout::placeholder_view(
                "Tracks",
                "No tracks here yet",
            )),
            Some("empty"),
        );
        stack.add_named(&collection.scrolling_widget(), Some("content"));
        stack.set_visible_child_name(if model.source_is_empty() {
            "empty"
        } else {
            "content"
        });
        wrapper.append(&stack);

        PlaylistEntriesView {
            widget: wrapper.upcast(),
            collection,
            toolbar,
            model,
            search,
            stack,
            toolbar_widget,
        }
    }
}

fn playlist_entry_collection(
    shell: &Rc<CatalogUi>,
    model: PlaylistEntryModel,
    playlist: PlaylistKey,
    playlist_name: String,
    writable: bool,
) -> LibraryCollectionProjection {
    let settings = shell
        .settings
        .current
        .borrow()
        .library_list(LibraryListKey::PlaylistTracks);
    let playing = TrackRowPlayingIndicator::new();
    let selection = PlaylistEntrySelection::new(model.clone(), playlist_name, writable);
    shell.set_current_playlist_entry_selection(selection.clone());
    let current_model = model.clone();
    let current_playing = playing.clone();
    shell.register_current_route_track_selection(Rc::new(move |current| {
        let position = current
            .and_then(|current| {
                let context = current.context.as_ref()?;
                current_model.position_for_current(
                    &current.media_uri,
                    &context.context_id,
                    context.source_rank,
                )
            })
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        current_playing.set_current(current.map(|current| current.media_uri.as_str()), position);
        current_playing.set_paused(current.is_some_and(|current| current.paused));
        true
    }));
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |_| {
            LibraryPresentationProjection::Row(playlist_entry_table(
                &shell,
                model.clone(),
                playlist,
                playing.clone(),
                selection.clone(),
            ))
        }),
    )
}

fn playlist_entry_table(
    shell: &Rc<CatalogUi>,
    model: PlaylistEntryModel,
    playlist: PlaylistKey,
    playing: TrackRowPlayingIndicator,
    selection: PlaylistEntrySelection,
) -> CollectionTableProjection {
    let key = LibraryListKey::PlaylistTracks;
    let fields = shell.settings.current.borrow().library_list(key).row_fields;
    let column_shell = Rc::clone(shell);
    let activate_model = model.clone();
    let queue = shell.playback.clone();
    let table = dynamic_collection_table(
        shell,
        key,
        model.list_model(),
        &fields,
        Vec::new(),
        move |field| {
            let column =
                track_column_for_key::<PlaylistEntryView>(&column_shell, key, field, &playing);
            if field != LibraryField::Favorite {
                install_playlist_entry_column_drag(&column, &column_shell, playlist);
            }
            column
        },
        move |field| track_column_fit_width(key, field),
        false,
        Some(Box::new(move |position, _: PlaylistEntryView| {
            activate_model.activate(position, queue.clone());
        })),
        Some(selection.selection_model()),
        column_view_initial_width(shell, crate::route_layout::PRIMARY_ROUTE_HORIZONTAL_INSET),
    );
    table.widget().add_css_class("track-list");
    table
}

fn install_playlist_entry_column_drag(
    column: &gtk::ColumnViewColumn,
    shell: &Rc<CatalogUi>,
    playlist: PlaylistKey,
) {
    let shell = Rc::downgrade(shell);
    super::columns::connect_column_cell_setup(column, move |item, child| {
        if let Some(shell) = shell.upgrade() {
            install_playlist_entry_drag(child, &shell, playlist, list_item_entry(item));
        }
    });
}

fn list_item_entry(item: &gtk::ListItem) -> Rc<dyn Fn() -> Option<PlaylistEntryView>> {
    let item = item.downgrade();
    Rc::new(move || {
        item.upgrade()
            .and_then(|item| item_at_from_item::<PlaylistEntryView>(&item))
    })
}

fn install_playlist_entry_drag(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<CatalogUi>,
    playlist: PlaylistKey,
    current: Rc<dyn Fn() -> Option<PlaylistEntryView>>,
) {
    target.as_ref().set_valign(gtk::Align::Fill);
    let drag_source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::COPY | gtk::gdk::DragAction::MOVE)
        .build();
    drag_source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let preview = MediaDragPreviewBinding::default();
    preview.connect(&drag_source);
    let drag_preview = preview.clone();
    let drag_shell = Rc::downgrade(shell);
    let drag_current = Rc::clone(&current);
    drag_source.connect_prepare(move |_, _, _| {
        drag_preview.clear();
        let shell = drag_shell.upgrade()?;
        let entry = drag_current()?;
        let title = entry.track.metadata.title.clone();
        let artwork = entry
            .track
            .artwork_binding
            .as_deref()
            .map(artwork::ArtworkBinding::opaque)
            .unwrap_or_default();
        let selection = shell
            .current_playlist_entry_selection(entry.playlist_entry_key)
            .or_else(|| {
                shell
                    .current_playlist_entry_selection_owner()
                    .map(|selection| selection.single_entry(entry.playlist_entry_key))
            })?;
        drag_preview.prepare_cache_only(&shell.artwork, title, artwork);
        Some(media_drag_content_provider(
            MediaDragSource::playlist_entries(selection),
        ))
    });
    target.as_ref().add_controller(drag_source);

    let move_shell = Rc::downgrade(shell);
    let drop_target = gtk::DropTarget::new(
        glib::BoxedAnyObject::static_type(),
        gtk::gdk::DragAction::MOVE,
    );
    drop_target.set_preload(true);
    drop_target.connect_notify_local(Some("value"), move |drop, _| {
        let Some(value) = drop.value() else { return };
        if !matches!(gtk_widgets::media_drag::media_drag_source(&value), Some(MediaDragSource::PlaylistEntries(selection)) if selection.playlist == playlist && selection.writable && selection.count == 1) {
            drop.reject();
        }
    });
    drop_target.connect_drop(move |drop, value, x, y| {
        let Some(MediaDragSource::PlaylistEntries(selection)) =
            gtk_widgets::media_drag::media_drag_source(value)
        else {
            return false;
        };
        if selection.playlist != playlist || !selection.writable || selection.count != 1 {
            return false;
        }
        let library::MemberSelection::Entries(order) = &selection.input else {
            return false;
        };
        let Some(entry) = order.first().copied() else {
            return false;
        };
        let Some(target) = current() else {
            return false;
        };
        if entry == target.playlist_entry_key {
            return false;
        }
        let Some(shell) = move_shell.upgrade() else {
            return false;
        };
        let after = drop
            .widget()
            .is_some_and(|widget| gtk_widgets::media_drag::order_drop_after(&widget, x, y));
        let settings = shell
            .settings
            .current
            .borrow()
            .library_list(LibraryListKey::PlaylistTracks);
        let after = after
            != (settings.descending
                && settings.sort_key.catalog_sort() == library::CatalogSort::Position);
        let database = Arc::clone(&shell.library);
        let task = shell.runtime.spawn(async move {
            database
                .playlist_entries(&[entry], &library::ReadCancellation::new())
                .await
        });
        let weak = Rc::downgrade(&shell);
        glib::spawn_future_local(async move {
            let (Ok(Ok(rows)), Some(shell)) = (task.await, weak.upgrade()) else {
                return;
            };
            let Some(moved) = rows.first() else { return };
            let position = target.position + i64::from(after)
                - i64::from(moved.position < target.position + i64::from(after));
            if position == moved.position {
                return;
            }
            slopsonic_core::playlists::move_playlist_entry(
                &shell.source,
                playlist,
                entry,
                position.max(0) as usize,
            );
        });
        true
    });
    gtk_widgets::media_drag::style_order_drop_target(&drop_target, true, |_| true);
    target.as_ref().add_controller(drop_target);
}
