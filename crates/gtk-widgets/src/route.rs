pub use slopsonic_core::route::{
    CollectionCategory, FolderPathItem, Route, SidebarRouteDescriptor, sidebar_route_descriptor,
};

pub fn sidebar_route_css_class(item: slopsonic_core::settings::SidebarRouteItem) -> &'static str {
    use slopsonic_core::settings::SidebarRouteItem;
    match item {
        SidebarRouteItem::Home => "nav-route-home",
        SidebarRouteItem::Search => "nav-route-search",
        SidebarRouteItem::Favorites => "nav-route-favorites",
        SidebarRouteItem::Albums => "nav-route-albums",
        SidebarRouteItem::Tracks => "nav-route-tracks",
        SidebarRouteItem::Artists => "nav-route-artists",
        SidebarRouteItem::AlbumArtists => "nav-route-album-artists",
        SidebarRouteItem::Genres => "nav-route-genres",
        SidebarRouteItem::Moods => "nav-route-moods",
        SidebarRouteItem::Folders => "nav-route-folders",
        SidebarRouteItem::Playlists => "nav-route-playlists",
    }
}
