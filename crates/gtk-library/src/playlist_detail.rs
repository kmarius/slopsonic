use rufin_core::browse::{DetailContent, DetailPage, DetailSummary};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use library::{PlaylistKey, PlaylistRow, ReadCancellation};
use rufin_core::text::track_count_text;

use crate::CatalogUi;
use crate::{LibraryListKey, LibraryListSettings};
use gtk_widgets::artwork::presentation::stable_seed;
use gtk_widgets::format_duration_units;
use gtk_widgets::mounted_route::{LatestMountedRouteRead, MountedRoute};

use super::collections::library_route_inset;
use super::detail_showcase::{
    CollectionDetailShowcase, DetailShowcaseView, collection_detail_showcase,
    detail_playback_controls, detail_radio_button,
};
use crate::route_layout::{
    DETAIL_SHOWCASE_METADATA_MIN_WIDTH, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
};
use gtk_widgets::library_fields::playlist_artwork;

const PLAYLIST_DETAIL_COMPACT_WIDTH: i32 = 760;
const PLAYLIST_DETAIL_TINY_COVER_SIZE: i32 = 150;
const PLAYLIST_DETAIL_WIDE_COVER_SIZE: i32 = 208;

#[derive(Clone, PartialEq)]
struct PlaylistDetailOwner {
    key: PlaylistKey,
    summary: PlaylistRow,
}

impl PlaylistDetailOwner {
    fn name(&self) -> &str {
        &self.summary.name
    }

    fn seed(&self) -> u32 {
        stable_seed(&self.summary.object_id)
    }

    fn track_count(&self) -> i64 {
        self.summary.track_count
    }

    fn duration_millis(&self) -> i64 {
        self.summary.duration_millis
    }

    fn artwork(&self) -> Vec<ArtworkBinding> {
        playlist_artwork(&self.summary)
    }

    fn install_context(
        &self,
        shell: &Rc<CatalogUi>,
        target: &gtk::Widget,
        play: CollectionPlay,
        position: Option<(f64, f64)>,
    ) {
        (shell.playlist_context_menu)(target, self.summary.clone(), Some(play), position)
    }
}

impl CatalogUi {
    pub fn playlist_detail_route(
        self: &Rc<Self>,
        key: PlaylistKey,
        detail: Option<DetailPage>,
    ) -> MountedRoute {
        let Some(DetailPage {
            summary: DetailSummary::Playlist(summary),
            content: DetailContent::Tracks(page),
        }) = detail
        else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                "Playlist",
                "This isn't available",
            ));
        };
        let first_rows = page
            .page
            .rows
            .into_iter()
            .filter_map(library::MemberRow::into_entry)
            .collect();
        let owner = PlaylistDetailOwner {
            key,
            summary: *summary,
        };
        let entries = Rc::new(self.playlist_entries_view(
            key,
            owner.name().to_string(),
            owner.summary.writable,
            page.page.count,
            page.page.first_row_position,
            first_rows,
            page.read,
        ));
        let item_navigation = entries.item_navigation();
        let tracks_widget = entries.widget();
        let database = Arc::clone(&self.library);
        let runtime = self.runtime.clone();
        let load = Arc::new(
            move |request: crate::playlist_entry_model::PlaylistEntryProjectionRequest,
                  cancellation: ReadCancellation| {
                let database = Arc::clone(&database);
                Box::pin(async move {
                    let page = request
                        .query
                        .page(
                            &database,
                            library::RouteSeedWindow::position(request.first_row_position),
                            &cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    let count = page.count;
                    let rows = page
                        .rows
                        .into_iter()
                        .filter_map(library::MemberRow::into_entry)
                        .collect();
                    Ok((count, rows))
                }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
            },
        );
        let apply_entries = Rc::downgrade(&entries);
        let apply = Rc::new(
            move |request, result: Result<(usize, Vec<library::PlaylistEntryView>), String>| {
                if let Ok((count, rows)) = result
                    && let Some(entries) = apply_entries.upgrade()
                {
                    entries.replace_prepared(request, count, rows);
                }
            },
        );
        let read = LatestMountedRouteRead::new_with_request(
            runtime,
            apply,
            load,
            "mounted Playlist tracks",
        );
        let search_read = Rc::downgrade(&read);
        entries.connect_search_request(move |request| {
            if let Some(read) = search_read.upgrade() {
                read.request_with(request);
            }
        });
        let apply_entries = Rc::clone(&entries);
        let refresh_entries = Rc::clone(&entries);
        let refresh = Rc::new(move || {
            read.request_with(refresh_entries.projection_request());
        }) as Rc<dyn Fn()>;
        let apply_refresh = Rc::clone(&refresh);
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            if apply_entries.apply_library_list_settings(LibraryListKey::PlaylistTracks, settings) {
                apply_refresh();
            }
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = entries.search();
        let layout_cycle = entries.layout_cycle();
        let initial_demand = {
            let entries = Rc::clone(&entries);
            Rc::new(move || entries.resume_initial_demand()) as Rc<dyn Fn()>
        };
        let play_queue = self.playback.clone();
        let download_entries = Rc::clone(&entries);
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            entries.play(play_queue.clone(), placement, shuffled);
        });
        self.shared_playlist_detail_route(
            owner,
            tracks_widget,
            item_navigation,
            apply,
            refresh,
            search,
            layout_cycle,
            initial_demand,
            play,
        )
        .with_download_change(Rc::new(move |event| {
            let downloads::DownloadEvent::Changed {
                media_uri,
                downloaded,
            } = event
            else {
                return;
            };
            let (media_uri, downloaded) = (media_uri.as_str(), *downloaded);
            download_entries.update_downloaded(media_uri, downloaded);
        }))
    }

    fn shared_playlist_detail_route(
        self: &Rc<Self>,
        owner: PlaylistDetailOwner,
        tracks_widget: gtk::Widget,
        item_navigation: gtk_widgets::mounted_route::MountedRouteItemNavigation,
        apply_list_settings: Rc<dyn Fn(&LibraryListSettings)>,
        refresh_tracks: Rc<dyn Fn()>,
        search: gtk::SearchEntry,
        layout_cycle: gtk_widgets::mounted_route::MountedRouteCommand,
        initial_demand: Rc<dyn Fn()>,
        play: CollectionPlay,
    ) -> MountedRoute {
        let key = LibraryListKey::PlaylistTracks;
        let owner_state = Rc::new(std::cell::RefCell::new(owner.clone()));
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 20);
        wrapper.add_css_class("route-content");
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let width = crate::route_layout::detail_route_inner_width_for_viewport(
            (self.route_width)(),
            PRIMARY_ROUTE_MARGIN_START,
        );
        let artwork = owner.artwork();
        let cover = self.artwork.cover_group_projection_for_artwork(
            &artwork,
            playlist_cover_size(width),
            playlist_cover_size(i32::MAX),
            false,
        );
        cover.widget().add_css_class("playlist-detail-cover");
        let showcase_view = DetailShowcaseView::new(
            "playlist-detail-showcase",
            owner.seed(),
            "Playlist",
            true,
            owner.name(),
        );
        self.append_playlist_detail_kind_controls(&showcase_view, &owner);
        showcase_view.replace_summary(&[
            (
                "rufin-tracks-symbolic",
                track_count_text(owner.track_count().max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((owner.duration_millis().max(0) / 1_000) as u32),
            ),
        ]);
        let actions = showcase_view.actions();
        actions.set_halign(gtk::Align::Start);
        let controls = detail_playback_controls(&actions, "Play playlist", None, Rc::clone(&play));
        if owner.summary.writable {
            let drop = gtk::DropTarget::new(
                glib::BoxedAnyObject::static_type(),
                gtk::gdk::DragAction::COPY,
            );
            let drop_owner = Rc::clone(&owner_state);
            let playlist = owner.summary.playlist_key;
            drop.set_preload(true);
            drop.connect_notify_local(Some("value"), move |drop, _| {
                let Some(value) = drop.value() else { return };
                if matches!(gtk_widgets::media_drag::media_drag_source(&value), Some(gtk_widgets::media_drag::MediaDragSource::PlaylistEntries(selection)) if selection.playlist == playlist && selection.writable && selection.count == 1) {
                    drop.reject();
                }
            });
            let drop_menus = Rc::downgrade(&self.media_menus);
            drop.connect_drop(move |_, value, _, _| {
                let Some(menus) = drop_menus.upgrade() else {
                    return false;
                };
                let Some(source) = gtk_widgets::media_drag::media_drag_source(value) else {
                    return false;
                };
                let summary = drop_owner.borrow().summary.clone();
                gtk_media_menus::media_menus::add_drag_to_playlist(
                    &menus,
                    summary.playlist_key,
                    summary.name,
                    source,
                )
            });
            wrapper.add_css_class("media-drop-target");
            wrapper.add_controller(drop);
        }

        let menu_shell = Rc::clone(self);
        let menu_owner = Rc::clone(&owner_state);
        let menu_play = Rc::clone(&play);
        let context_menu = Rc::new(move |target: &gtk::Widget, position| {
            menu_owner.borrow().install_context(
                &menu_shell,
                target,
                Rc::clone(&menu_play),
                position,
            );
        });
        let showcase = collection_detail_showcase(CollectionDetailShowcase {
            view: showcase_view.clone(),
            initial_width: width,
            compact_spacing: 20,
            wide_spacing: 28,
            cover: cover.clone(),
            cover_controls: controls,
            context_menu: Some(context_menu),
            open_cover: {
                let present = Rc::clone(&self.present_full_artwork);
                let owner = Rc::clone(&owner_state);
                Rc::new(move || {
                    let artwork = owner.borrow().artwork();
                    present(artwork);
                })
            },
        });
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&tracks_widget);

        let resume = {
            let shell = Rc::downgrade(self);
            let apply_list_settings = Rc::clone(&apply_list_settings);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                apply_list_settings(&settings);
            })
        };
        let refresh = {
            let shell = Rc::downgrade(self);
            let owner = Rc::clone(&owner_state);
            let showcase = showcase_view.clone();
            let cover = cover.clone();
            let apply_owner = Rc::clone(&owner);
            let apply = Rc::new(
                move |_: PlaylistDetailOwner, result: Result<PlaylistDetailOwner, String>| {
                    let Ok(next) = result else { return };
                    let Some(shell) = shell.upgrade() else { return };
                    refresh_tracks();
                    if *apply_owner.borrow() == next {
                        return;
                    }
                    showcase.set_title(next.name());
                    showcase.replace_summary(&[
                        (
                            "rufin-tracks-symbolic",
                            track_count_text(next.track_count().max(0) as u64),
                        ),
                        (
                            "rufin-preferences-system-time-symbolic",
                            format_duration_units((next.duration_millis().max(0) / 1_000) as u32),
                        ),
                    ]);
                    cover.replace(&shell.artwork, &next.artwork());
                    shell.append_playlist_detail_kind_controls(&showcase, &next);
                    apply_owner.replace(next);
                },
            );
            let database = Arc::clone(&self.library);
            let load = Arc::new(
                move |owner_key: PlaylistDetailOwner, cancellation: ReadCancellation| {
                    let database = Arc::clone(&database);
                    Box::pin(async move {
                        let key = owner_key.key;
                        database
                            .playlists(&[key], &cancellation)
                            .await
                            .map_err(|error| error.to_string())?
                            .pop()
                            .map(|summary| PlaylistDetailOwner { key, summary })
                            .ok_or_else(|| "Playlist no longer exists".to_string())
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
                },
            );
            let read = LatestMountedRouteRead::new_with_request(
                self.runtime.clone(),
                apply,
                load,
                "mounted Playlist summary",
            );
            Rc::new(move || {
                let request = owner.borrow().clone();
                read.request_with(request);
            }) as Rc<dyn Fn()>
        };

        let download_owner = Rc::clone(&owner_state);
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            let mut owner = download_owner.borrow_mut();
            if identity == format!("playlist:{}", owner.key) {
                owner.summary.downloaded_count = if downloaded {
                    owner.summary.track_count
                } else {
                    0
                };
            }
        });
        MountedRoute::new(wrapper.upcast(), resume)
            .with_download_change(downloads)
            .with_search(search)
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(item_navigation)
            .with_initial_demand(initial_demand)
            .with_catalog_refresh(refresh)
    }

    fn append_playlist_detail_kind_controls(
        self: &Rc<Self>,
        showcase: &DetailShowcaseView,
        owner: &PlaylistDetailOwner,
    ) {
        showcase.clear_kind_controls();
        let radio = detail_radio_button();
        let controller = self.playback.clone();
        let key = owner.key;
        radio.connect_clicked(move |_| {
            controller.play_radio(rufin_core::playback::RadioPlayRequest::now(
                library::RadioSeed::Playlist(key),
            ));
        });
        showcase.append_kind_control(&radio);
        showcase.set_genres(
            self,
            owner
                .summary
                .genres
                .iter()
                .map(|genre| (genre.name.as_str(), genre.genre_key)),
        );
    }
}

pub fn playlist_cover_size(width: i32) -> i32 {
    if width < DETAIL_SHOWCASE_METADATA_MIN_WIDTH {
        width.clamp(96, PLAYLIST_DETAIL_TINY_COVER_SIZE)
    } else if playlist_detail_compact_for_width(width) {
        PLAYLIST_DETAIL_TINY_COVER_SIZE
            + ((width - DETAIL_SHOWCASE_METADATA_MIN_WIDTH)
                * (PLAYLIST_DETAIL_WIDE_COVER_SIZE - PLAYLIST_DETAIL_TINY_COVER_SIZE)
                / (PLAYLIST_DETAIL_COMPACT_WIDTH - DETAIL_SHOWCASE_METADATA_MIN_WIDTH))
    } else {
        PLAYLIST_DETAIL_WIDE_COVER_SIZE
    }
}

pub fn playlist_detail_compact_for_width(width: i32) -> bool {
    width < PLAYLIST_DETAIL_COMPACT_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_cover_respects_checkpoint_breakpoints() {
        assert_eq!(playlist_cover_size(359), PLAYLIST_DETAIL_TINY_COVER_SIZE);
        assert_eq!(playlist_cover_size(420), PLAYLIST_DETAIL_TINY_COVER_SIZE);
        assert_eq!(playlist_cover_size(760), PLAYLIST_DETAIL_WIDE_COVER_SIZE);
    }

    #[test]
    fn mounted_detail_covers_track_width_without_resize_jumps() {
        let media_limit = crate::route_layout::detail_showcase_cover_size(i32::MAX);
        let collection_limit = playlist_cover_size(i32::MAX);
        let mut previous_media = crate::route_layout::detail_showcase_cover_size(96);
        let mut previous_collection = playlist_cover_size(96);
        for width in 97..=900 {
            let media = crate::route_layout::detail_showcase_cover_size(width);
            let collection = playlist_cover_size(width);
            assert!(media >= previous_media && media - previous_media <= 1);
            assert!(collection >= previous_collection && collection - previous_collection <= 1);
            assert!(media <= media_limit);
            assert!(collection <= collection_limit);
            assert!(media <= width);
            assert!(collection <= width);
            previous_media = media;
            previous_collection = collection;
        }
    }

    #[test]
    fn provider_playlist_cover_is_not_nested_inside_representative_mosaic() {
        let owner = PlaylistDetailOwner {
            key: PlaylistKey::from_raw(1),
            summary: PlaylistRow {
                source_owned: true,
                writable: true,
                metadata_writable: true,
                playlist_key: PlaylistKey::from_raw(1),
                source_key: Some(library::SourceKey::from_raw(1)),
                source_id: Some("source".into()),
                object_id: "playlist".to_string(),
                name: "Playlist".to_string(),
                artwork_binding: Some(vec![1]),
                track_count: 4,
                duration_millis: 1,
                downloaded_count: 0,
                genres: Vec::new(),
            },
        };

        assert_eq!(owner.artwork().len(), 1);
    }
}

use gtk_media_menus::media_menus::CollectionPlay;
