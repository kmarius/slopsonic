use super::Shell;
use adw::prelude::*;
use gtk_widgets::route::Route;
use std::{cell::RefCell, rc::Rc};
impl Shell {
    pub(super) fn build_catalog(
        self: &Rc<Self>,
        route: &Route,
        scoped: bool,
    ) -> Rc<gtk_library::CatalogUi> {
        let selected = self.selected_library().as_deref().cloned();
        let has_selected = selected.is_some();
        let source = self.products.source.clone();
        Rc::new(gtk_library::CatalogUi {
            library: self.products.library.clone(),
            runtime: self.products.runtime.clone(),
            source: self.products.source.clone(),
            playback: self.products.playback.clone(),
            settings: Rc::clone(&self.settings),
            artwork: Rc::clone(&self.artwork),
            downloads: Rc::clone(&self.downloads),
            media_menus: Rc::clone(&self.media_menus),
            selected,
            favorites: self
                .selected_ui
                .session()
                .map(|session| Rc::clone(&session.favorites)),
            window: self.chrome.window.downgrade(),
            global_search: self.chrome.topbar.search.clone(),
            search_session: Rc::clone(&self.search_session),
            current: RefCell::new(None),
            current_track_selections: RefCell::new(Vec::new()),
            media_playing_cells: RefCell::new(Vec::new()),
            track_selections: RefCell::new(Vec::new()),
            playlist_entry_selection: RefCell::new(None),
            is_current: {
                let weak = Rc::downgrade(self);
                let route = route.clone();
                Rc::new(move || {
                    weak.upgrade().is_some_and(|shell| {
                        shell.navigation.routes.borrow().current() == &route
                            && (!scoped || shell.selected_library().is_some())
                    })
                })
            },
            route_width: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    weak.upgrade()
                        .map_or(1, |shell| super::layout::route_content_width(&shell))
                })
            },

            refresh_home_section: {
                Rc::new(move |kind| {
                    if has_selected {
                        source.refresh_home(kind);
                    }
                })
            },

            reconcile: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.reconcile_mounted_route();
                    }
                })
            },
            refresh: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.replace_current_route_when_ready();
                    }
                })
            },
            present_full_artwork: {
                let weak = Rc::downgrade(self);
                Rc::new(move |artwork| {
                    if let Some(shell) = weak.upgrade() {
                        shell.present_full_artwork_group(artwork);
                    }
                })
            },
            present_selected_dialog: {
                let weak = Rc::downgrade(self);
                Rc::new(move |dialog| {
                    if let Some(shell) = weak.upgrade() {
                        shell.present_selected_dialog(dialog);
                    }
                })
            },
            set_favorite_category: {
                let weak = Rc::downgrade(self);
                Rc::new(move |category| {
                    if let Some(shell) = weak.upgrade() {
                        shell.set_favorite_category(category);
                    }
                })
            },
            cycle_favorite_category: {
                let weak = Rc::downgrade(self);
                Rc::new(move || {
                    if let Some(shell) = weak.upgrade() {
                        shell.set_favorite_category(
                            shell
                                .navigation
                                .favorite_category
                                .get()
                                .next_in(&gtk_widgets::route::CollectionCategory::FAVORITES),
                        );
                    }
                })
            },
            playlist_context_menu: {
                let weak = Rc::downgrade(self);
                Rc::new(move |target, playlist, play, position| {
                    if let Some(shell) = weak.upgrade() {
                        gtk_media_menus::media_menus::present_playlist_context_menu(
                            target,
                            &shell.media_menus,
                            playlist,
                            play,
                            position,
                            gtk_player::state::current_playback_track(
                                shell.player_ui.selected_playback().as_deref(),
                            )
                            .map(|track| track.media_uri),
                        );
                    }
                })
            },
        })
    }
}
