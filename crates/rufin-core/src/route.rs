pub mod detail_links;

use library::{GenreKey, MoodKey, PlaylistKey};
use serde::{Deserialize, Serialize};

use crate::settings::SidebarRouteItem;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct FolderPathItem {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CollectionCategory {
    #[default]
    Tracks,
    Albums,
    Artists,
}

impl CollectionCategory {
    pub const ALL: [Self; 3] = [Self::Tracks, Self::Albums, Self::Artists];

    pub const fn next(self) -> Self {
        match self {
            Self::Tracks => Self::Albums,
            Self::Albums => Self::Artists,
            Self::Artists => Self::Tracks,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum Route {
    Home,
    Search,
    Favorites,
    History,
    Albums,
    AlbumDetail(String),
    Tracks,
    Artists,
    ArtistDetail(String),
    ArtistDiscography(String),
    ArtistTracks(String),
    ArtistFavoriteTracks(String),
    AlbumArtists,
    AlbumArtistDetail(String),
    AlbumArtistDiscography(String),
    AlbumArtistTracks(String),
    AlbumArtistFavoriteTracks(String),
    Genres,
    GenreDetail(GenreKey),
    Moods,
    MoodDetail(MoodKey),
    Folders { path: Vec<FolderPathItem> },
    Playlists,
    PlaylistDetail(PlaylistKey),
    PlaylistFolder { key: library::PlaylistFolderKey },
}

impl Route {
    pub fn requires_selected_source(&self) -> bool {
        matches!(
            self,
            Self::Home
                | Self::Search
                | Self::Favorites
                | Self::Albums
                | Self::Tracks
                | Self::Artists
                | Self::AlbumArtists
                | Self::Genres
                | Self::Moods
                | Self::Folders { .. }
        )
    }
    pub fn uses_selected_source(&self) -> bool {
        self.requires_selected_source()
            || matches!(self, Self::Playlists | Self::PlaylistFolder { .. })
    }
}

pub struct SidebarRouteDescriptor {
    pub stable_id: &'static str,
    pub title: &'static str,
    pub icon_name: &'static str,
    selected_icon_name: Option<&'static str>,
    pub root_route: Route,
}

impl SidebarRouteDescriptor {
    fn new(
        stable_id: &'static str,
        title: &'static str,
        icon_name: &'static str,
        selected_icon_name: Option<&'static str>,
        root_route: Route,
    ) -> Self {
        Self {
            stable_id,
            title,
            icon_name,
            selected_icon_name,
            root_route,
        }
    }

    pub fn selected_icon_name(&self) -> &'static str {
        self.selected_icon_name.unwrap_or(self.icon_name)
    }
}

#[rustfmt::skip]
    pub fn sidebar_route_descriptor(item: SidebarRouteItem) -> SidebarRouteDescriptor {
        match item {
            SidebarRouteItem::Home => SidebarRouteDescriptor::new("Home", "Home", "rufin-home-symbolic", None, Route::Home),
            SidebarRouteItem::Search => SidebarRouteDescriptor::new("Search", "Search", "rufin-search-symbolic", None, Route::Search),
            SidebarRouteItem::Favorites => SidebarRouteDescriptor::new("Favorites", "Favorites", "rufin-heart-outline-symbolic", Some("rufin-heart-filled-symbolic"), Route::Favorites),
            SidebarRouteItem::Albums => SidebarRouteDescriptor::new("Albums", "Albums", "rufin-albums-symbolic", None, Route::Albums),
            SidebarRouteItem::Tracks => SidebarRouteDescriptor::new("Tracks", "Tracks", "rufin-tracks-symbolic", None, Route::Tracks),
            SidebarRouteItem::Artists => SidebarRouteDescriptor::new("Artists", "Artists", "rufin-artists-symbolic", None, Route::Artists),
            SidebarRouteItem::AlbumArtists => SidebarRouteDescriptor::new("AlbumArtists", "Album Artists", "rufin-album-artists-symbolic", None, Route::AlbumArtists),
            SidebarRouteItem::Genres => SidebarRouteDescriptor::new("Genres", "Genres", "rufin-tag-outline-symbolic", None, Route::Genres),
            SidebarRouteItem::Moods => SidebarRouteDescriptor::new("Moods", "Moods", "rufin-moods-symbolic", None, Route::Moods),
            SidebarRouteItem::History => SidebarRouteDescriptor::new("History", "History", "rufin-history-symbolic", None, Route::History),
            SidebarRouteItem::Folders => SidebarRouteDescriptor::new("Folders", "Folders", "rufin-folders-symbolic", Some("rufin-folders-selected-symbolic"), Route::Folders { path: Vec::new() }),
            SidebarRouteItem::Playlists => SidebarRouteDescriptor::new("Playlists", "Playlists", "rufin-playlists-symbolic", None, Route::Playlists),
        }
    }

use crate::settings::layout::{LibraryField, LibraryListKey};
impl CollectionCategory {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tracks => "tracks",
            Self::Albums => "albums",
            Self::Artists => "artists",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "albums" => Self::Albums,
            "artists" => Self::Artists,
            _ => Self::Tracks,
        }
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::Tracks => "Tracks",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
        }
    }

    pub const fn icon_name(self) -> &'static str {
        match self {
            Self::Tracks => "rufin-tracks-symbolic",
            Self::Albums => "rufin-albums-symbolic",
            Self::Artists => "rufin-artists-symbolic",
        }
    }

    pub const fn key(self) -> LibraryListKey {
        match self {
            Self::Tracks => LibraryListKey::Tracks,
            Self::Albums => LibraryListKey::Albums,
            Self::Artists => LibraryListKey::Artists,
        }
    }

    pub fn sort_fields(self) -> &'static [LibraryField] {
        match self {
            Self::Tracks => crate::settings::available_sort_fields(LibraryListKey::Tracks),
            Self::Albums => &[
                LibraryField::Title,
                LibraryField::AlbumArtist,
                LibraryField::Year,
                LibraryField::ReleaseDate,
                LibraryField::DateAdded,
                LibraryField::LastPlayed,
                LibraryField::PlayCount,
                LibraryField::UserRating,
                LibraryField::Favorite,
            ],
            Self::Artists => &[
                LibraryField::Title,
                LibraryField::LastPlayed,
                LibraryField::PlayCount,
                LibraryField::UserRating,
                LibraryField::Favorite,
            ],
        }
    }
}
