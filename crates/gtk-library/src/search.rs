use gtk_widgets::media_drag::{MediaDragSource, install_media_drag_source};
use gtk_widgets::sparse_model::item_at_from_item;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use library::{ReadCancellation, SearchResults};
use slopsonic_core::playback::QueuePlacement;

use crate::CatalogUi;
use crate::{LibraryField, LibraryLayout};
use gtk_widgets::mounted_route::{MountedRoute, MountedRouteItemNavigation};
use slopsonic_core::runtime::SelectedLibrary;

use super::collection_context::{present_collection_menu, present_track_context_menu};
use super::collections::{
    LibraryCollectionProjection, LibraryPresentationProjection, dynamic_collection_table,
};
use super::columns::{
    album_column, artist_column, connect_column_cell_setup, track_column_fit_width,
    track_column_for_key,
};
use super::grid_cells::{AlbumGridCell, ArtistGridCell, collection_grid_with_selection_and_demand};
use gtk_media_menus::media_menus::{
    present_album_context_menu, present_artist_context_menu, present_collection_selection_menu,
};
use gtk_widgets::library_fields::column_fit_width;
use slopsonic_core::settings::app::{RecentSearchKind, RecentSearchResult};

use super::route_shell::{LibraryPageShell, LibraryToolbarProjection};
use super::table_sizing::route_column_view_initial_width;
use gtk_widgets::library_fields::{
    album_field, artist_field, column_width, playlist_field, track_field,
};
use gtk_widgets::route::CollectionCategory;
use gtk_widgets::route::Route;
use gtk_widgets::sparse_model::SparseObjectItem;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
pub mod session;

#[derive(Clone)]
#[expect(clippy::large_enum_variant, reason = "bounded prepared Search rows")]
enum SearchItem {
    Track(library::TrackView),
    Album(library::AlbumView),
    Artist(library::ArtistView),
    Playlist(library::PlaylistRow),
}

impl SearchItem {
    fn recent_result(&self) -> Option<RecentSearchResult> {
        let (kind, media_uri, artwork_binding) = match self {
            Self::Track(row) => (
                RecentSearchKind::Track,
                &row.media_uri,
                &row.artwork_binding,
            ),
            Self::Album(row) => (
                RecentSearchKind::Album,
                &row.media_uri,
                &row.artwork_binding,
            ),
            Self::Artist(row) => (
                RecentSearchKind::Artist,
                &row.media_uri,
                &row.artwork_binding,
            ),
            Self::Playlist(_) => return None,
        };
        Some(RecentSearchResult {
            kind,
            media_uri: media_uri.clone(),
            title: self.title().to_owned(),
            subtitle: self.subtitle().to_owned(),
            artwork_binding: artwork_binding.clone(),
        })
    }
    fn play_request(&self) -> slopsonic_core::playback::PlayRequest {
        match self {
            Self::Track(row) => gtk_widgets::selection::track_play_request(row.track_key),
            Self::Album(row) => gtk_widgets::selection::album_play_request(row),
            Self::Artist(row) => gtk_widgets::selection::artist_play_request(row, false),
            Self::Playlist(row) => gtk_widgets::selection::collection_play_request(
                library::Collection::Playlist(row.playlist_key),
            ),
        }
    }
    fn title(&self) -> &str {
        match self {
            Self::Track(row) => &row.metadata.title,
            Self::Album(row) => &row.metadata.title,
            Self::Artist(row) => &row.metadata.name,
            Self::Playlist(row) => &row.name,
        }
    }
    fn artwork(&self) -> ArtworkBinding {
        let bytes = match self {
            Self::Track(row) => &row.artwork_binding,
            Self::Album(row) => &row.artwork_binding,
            Self::Artist(row) => &row.artwork_binding,
            Self::Playlist(row) => &row.artwork_binding,
        };
        bytes
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default()
    }
    fn field(&self, field: LibraryField) -> String {
        match self {
            Self::Track(row) => track_field(row, field),
            Self::Album(row) => album_field(row, field),
            Self::Artist(row) => artist_field(row, field),
            Self::Playlist(row) => playlist_field(row, field),
        }
    }
    fn subtitle(&self) -> &str {
        match self {
            Self::Track(row) => &row.metadata.artist,
            Self::Album(row) => &row.metadata.artist,
            Self::Artist(_) | Self::Playlist(_) => "",
        }
    }
    fn numeric_field(&self, field: LibraryField) -> Option<i64> {
        match self {
            Self::Track(row) => match field {
                LibraryField::Year => row.metadata.year,
                LibraryField::Duration => Some(row.metadata.duration_millis),
                LibraryField::PlayCount => Some(row.state.play_count),
                LibraryField::UserRating => row.state.rating,
                LibraryField::Favorite => Some(i64::from(row.state.favorite)),
                _ => None,
            },
            Self::Album(row) => match field {
                LibraryField::Year => row.metadata.year,
                LibraryField::Duration => Some(row.summary.duration_millis),
                LibraryField::PlayCount => Some(row.state.play_count),
                LibraryField::UserRating => row.state.rating,
                LibraryField::SongCount => Some(row.summary.track_count),
                LibraryField::Favorite => Some(i64::from(row.state.favorite)),
                _ => None,
            },
            Self::Artist(row) => match field {
                LibraryField::Duration => Some(row.summary.duration_millis),
                LibraryField::PlayCount => Some(row.state.play_count),
                LibraryField::AlbumCount => Some(row.summary.album_count),
                LibraryField::SongCount => Some(row.summary.track_count),
                LibraryField::Favorite => Some(i64::from(row.state.favorite)),
                _ => None,
            },
            Self::Playlist(row) => match field {
                LibraryField::Duration => Some(row.duration_millis),
                LibraryField::SongCount => Some(row.track_count),
                _ => None,
            },
        }
    }
    fn date_field(&self, field: LibraryField) -> Option<&str> {
        match self {
            Self::Track(row) => match field {
                LibraryField::ReleaseDate => row.metadata.release_date.as_deref(),
                LibraryField::DateAdded => row.metadata.date_added.as_deref(),
                _ => None,
            },
            Self::Album(row) => match field {
                LibraryField::ReleaseDate => row.metadata.release_date.as_deref(),
                LibraryField::DateAdded => row.metadata.date_added.as_deref(),
                _ => None,
            },
            Self::Artist(_) | Self::Playlist(_) => None,
        }
    }
    fn play(&self, shell: &Rc<CatalogUi>, placement: QueuePlacement) {
        if let Some(result) = self.recent_result() {
            shell
                .settings
                .update_app_settings("recent search", |settings| {
                    settings.remember_search_result(result)
                });
        }
        let mut request = self.play_request();
        request.placement = placement;
        shell.playback.play_selection(request);
    }
    fn activate(&self, shell: &Rc<CatalogUi>) {
        match (self, self.recent_result()) {
            (Self::Track(..), _) => self.play(shell, QueuePlacement::Now),
            (Self::Playlist(row), _) => shell.navigate(Route::PlaylistDetail(row.playlist_key)),
            (_, Some(result)) => session::SearchSession::activate_recent(&result, shell),
            (_, None) => {}
        }
    }
    fn present_context(
        &self,
        target: &gtk::Widget,
        shell: &Rc<CatalogUi>,
        position: Option<(f64, f64)>,
        is_current: Rc<dyn Fn() -> bool>,
    ) {
        let menus = Rc::clone(&shell.media_menus);
        match self.clone() {
            Self::Track(row) => {
                if !present_collection_selection_menu(target, &menus, self.play_request(), position)
                {
                    present_track_context_menu(target, shell, row.media_uri, position);
                }
            }
            Self::Album(row) => present_collection_menu(
                target,
                shell,
                true,
                self.play_request(),
                row.media_uri.clone(),
                position,
                is_current,
                move |target| present_album_context_menu(target, &menus, row, None, None, position),
            ),
            Self::Artist(row) => present_collection_menu(
                target,
                shell,
                true,
                self.play_request(),
                row.media_uri.clone(),
                position,
                is_current,
                move |target| {
                    present_artist_context_menu(target, &menus, row, false, None, position)
                },
            ),
            Self::Playlist(row) => (shell.playlist_context_menu)(target, row, None, position),
        }
    }
}

struct SearchRouteProjection {
    root: gtk::Widget,
    shell: Weak<CatalogUi>,
    search: gtk::SearchEntry,
    session: Rc<session::SearchSession>,
    observer: RefCell<Option<Rc<dyn Fn()>>>,
    revision: Cell<Option<u64>>,
    status: gtk::Stack,
    results: adw::ViewStack,
    result_page: gtk::Stack,
    toolbar_host: gtk::Box,
    models: [gio::ListStore; 4],
    playing: super::columns::TrackRowPlayingIndicator,
    active: RefCell<Option<SearchPresentation>>,
}

struct SearchPresentation {
    category: CollectionCategory,
    collection: LibraryCollectionProjection,
    toolbar: LibraryToolbarProjection,
    navigation: MountedRouteItemNavigation,
}

gtk_widgets::composite_box!(
    pub SearchRouteView,
    search_route_view_imp,
    "SlopsonicSearchRouteView",
    "/io/github/kmarius/Slopsonic/ui/routes/search.ui",
    {
        toolbar_host: gtk::Box,
        category_host: gtk::Box,
        status: gtk::Stack,
        result_page: gtk::Stack,
    }
);

impl SearchRouteProjection {
    fn new(shell: &Rc<CatalogUi>) -> Rc<Self> {
        let wrapper = SearchRouteView::new();
        let search = shell.global_search.clone();

        let models = CollectionCategory::ALL.map(|_| gio::ListStore::new::<SparseObjectItem>());
        let (results, switcher) = collection_category_tabs(
            &CollectionCategory::ALL,
            shell.search_session.category.get(),
        );
        wrapper.imp().category_host.append(&switcher);
        let toolbar_host = wrapper.imp().toolbar_host.get();
        let status = wrapper.imp().status.get();
        let result_page = wrapper.imp().result_page.get();

        let projection = Rc::new(Self {
            root: wrapper.upcast(),
            shell: Rc::downgrade(shell),
            search,
            session: Rc::clone(&shell.search_session),
            observer: RefCell::new(None),
            revision: Cell::new(None),
            status,
            results,
            result_page,
            toolbar_host,
            models,
            playing: super::columns::TrackRowPlayingIndicator::new(),
            active: RefCell::new(None),
        });
        projection.connect();
        projection.mount_category(projection.session.category.get());
        projection.register_now_playing(shell);
        projection.sync();
        projection
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.observer.replace(Some(self.session.observe(move || {
            if let Some(projection) = weak.upgrade() {
                projection.sync();
            }
        })));
        let weak = Rc::downgrade(self);
        self.results.connect_visible_child_notify(move |results| {
            let Some(projection) = weak.upgrade() else {
                return;
            };
            let category = results
                .visible_child_name()
                .as_deref()
                .map(CollectionCategory::from_name)
                .unwrap_or_default();
            projection.mount_category(category);
            projection.session.category.set(category);
        });
    }

    fn mount_category(&self, category: CollectionCategory) {
        if self.active.borrow().as_ref().map(|active| active.category) == Some(category) {
            return;
        }
        if let Some(previous) = self.active.borrow_mut().take() {
            self.toolbar_host.remove(&previous.toolbar.widget());
            self.result_page
                .remove(&previous.collection.scrolling_widget());
        }
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        self.sort_model(category);
        let collection = search_collection(
            &shell,
            self.models[category as usize].clone(),
            category,
            self.playing.clone(),
        );
        let toolbar = shell.library_toolbar_projection_without_detail(
            category.key(),
            None,
            category.sort_fields(),
        );
        let presentation = SearchPresentation {
            category,
            navigation: collection.item_navigation(),
            collection,
            toolbar,
        };
        self.toolbar_host.append(&presentation.toolbar.widget());
        self.result_page
            .add_named(&presentation.collection.scrolling_widget(), Some("results"));
        self.active.replace(Some(presentation));
        self.show_results(self.models[category as usize].n_items() != 0);
    }

    fn register_now_playing(&self, shell: &Rc<CatalogUi>) {
        let model = self.models[CollectionCategory::Tracks as usize].clone();
        let indicator = self.playing.clone();
        shell.register_current_route_track_selection(Rc::new(move |current| {
            let position = current
                .and_then(|current| {
                    (0..model.n_items()).find(|position| {
                        model
                            .item(*position)
                            .and_then(|item| {
                                gtk_widgets::sparse_model::object_item::<library::TrackView, _>(
                                    item,
                                    |track| track.media_uri == current.media_uri,
                                )
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(gtk::INVALID_LIST_POSITION);
            indicator.set_current(current.map(|current| current.media_uri.as_str()), position);
            indicator.set_paused(current.is_some_and(|current| current.paused));
            true
        }));
    }

    fn show_results(&self, has_results: bool) {
        self.result_page
            .set_visible_child_name(if has_results { "results" } else { "empty" });
    }

    fn sync(&self) {
        self.results
            .set_visible_child_name(self.session.category.get().name());
        if self.revision.replace(Some(self.session.revision.get()))
            == Some(self.session.revision.get())
        {
            return;
        }
        let items = self.session.items.borrow().clone();
        self.apply(items);
        self.status.set_visible_child_name(self.session.status());
    }

    fn apply(&self, mut items: [Vec<SearchItem>; 4]) {
        for ((items, model), category) in items
            .iter_mut()
            .zip(&self.models)
            .zip(CollectionCategory::ALL)
        {
            if let Some(shell) = self.shell.upgrade() {
                let settings = shell.settings.current.borrow().library_list(category.key());
                sort_search_items(items, settings.sort_key, settings.descending);
            }
            let additions = items.iter().cloned().map(search_object).collect::<Vec<_>>();
            model.splice(0, model.n_items(), &additions);
        }
        if let Some(active) = self.active.borrow().as_ref() {
            self.show_results(self.models[active.category as usize].n_items() != 0);
        }
        self.status.set_visible_child_name("results");
    }

    fn resume(&self) {
        self.sync();
        for category in CollectionCategory::ALL {
            self.sort_model(category);
        }
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        if let Some(active) = self.active.borrow().as_ref() {
            let settings = shell
                .settings
                .current
                .borrow()
                .library_list(active.category.key());
            active.collection.apply_settings(&settings);
            active.toolbar.apply(active.category.key(), &settings);
        }
    }

    fn sort_model(&self, category: CollectionCategory) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let settings = shell.settings.current.borrow().library_list(category.key());
        let model = &self.models[category as usize];
        let mut items = (0..model.n_items())
            .filter_map(|position| search_item(model, category, position))
            .collect::<Vec<_>>();
        sort_search_items(&mut items, settings.sort_key, settings.descending);
        let additions = items.into_iter().map(search_object).collect::<Vec<_>>();
        model.splice(0, model.n_items(), &additions);
    }

    fn cycle_category(&self) {
        let current = self
            .active
            .borrow()
            .as_ref()
            .map_or(CollectionCategory::default(), |presentation| {
                presentation.category
            });
        self.results
            .set_visible_child_name(current.next_in(&CollectionCategory::ALL).name());
    }

    fn cycle_layout(&self) {
        if let Some(active) = self.active.borrow().as_ref() {
            active.toolbar.cycle_layout();
        }
    }
}

fn sort_search_items(items: &mut [SearchItem], field: LibraryField, descending: bool) {
    items.sort_by(|left, right| {
        compare_optional(left.numeric_field(field), right.numeric_field(field))
            .then_with(|| compare_optional(left.date_field(field), right.date_field(field)))
            .then_with(|| {
                left.field(field)
                    .to_lowercase()
                    .cmp(&right.field(field).to_lowercase())
            })
            .then_with(|| left.title().cmp(right.title()))
    });
    if descending {
        items.reverse();
    }
}

fn compare_optional<T: Ord>(left: Option<T>, right: Option<T>) -> std::cmp::Ordering {
    left.cmp(&right)
}

fn collection_category_tabs(
    categories: &[CollectionCategory],
    active: CollectionCategory,
) -> (adw::ViewStack, adw::ViewSwitcher) {
    let stack = adw::ViewStack::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    for &category in categories {
        stack.add_titled_with_icon(
            &gtk::Box::new(gtk::Orientation::Vertical, 0),
            Some(category.name()),
            &String::from(category.title()),
            category.icon_name(),
        );
    }
    stack.set_visible_child_name(active.name());
    let switcher = adw::ViewSwitcher::builder()
        .policy(adw::ViewSwitcherPolicy::Wide)
        .stack(&stack)
        .build();
    switcher.set_halign(gtk::Align::Start);
    (stack, switcher)
}

impl CatalogUi {
    pub fn install_favorite_category_tabs(
        self: &Rc<Self>,
        active: CollectionCategory,
        page: &mut LibraryPageShell,
    ) {
        let (pages, switcher) = collection_category_tabs(&CollectionCategory::FAVORITES, active);
        page.set_category_switcher(&switcher);

        let shell = Rc::downgrade(self);
        pages.connect_visible_child_notify(move |pages| {
            let Some(category) = pages
                .visible_child_name()
                .as_deref()
                .map(CollectionCategory::from_name)
            else {
                return;
            };
            if category != active
                && let Some(shell) = shell.upgrade()
            {
                (shell.set_favorite_category)(category);
            }
        });
        let shell = Rc::downgrade(self);
        page.set_tab_cycle(Rc::new(move || {
            if let Some(shell) = shell.upgrade() {
                (shell.cycle_favorite_category)();
            }
        }));
    }

    pub fn search_route(self: &Rc<Self>) -> MountedRoute {
        let projection = SearchRouteProjection::new(self);
        let layout_projection = Rc::downgrade(&projection);
        let layout_cycle = Rc::new(move || {
            if let Some(projection) = layout_projection.upgrade() {
                projection.cycle_layout();
            }
        });
        let tab_projection = Rc::downgrade(&projection);
        let tab_cycle = Rc::new(move || {
            if let Some(projection) = tab_projection.upgrade() {
                projection.cycle_category();
            }
        });
        let resume_projection = Rc::clone(&projection);
        let navigation_projection = Rc::downgrade(&projection);
        let navigation = Rc::new(move |direction| {
            navigation_projection
                .upgrade()
                .and_then(|projection| {
                    projection
                        .active
                        .borrow()
                        .as_ref()
                        .map(|active| (active.navigation)(direction))
                })
                .unwrap_or(glib::Propagation::Stop)
        });

        let download_counts = Rc::downgrade(&projection);
        let download_tracks = Rc::downgrade(&projection);
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            let Some(projection) = download_counts.upgrade() else {
                return;
            };
            update_rows::<library::AlbumView>(
                &projection.models[CollectionCategory::Albums as usize],
                |row| {
                    identity.strip_prefix("album:") == Some(row.media_uri.as_str())
                        && downloaded_summary_changes(&row.summary, downloaded)
                },
                |row| set_downloaded_summary(&mut row.summary, downloaded),
            );
            update_rows::<library::ArtistView>(
                &projection.models[CollectionCategory::Artists as usize],
                |row| {
                    identity.strip_prefix("artist:") == Some(row.media_uri.as_str())
                        && downloaded_summary_changes(&row.summary, downloaded)
                },
                |row| set_downloaded_summary(&mut row.summary, downloaded),
            );
            update_rows::<library::PlaylistRow>(
                &projection.models[CollectionCategory::Playlists as usize],
                |row| {
                    identity.strip_prefix("playlist:")
                        == Some(row.playlist_key.to_string().as_str())
                        && (row.downloaded_count == row.track_count) != downloaded
                },
                |row| row.downloaded_count = if downloaded { row.track_count } else { 0 },
            );
        });
        MountedRoute::new(
            projection.root.clone(),
            Rc::new(move || resume_projection.resume()),
        )
        .with_download_change(downloads)
        .with_download_change(Rc::new(move |event| {
            let downloads::DownloadEvent::Changed {
                media_uri: uri,
                downloaded,
            } = event
            else {
                return;
            };
            let (uri, downloaded) = (uri.as_str(), *downloaded);
            if let Some(projection) = download_tracks.upgrade() {
                update_rows::<library::TrackView>(
                    &projection.models[CollectionCategory::Tracks as usize],
                    |row| row.media_uri == uri && row.access.is_downloaded != downloaded,
                    |row| row.access.is_downloaded = downloaded,
                );
            }
        }))
        .with_search(projection.search.clone())
        .with_layout_cycle(layout_cycle)
        .with_tab_cycle(tab_cycle)
        .with_item_navigation(navigation)
    }
}

fn search_collection(
    shell: &Rc<CatalogUi>,
    model: gio::ListStore,
    category: CollectionCategory,
    playing: super::columns::TrackRowPlayingIndicator,
) -> LibraryCollectionProjection {
    let key = category.key();
    let settings = shell.settings.current.borrow().library_list(key);
    let selection = gtk_widgets::selection::PositionSelectionModel::new(model.clone());
    let selected_model = model.downgrade();
    selection.set_collection_selection(Box::new(move |clicked, positions| {
        let model = selected_model.upgrade()?;
        let (iter, first) = gtk::BitsetIter::init_first(positions)?;
        let targets = std::iter::once(first)
            .chain(iter)
            .filter_map(|position| search_item(&model, category, position))
            .map(|item| item.play_request())
            .collect::<Vec<_>>();
        targets
            .iter()
            .any(|target| target.selection() == clicked.selection())
            .then_some(gtk_widgets::media_drag::CollectionTargets::Ready(targets))
    }));
    let selection: gtk::SelectionModel = selection.upcast();
    let shell = Rc::clone(shell);
    LibraryCollectionProjection::new(
        settings,
        Rc::new(move |layout| {
            let list = shell.settings.current.borrow().library_list(key);
            let activate_shell = Rc::clone(&shell);
            let activate_model = model.clone();
            let activate = move |position| {
                if let Some(item) = search_item(&activate_model, category, position) {
                    item.activate(&activate_shell);
                }
            };
            match (layout, category) {
                (_, CollectionCategory::Playlists) => {
                    super::named_collections::playlist_presentation(&shell, model.clone(), layout)
                }
                (LibraryLayout::Grid, CollectionCategory::Albums) => {
                    let cell_shell = Rc::clone(&shell);
                    LibraryPresentationProjection::Grid(collection_grid_with_selection_and_demand(
                        model.clone(),
                        Some(selection.clone()),
                        &list.grid_fields,
                        move |fields| AlbumGridCell::new(&cell_shell, fields, None, true),
                        move |position, _: library::AlbumView| activate(position),
                        |_| {},
                    ))
                }
                (LibraryLayout::Grid, CollectionCategory::Artists) => {
                    let cell_shell = Rc::clone(&shell);
                    LibraryPresentationProjection::Grid(collection_grid_with_selection_and_demand(
                        model.clone(),
                        Some(selection.clone()),
                        &list.grid_fields,
                        move |fields| ArtistGridCell::new(&cell_shell, fields, false, true),
                        move |position, _: library::ArtistView| activate(position),
                        |_| {},
                    ))
                }
                _ => LibraryPresentationProjection::Row(match category {
                    CollectionCategory::Tracks => search_table::<library::TrackView>(
                        &shell,
                        category,
                        &model,
                        &list.row_fields,
                        &selection,
                        &playing,
                        activate,
                    ),
                    CollectionCategory::Albums => search_table::<library::AlbumView>(
                        &shell,
                        category,
                        &model,
                        &list.row_fields,
                        &selection,
                        &playing,
                        activate,
                    ),
                    CollectionCategory::Artists => search_table::<library::ArtistView>(
                        &shell,
                        category,
                        &model,
                        &list.row_fields,
                        &selection,
                        &playing,
                        activate,
                    ),
                    CollectionCategory::Playlists => unreachable!("playlist search presentation"),
                }),
            }
        }),
    )
}

fn search_table<T: Clone + 'static>(
    shell: &Rc<CatalogUi>,
    category: CollectionCategory,
    model: &gio::ListStore,
    fields: &[LibraryField],
    selection: &gtk::SelectionModel,
    playing: &super::columns::TrackRowPlayingIndicator,
    activate: impl Fn(u32) + 'static,
) -> super::collections::CollectionTableProjection {
    let key = category.key();
    let column_shell = Rc::clone(shell);
    let playing = playing.clone();
    let table = dynamic_collection_table(
        shell,
        key,
        model.clone(),
        fields,
        Vec::new(),
        move |field| search_column(&column_shell, category, field, &playing),
        move |field| match category {
            CollectionCategory::Tracks => track_column_fit_width(key, field),
            _ => column_fit_width(field, column_width(field)),
        },
        category != CollectionCategory::Tracks,
        Some(Box::new(move |position, _: T| activate(position))),
        Some(selection.clone()),
        route_column_view_initial_width(shell),
    );
    if category == CollectionCategory::Tracks {
        table.widget().add_css_class("track-list");
    }
    table
}

fn search_column(
    shell: &Rc<CatalogUi>,
    category: CollectionCategory,
    field: LibraryField,
    playing: &super::columns::TrackRowPlayingIndicator,
) -> gtk::ColumnViewColumn {
    match category {
        CollectionCategory::Albums => album_column(shell, field, None, true),
        CollectionCategory::Artists => artist_column(shell, field, false, true),
        CollectionCategory::Playlists => unreachable!("playlist search presentation"),
        CollectionCategory::Tracks => {
            let column =
                track_column_for_key::<library::TrackView>(shell, category.key(), field, playing);
            if field != LibraryField::Favorite {
                connect_column_cell_setup(&column, |item, child| {
                    let item = item.downgrade();
                    install_media_drag_source(child, move || {
                        let track = item_at_from_item::<library::TrackView>(&item.upgrade()?)?;
                        Some((
                            MediaDragSource::capture_target(
                                gtk_widgets::selection::track_play_request(track.track_key),
                            ),
                            track.metadata.title,
                        ))
                    });
                });
            }
            column
        }
    }
}

fn search_object(item: SearchItem) -> SparseObjectItem {
    match item {
        SearchItem::Track(row) => SparseObjectItem::new(Arc::new(row), true),
        SearchItem::Album(row) => SparseObjectItem::new(Arc::new(row), true),
        SearchItem::Artist(row) => SparseObjectItem::new(Arc::new(row), true),
        SearchItem::Playlist(row) => SparseObjectItem::new(Arc::new(row), true),
    }
}

fn search_item(
    model: &impl IsA<gio::ListModel>,
    category: CollectionCategory,
    position: u32,
) -> Option<SearchItem> {
    use gtk_widgets::sparse_model::item_at;
    match category {
        CollectionCategory::Tracks => item_at(model, position).map(SearchItem::Track),
        CollectionCategory::Albums => item_at(model, position).map(SearchItem::Album),
        CollectionCategory::Artists => item_at(model, position).map(SearchItem::Artist),
        CollectionCategory::Playlists => item_at(model, position).map(SearchItem::Playlist),
    }
}

fn update_rows<T: Clone + 'static>(
    model: &gio::ListStore,
    matches: impl Fn(&T) -> bool,
    update: impl Fn(&mut T),
) {
    for position in 0..model.n_items() {
        let Some(item) = model.item(position).and_downcast::<SparseObjectItem>() else {
            continue;
        };
        if let Some(row) = item.value::<Arc<T>>().filter(|row| matches(row)) {
            let mut row = (*row).clone();
            update(&mut row);
            item.replace(Arc::new(row), true);
        }
    }
}

fn downloaded_summary_changes(summary: &library::CollectionSummary, downloaded: bool) -> bool {
    (summary.downloaded_count == summary.track_count) != downloaded
}

fn set_downloaded_summary(summary: &mut library::CollectionSummary, downloaded: bool) {
    summary.downloaded_count = if downloaded { summary.track_count } else { 0 };
}

fn cached_items(results: SearchResults) -> [Vec<SearchItem>; 4] {
    [
        results.tracks.into_iter().map(SearchItem::Track).collect(),
        results.albums.into_iter().map(SearchItem::Album).collect(),
        results
            .artists
            .into_iter()
            .map(SearchItem::Artist)
            .collect(),
        results
            .playlists
            .into_iter()
            .map(SearchItem::Playlist)
            .collect(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_tabs_cycle_within_their_categories() {
        let all = &CollectionCategory::ALL;
        let favorites = &CollectionCategory::FAVORITES;
        assert_eq!(
            CollectionCategory::Tracks.next_in(all),
            CollectionCategory::Albums
        );
        assert_eq!(
            CollectionCategory::Artists.next_in(all),
            CollectionCategory::Playlists
        );
        assert_eq!(
            CollectionCategory::Playlists.next_in(all),
            CollectionCategory::Tracks
        );
        assert_eq!(
            CollectionCategory::Artists.next_in(favorites),
            CollectionCategory::Tracks
        );
    }

    #[test]
    fn optional_search_values_follow_native_sort_direction() {
        assert_eq!(
            compare_optional::<i64>(None, Some(1)),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_optional(Some(1_i64), None),
            std::cmp::Ordering::Greater
        );
        assert_eq!(
            compare_optional(Some(1_i64), Some(2)),
            std::cmp::Ordering::Less
        );
    }
}
