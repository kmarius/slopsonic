use std::sync::Arc;

use crate::route::{CollectionCategory, Route};
use crate::runtime::SelectedLibrary;
use crate::settings::{LibraryField, LibraryLayout, LibraryListKey, LibraryListSettings};
use library::{
    Collection, Database, ListKind, ListRead, MemberRead, MemberRow, Page, ReadCancellation,
};

#[derive(Clone, Debug)]
pub struct ArtistReleasePage {
    pub query: ListRead,
    pub counts: [usize; 6],
    pub source_counts: [usize; 6],
    pub page: Page<library::AlbumView>,
}

impl ArtistReleasePage {
    pub async fn load(
        database: &Database,
        query: ListRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Self> {
        let mut unfiltered = query.clone();
        unfiltered.filter.clear();
        if let ListKind::ArtistAlbums { filters, .. } = &mut unfiltered.kind {
            *filters = None;
        }
        let source_counts = unfiltered.artist_release_counts(database, cancel).await?;
        let filtered = match &query.kind {
            ListKind::ArtistAlbums {
                filters: Some(filters),
                ..
            } => filters.iter().any(|filter| !filter.trim().is_empty()),
            _ => !query.filter.trim().is_empty(),
        };
        let counts = if filtered {
            query.artist_release_counts(database, cancel).await?
        } else {
            source_counts
        };
        let range = window.range(counts.iter().sum());
        let first_rows = query
            .albums(database, range.start, range.len(), cancel)
            .await?;
        Ok(Self {
            query,
            counts,
            source_counts,
            page: Page {
                count: counts.iter().sum(),
                first_row_position: range.start,
                rows: first_rows,
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderEntry {
    pub object_id: String,
    pub name: String,
    pub artwork_binding: Option<Vec<u8>>,
}

impl crate::source::folders::FolderProjection {
    pub async fn child_page(
        &self,
        database: &Database,
        source: library::SourceKey,
        query: &MemberRead,
        range: std::ops::Range<usize>,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<(usize, Vec<FolderEntry>)> {
        Ok(match self {
            Self::Live { .. } => (
                self.live_folders(&query.filter, query.descending).count(),
                self.live_folders(&query.filter, query.descending)
                    .skip(range.start)
                    .take(range.len())
                    .map(|folder| FolderEntry {
                        object_id: folder.object_id.clone(),
                        name: folder.name.clone(),
                        artwork_binding: None,
                    })
                    .collect(),
            ),
            Self::Cached(folder) => {
                let (count, rows) = database
                    .filtered_folder_page(
                        source,
                        *folder,
                        &query.filter,
                        query.descending,
                        range.start,
                        range.len(),
                        cancel,
                    )
                    .await?;
                (
                    count,
                    rows.into_iter()
                        .map(|row| FolderEntry {
                            object_id: row.object_id,
                            name: row.metadata.name.unwrap_or_default(),
                            artwork_binding: row.artwork_binding,
                        })
                        .collect(),
                )
            }
        })
    }

    pub async fn sections(
        &self,
        database: &Database,
        source: library::SourceKey,
        query: &MemberRead,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Vec<library::ScrollSection>> {
        let mut sections = match self {
            Self::Live { .. } => {
                database
                    .ordered_name_sections(
                        self.live_folders(&query.filter, query.descending)
                            .map(|folder| folder.name.as_str()),
                        cancel,
                    )
                    .await?
            }
            Self::Cached(folder) => {
                database
                    .folder_section_positions(
                        source,
                        *folder,
                        &query.filter,
                        query.descending,
                        cancel,
                    )
                    .await?
            }
        };
        let folders = self
            .child_page(database, source, query, 0..0, cancel)
            .await?
            .0;
        for mut section in query.sections(database, cancel).await? {
            section.index += folders as u64;
            if let Some(existing) = sections
                .iter_mut()
                .find(|existing| existing.title == section.title)
            {
                existing.index = existing.index.min(section.index);
            } else {
                sections.push(section);
            }
        }
        sections.sort_by_key(|section| section.index);
        Ok(sections)
    }
}

pub fn library_list_key(route: &Route, category: CollectionCategory) -> LibraryListKey {
    match route {
        Route::Albums => LibraryListKey::Albums,
        Route::Artists => LibraryListKey::Artists,
        Route::AlbumArtists => LibraryListKey::AlbumArtists,
        Route::Genres => LibraryListKey::Genres,
        Route::Moods => LibraryListKey::Moods,
        Route::Playlists => LibraryListKey::Playlists,
        Route::PlaylistDetail(_) => LibraryListKey::PlaylistTracks,
        Route::AlbumDetail(_) => LibraryListKey::AlbumDetailTracks,
        Route::ArtistDiscography(_) | Route::AlbumArtistDiscography(_) => {
            LibraryListKey::ArtistAlbums
        }
        Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)
            if category == CollectionCategory::Albums =>
        {
            LibraryListKey::ArtistAlbums
        }
        Route::ArtistDetail(_)
        | Route::AlbumArtistDetail(_)
        | Route::ArtistTracks(_)
        | Route::AlbumArtistTracks(_)
        | Route::ArtistFavoriteTracks(_)
        | Route::AlbumArtistFavoriteTracks(_) => LibraryListKey::ArtistTracks,
        Route::GenreDetail(_) => LibraryListKey::GenreTracks,
        Route::MoodDetail(_) => LibraryListKey::MoodTracks,
        Route::Folders { .. } => LibraryListKey::Folders,
        Route::Favorites if category == CollectionCategory::Tracks => {
            LibraryListKey::FavoriteTracks
        }
        Route::Favorites => category.key(),
        _ => LibraryListKey::Tracks,
    }
}

#[derive(Clone)]
pub struct OpenOptions {
    pub category: CollectionCategory,
    pub filter: String,
    pub sort: Option<LibraryField>,
    pub descending: Option<bool>,
    pub favorites_only: bool,
    pub downloaded_only: bool,
    pub release_filters: Option<[String; 6]>,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            category: CollectionCategory::Tracks,
            filter: String::new(),
            sort: None,
            descending: None,
            favorites_only: false,
            downloaded_only: false,
            release_filters: None,
        }
    }
}

#[derive(Clone)]
pub struct MemberPage {
    pub read: MemberRead,
    pub page: Page<MemberRow>,
    pub disc_sections: Vec<(u32, i64)>,
}

impl MemberPage {
    pub async fn load(
        database: &Database,
        read: MemberRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Self> {
        let page = read.page(database, window, cancel).await?;
        let disc_sections = if matches!(read.collection, Collection::Album(_)) {
            read.disc_sections(database, cancel).await?
        } else {
            Vec::new()
        };
        Ok(Self {
            read,
            page,
            disc_sections,
        })
    }
}

#[derive(Clone)]
pub enum DetailSummary {
    Album(Box<library::AlbumView>),
    Artist(Box<library::ArtistView>),
    Playlist(Box<library::PlaylistRow>),
    Genre(Box<library::GenreView>),
    Mood(Box<library::MoodView>),
}

#[derive(Clone)]
pub enum DetailContent {
    Tracks(MemberPage),
    Releases {
        page: Box<ArtistReleasePage>,
        favorites: Option<MemberPage>,
    },
}

#[derive(Clone)]
pub struct DetailPage {
    pub summary: DetailSummary,
    pub content: DetailContent,
}

#[derive(Clone)]
pub struct FolderPage {
    pub projection: crate::source::folders::FolderProjection,
    pub read: MemberRead,
    pub folder_count: usize,
    pub track_count: usize,
    pub first_row_position: usize,
    pub folders: Vec<FolderEntry>,
    pub tracks: Vec<MemberRow>,
}

impl FolderPage {
    pub async fn load(
        database: &Database,
        source: library::SourceKey,
        projection: crate::source::folders::FolderProjection,
        read: MemberRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<Self> {
        let folder_count = projection
            .child_page(database, source, &read, 0..0, cancel)
            .await?
            .0;
        let track_count = read.count(database, cancel).await?;
        let range = window.range(folder_count + track_count);
        let folders = if range.start < folder_count {
            projection
                .child_page(database, source, &read, range.clone(), cancel)
                .await?
                .1
        } else {
            Vec::new()
        };
        let tracks = read
            .rows(
                database,
                range.start.saturating_sub(folder_count),
                range.len().saturating_sub(folders.len()),
                cancel,
            )
            .await?;
        Ok(Self {
            projection,
            read,
            folder_count,
            track_count,
            first_row_position: range.start,
            folders,
            tracks,
        })
    }
}

#[derive(Clone)]
pub enum AlbumListContent {
    Rows(Page<library::AlbumView>),
    Detail {
        index: Arc<library::AlbumDetailIndex>,
        page: Page<library::AlbumDetailRow>,
    },
}
#[derive(Clone)]
pub struct AlbumListPage {
    pub read: ListRead,
    pub content: AlbumListContent,
}

#[derive(Clone)]
pub enum OpenedCollection {
    Tracks(MemberPage),
    Detail(Option<DetailPage>),
    Albums(AlbumListPage),
    Artists {
        read: ListRead,
        page: Page<library::ArtistView>,
    },
    Genres {
        read: ListRead,
        page: Page<library::GenreView>,
    },
    Moods {
        read: ListRead,
        page: Page<library::MoodView>,
    },
    Playlists {
        read: ListRead,
        page: Page<library::PlaylistRow>,
    },
    Folders(FolderPage),
    Home(Box<library::HomePage>),
    Search(library::SearchResults),
}

pub(crate) struct PreparedAlbums {
    revision: crate::runtime::CatalogRevision,
    configuration: tokio::sync::watch::Receiver<()>,
    index: Arc<library::AlbumDetailIndex>,
}

pub fn list_display(captured: &ListRead, filter: &str, settings: &LibraryListSettings) -> ListRead {
    let mut read = captured.clone();
    read.filter = filter.trim().to_owned();
    read.sort = settings.sort_key.catalog_sort();
    read.descending = settings.descending;
    read.activity = match settings.layout {
        LibraryLayout::Grid => &settings.grid_fields,
        _ => &settings.row_fields,
    }
    .iter()
    .chain(if settings.layout == LibraryLayout::Detail {
        settings.detail_track_fields.as_slice()
    } else {
        &[]
    })
    .any(|field| field.catalog_sort().activity_sensitive());
    read
}

pub fn supports_favorite_filter(route: &Route, category: CollectionCategory) -> bool {
    matches!(
        route,
        Route::Tracks
            | Route::Albums
            | Route::Artists
            | Route::AlbumArtists
            | Route::AlbumDetail(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
    ) || matches!(route, Route::GenreDetail(_) | Route::MoodDetail(_))
        && category == CollectionCategory::Tracks
}

pub fn supports_downloaded_filter(route: &Route, category: CollectionCategory) -> bool {
    matches!(
        route,
        Route::Tracks
            | Route::AlbumDetail(_)
            | Route::ArtistTracks(_)
            | Route::AlbumArtistTracks(_)
            | Route::ArtistFavoriteTracks(_)
            | Route::AlbumArtistFavoriteTracks(_)
    ) || matches!(
        route,
        Route::Favorites | Route::GenreDetail(_) | Route::MoodDetail(_)
    ) && category == CollectionCategory::Tracks
}

struct ReadFlags {
    sort: library::CatalogSort,
    descending: bool,
    favorites_only: bool,
    activity: bool,
}

fn read_flags(route: &Route, options: &OpenOptions, display: &LibraryListSettings) -> ReadFlags {
    let sort = options.sort.unwrap_or(display.sort_key).catalog_sort();
    ReadFlags {
        sort,
        descending: options.descending.unwrap_or(display.descending),
        favorites_only: options.favorites_only && supports_favorite_filter(route, options.category)
            || matches!(
                route,
                Route::Favorites
                    | Route::ArtistFavoriteTracks(_)
                    | Route::AlbumArtistFavoriteTracks(_)
            ),
        activity: uses_activity(route, display) || sort.activity_sensitive(),
    }
}

fn member_read(
    route: &Route,
    options: &OpenOptions,
    display: &LibraryListSettings,
    folder: Option<library::FolderKey>,
    collection: Collection,
) -> MemberRead {
    let flags = read_flags(route, options, display);
    let mut read = MemberRead::new(collection);
    read.sort = Some(flags.sort);
    read.descending = flags.descending;
    read.folder = folder;
    read.filter.clone_from(&options.filter);
    read.favorites_only = flags.favorites_only;
    read.downloaded_only =
        options.downloaded_only && supports_downloaded_filter(route, options.category);
    read.activity = flags.activity;
    read
}

/// The read behind each plain list route: the one place that maps a route to its read.
enum ListPlan {
    Members(MemberRead),
    Albums(ListRead),
    Artists(ListRead),
    Genres(ListRead),
    Moods(ListRead),
    Playlists(ListRead),
}

/// `Ok(None)` for routes that are not plain lists (details, folders, home, search).
fn list_plan(
    route: &Route,
    selected: Option<&SelectedLibrary>,
    options: &OpenOptions,
    ui: &crate::settings::Settings,
) -> library::LibraryResult<Option<ListPlan>> {
    let display = ui.library_list(library_list_key(route, options.category));
    let folder = selected.and_then(|selected| selected.music_folder_key);
    let source = selected.map(|selected| selected.source_key);
    let require_source =
        || source.ok_or_else(|| library::LibraryError::InvalidRequest("No source selected".into()));
    let member = |collection| member_read(route, options, &display, folder, collection);
    let list = |kind| list_read(route, options, &display, folder, source, kind);
    let favorites_tab = |tab| *route == Route::Favorites && options.category == tab;
    Ok(Some(match route {
        Route::Tracks => ListPlan::Members(member(Collection::Source(require_source()?))),
        Route::Favorites if favorites_tab(CollectionCategory::Tracks) => {
            ListPlan::Members(member(Collection::Source(require_source()?)))
        }
        Route::Albums => {
            require_source()?;
            ListPlan::Albums(list(ListKind::Albums))
        }
        Route::Favorites if favorites_tab(CollectionCategory::Albums) => {
            require_source()?;
            ListPlan::Albums(list(ListKind::Albums))
        }
        Route::Artists | Route::AlbumArtists | Route::Favorites => {
            require_source()?;
            ListPlan::Artists(list(ListKind::Artists {
                album_artist: *route == Route::AlbumArtists,
            }))
        }
        Route::Genres => {
            require_source()?;
            ListPlan::Genres(list(ListKind::Genres))
        }
        Route::Moods => {
            require_source()?;
            ListPlan::Moods(list(ListKind::Moods))
        }
        Route::Playlists => ListPlan::Playlists(list(ListKind::Playlists)),
        _ => return Ok(None),
    }))
}

impl ListPlan {
    async fn load(
        self,
        owner: &crate::source::SourceOwner,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<OpenedCollection> {
        let database = &owner.shared.database;
        Ok(match self {
            Self::Members(read) => {
                OpenedCollection::Tracks(MemberPage::load(database, read, window, cancel).await?)
            }
            Self::Albums(read) => {
                OpenedCollection::Albums(owner.album_list_page(read, window, cancel).await?)
            }
            Self::Artists(read) => {
                let page = read.page_artists(database, window, cancel).await?;
                OpenedCollection::Artists { read, page }
            }
            Self::Genres(read) => {
                let page = read.page_genres(database, window, cancel).await?;
                OpenedCollection::Genres { read, page }
            }
            Self::Moods(read) => {
                let page = read.page_moods(database, window, cancel).await?;
                OpenedCollection::Moods { read, page }
            }
            Self::Playlists(read) => {
                let page = read.page_playlists(database, window, cancel).await?;
                OpenedCollection::Playlists { read, page }
            }
        })
    }

    fn empty(self, ui: &crate::settings::Settings) -> OpenedCollection {
        match self {
            Self::Members(read) => OpenedCollection::Tracks(MemberPage {
                read,
                page: empty_page(),
                disc_sections: Vec::new(),
            }),
            Self::Albums(read) => {
                let content =
                    if ui.library_list(LibraryListKey::Albums).layout == LibraryLayout::Detail {
                        AlbumListContent::Detail {
                            index: Arc::new(library::AlbumDetailIndex::empty(read.clone())),
                            page: empty_page(),
                        }
                    } else {
                        AlbumListContent::Rows(empty_page())
                    };
                OpenedCollection::Albums(AlbumListPage { read, content })
            }
            Self::Artists(read) => OpenedCollection::Artists {
                read,
                page: empty_page(),
            },
            Self::Genres(read) => OpenedCollection::Genres {
                read,
                page: empty_page(),
            },
            Self::Moods(read) => OpenedCollection::Moods {
                read,
                page: empty_page(),
            },
            Self::Playlists(read) => OpenedCollection::Playlists {
                read,
                page: empty_page(),
            },
        }
    }
}

fn empty_page<T>() -> Page<T> {
    Page {
        count: 0,
        first_row_position: 0,
        rows: Vec::new(),
    }
}

fn list_read(
    route: &Route,
    options: &OpenOptions,
    display: &LibraryListSettings,
    folder: Option<library::FolderKey>,
    source: Option<library::SourceKey>,
    kind: ListKind,
) -> ListRead {
    let flags = read_flags(route, options, display);
    let mut read = ListRead::new(kind, source, flags.sort);
    read.folder = folder;
    read.filter.clone_from(&options.filter);
    read.descending = flags.descending;
    read.favorites_only = flags.favorites_only;
    read.activity = flags.activity;
    read
}

pub fn uses_activity(route: &Route, settings: &LibraryListSettings) -> bool {
    matches!(route, Route::Home)
        || settings.sort_key.catalog_sort().activity_sensitive()
        || match settings.layout {
            LibraryLayout::Grid => &settings.grid_fields,
            _ => &settings.row_fields,
        }
        .iter()
        .chain(if settings.layout == LibraryLayout::Detail {
            settings.detail_track_fields.as_slice()
        } else {
            &[]
        })
        .any(|field| field.catalog_sort().activity_sensitive())
}

impl crate::source::SourceOwner {
    pub async fn album_list_page(
        &self,
        read: ListRead,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<AlbumListPage> {
        if self
            .shared
            .settings
            .load()
            .ui
            .library_list(LibraryListKey::Albums)
            .layout
            != LibraryLayout::Detail
        {
            return Ok(AlbumListPage {
                content: AlbumListContent::Rows(
                    read.page_albums(&self.shared.database, window, cancel)
                        .await?,
                ),
                read,
            });
        }
        let mut retained = tokio::select! {
            retained = self.shared.prepared_albums.lock() => retained,
            () = cancel.cancelled() => return Err(library::LibraryError::ReadCancelled),
        };
        let revision = self.catalog_changes().borrow().clone();
        let index = if let Some(prepared) = retained.as_ref().filter(|prepared| {
            prepared.index.matches(&read)
                && !prepared.configuration.has_changed().unwrap_or(true)
                && prepared.revision.catalog == revision.catalog
                && (!read.sort.activity_sensitive()
                    || prepared.revision.activity == revision.activity)
        }) {
            Arc::clone(&prepared.index)
        } else {
            let index = read
                .album_detail_index(&self.shared.database, cancel)
                .await?;
            *retained = Some(PreparedAlbums {
                revision,
                configuration: self.configuration_changes(),
                index: Arc::clone(&index),
            });
            index
        };
        drop(retained);
        let page = index.page(&self.shared.database, window, cancel).await?;
        Ok(AlbumListPage {
            read,
            content: AlbumListContent::Detail { index, page },
        })
    }

    /// The route's page with no rows yet, so it can mount before the first page loads.
    /// Built from the same `list_plan` as `open_collection`, so the later fill matches it.
    pub fn pending_collection(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        category: CollectionCategory,
    ) -> Option<OpenedCollection> {
        let options = OpenOptions {
            category,
            ..OpenOptions::default()
        };
        let ui = self.shared.settings.load().ui;
        let plan = list_plan(route, selected, &options, &ui).ok().flatten()?;
        Some(plan.empty(&ui))
    }

    pub async fn open_collection(
        &self,
        route: &Route,
        selected: Option<&SelectedLibrary>,
        options: &OpenOptions,
        window: library::RouteSeedWindow,
        cancel: &ReadCancellation,
    ) -> library::LibraryResult<OpenedCollection> {
        let database = &self.shared.database;
        let settings = self.shared.settings.load();
        let key = library_list_key(route, options.category);
        let display = settings.ui.library_list(key);
        let sort = options.sort.unwrap_or(display.sort_key).catalog_sort();
        let descending = options.descending.unwrap_or(display.descending);
        let activity = uses_activity(route, &display) || sort.activity_sensitive();
        let folder = selected.and_then(|selected| selected.music_folder_key);
        let source = selected.map(|selected| selected.source_key);
        let require_source = || {
            source.ok_or_else(|| library::LibraryError::InvalidRequest("No source selected".into()))
        };
        if let Some(plan) = list_plan(route, selected, options, &settings.ui)? {
            return plan.load(self, window, cancel).await;
        }
        let member = |collection| member_read(route, options, &display, folder, collection);
        let list = |kind| list_read(route, options, &display, folder, source, kind);
        let summary_and_members = match route {
            Route::AlbumDetail(uri) => database
                .album(uri, activity, cancel)
                .await?
                .filter(|row| row.summary.track_count > 0)
                .map(|row| {
                    let read = member(Collection::Album(row.album_key));
                    (DetailSummary::Album(Box::new(row)), read)
                }),
            Route::ArtistDetail(uri)
            | Route::AlbumArtistDetail(uri)
            | Route::ArtistDiscography(uri)
            | Route::AlbumArtistDiscography(uri)
            | Route::ArtistTracks(uri)
            | Route::AlbumArtistTracks(uri)
            | Route::ArtistFavoriteTracks(uri)
            | Route::AlbumArtistFavoriteTracks(uri) => {
                let role = matches!(
                    route,
                    Route::AlbumArtistDetail(_)
                        | Route::AlbumArtistDiscography(_)
                        | Route::AlbumArtistTracks(_)
                        | Route::AlbumArtistFavoriteTracks(_)
                );
                let Some(row) = database.artist(uri, role, activity, cancel).await? else {
                    return Ok(OpenedCollection::Detail(None));
                };
                let read = member(Collection::Artist {
                    key: row.artist_key,
                    album_artist: role,
                });
                if matches!(
                    route,
                    Route::ArtistDetail(_)
                        | Route::AlbumArtistDetail(_)
                        | Route::ArtistDiscography(_)
                        | Route::AlbumArtistDiscography(_)
                ) {
                    let mut query = list(ListKind::ArtistAlbums {
                        artist: row.artist_key,
                        album_artist: role,
                        release_class: None,
                        filters: options.release_filters.as_ref().map(|filters| {
                            Arc::new(std::array::from_fn(|index| filters[index].clone()))
                        }),
                    });
                    query.source = Some(row.source_key);
                    if options.sort.is_none() {
                        let albums = settings.ui.library_list(LibraryListKey::ArtistAlbums);
                        query.sort = albums.sort_key.catalog_sort();
                        query.descending = options.descending.unwrap_or(albums.descending);
                    }
                    let page =
                        Box::new(ArtistReleasePage::load(database, query, window, cancel).await?);
                    let favorites =
                        if matches!(route, Route::ArtistDetail(_) | Route::AlbumArtistDetail(_)) {
                            let tracks = settings.ui.library_list(LibraryListKey::ArtistTracks);
                            let mut read = read;
                            read.sort = Some(tracks.sort_key.catalog_sort());
                            read.descending = tracks.descending;
                            read.favorites_only = true;
                            Some(
                                MemberPage::load(
                                    database,
                                    read,
                                    library::RouteSeedWindow::top(),
                                    cancel,
                                )
                                .await?,
                            )
                        } else {
                            None
                        };
                    return Ok(OpenedCollection::Detail(Some(DetailPage {
                        summary: DetailSummary::Artist(Box::new(row)),
                        content: DetailContent::Releases { page, favorites },
                    })));
                }
                Some((DetailSummary::Artist(Box::new(row)), read))
            }
            Route::PlaylistDetail(key) => {
                database.playlists(&[*key], cancel).await?.pop().map(|row| {
                    (
                        DetailSummary::Playlist(Box::new(row)),
                        member(Collection::Playlist(*key)),
                    )
                })
            }
            Route::GenreDetail(key) => database
                .genres(&[*key], folder, cancel)
                .await?
                .pop()
                .filter(|row| row.summary.track_count > 0)
                .map(|row| {
                    (
                        DetailSummary::Genre(Box::new(row)),
                        member(Collection::Genre(*key)),
                    )
                }),
            Route::MoodDetail(key) => database
                .moods(&[*key], folder, cancel)
                .await?
                .pop()
                .filter(|row| row.summary.track_count > 0)
                .map(|row| {
                    (
                        DetailSummary::Mood(Box::new(row)),
                        member(Collection::Mood(*key)),
                    )
                }),
            _ => None,
        };
        if let Some((summary, read)) = summary_and_members {
            let page = if read.filter.trim().is_empty()
                && !read.favorites_only
                && !read.downloaded_only
                && (read.folder.is_none()
                    || matches!(&summary, DetailSummary::Playlist(row) if row.source_key.is_none()))
            {
                let count = match &summary {
                    DetailSummary::Album(row) => row.summary.track_count,
                    DetailSummary::Artist(row) => row.summary.track_count,
                    DetailSummary::Genre(row) => row.summary.track_count,
                    DetailSummary::Mood(row) => row.summary.track_count,
                    DetailSummary::Playlist(row) => row.track_count,
                }
                .max(0) as usize;
                let range = window.range(count);
                let rows = read
                    .rows(database, range.start, range.len(), cancel)
                    .await?;
                let disc_sections = if matches!(read.collection, Collection::Album(_)) {
                    read.disc_sections(database, cancel).await?
                } else {
                    Vec::new()
                };
                MemberPage {
                    read,
                    page: Page {
                        count,
                        first_row_position: range.start,
                        rows,
                    },
                    disc_sections,
                }
            } else {
                MemberPage::load(database, read, window, cancel).await?
            };
            return Ok(OpenedCollection::Detail(Some(DetailPage {
                summary,
                content: DetailContent::Tracks(page),
            })));
        }
        Ok(match route {
            Route::AlbumDetail(_)
            | Route::PlaylistDetail(_)
            | Route::GenreDetail(_)
            | Route::MoodDetail(_) => OpenedCollection::Detail(None),
            Route::Folders { path } => {
                let selected = selected.ok_or_else(|| {
                    library::LibraryError::InvalidRequest("No source selected".into())
                })?;
                let projection = crate::source::folders::resolve_folder(
                    self,
                    database,
                    selected,
                    path.last().map(|item| item.id.as_str()),
                    cancel,
                )
                .await?;
                let mut read = projection.members(selected.source_key);
                read.sort = Some(sort);
                read.descending = descending;
                read.filter.clone_from(&options.filter);
                read.activity = activity;
                OpenedCollection::Folders(
                    FolderPage::load(
                        database,
                        selected.source_key,
                        projection,
                        read,
                        window,
                        cancel,
                    )
                    .await?,
                )
            }
            Route::Home => {
                let (showcase, explore) = self.home_variations();
                OpenedCollection::Home(Box::new(
                    database
                        .home_page(
                            require_source()?,
                            folder,
                            showcase,
                            explore,
                            &settings.ui.application.home_blocks,
                            cancel,
                        )
                        .await?,
                ))
            }
            Route::Search => OpenedCollection::Search(
                crate::source::search::acquire_search(
                    self,
                    database,
                    selected,
                    options.filter.clone(),
                    60,
                    cancel.clone(),
                )
                .await
                .map_err(library::LibraryError::InvalidRequest)?,
            ),
            _ => unreachable!("collection route was handled above"),
        })
    }
}

#[cfg(test)]
mod list_plan_tests {
    use super::*;
    use crate::settings::Settings;
    use crate::settings::layout::LibraryListSettingsEntry;

    fn selected() -> SelectedLibrary {
        SelectedLibrary {
            source_id: sources::SourceId::new("server"),
            source_key: library::SourceKey::from_raw(1),
            music_folder_key: None,
            music_folder_object_id: None,
            music_folders: Arc::from([]),
        }
    }

    fn plan(route: Route, category: CollectionCategory) -> Option<ListPlan> {
        let options = OpenOptions {
            category,
            ..OpenOptions::default()
        };
        list_plan(&route, Some(&selected()), &options, &Settings::default()).unwrap()
    }

    #[test]
    fn only_plain_list_routes_have_a_plan() {
        for route in [
            Route::Tracks,
            Route::Albums,
            Route::Artists,
            Route::AlbumArtists,
            Route::Genres,
            Route::Moods,
            Route::Playlists,
        ] {
            assert!(
                plan(route.clone(), CollectionCategory::Tracks).is_some(),
                "{route:?}"
            );
        }
        for route in [
            Route::Home,
            Route::Search,
            Route::Folders { path: Vec::new() },
        ] {
            assert!(
                plan(route.clone(), CollectionCategory::Tracks).is_none(),
                "{route:?}"
            );
        }
    }

    #[test]
    fn favorites_tabs_read_their_own_list_filtered_to_favorites() {
        let favorites = |category| plan(Route::Favorites, category).unwrap();
        assert!(matches!(
            favorites(CollectionCategory::Tracks),
            ListPlan::Members(read) if read.favorites_only
        ));
        assert!(matches!(
            favorites(CollectionCategory::Albums),
            ListPlan::Albums(read) if read.favorites_only
        ));
        assert!(matches!(
            favorites(CollectionCategory::Artists),
            ListPlan::Artists(read) if read.favorites_only
        ));
    }

    #[test]
    fn source_lists_need_a_source_but_playlists_do_not() {
        let options = OpenOptions::default();
        let ui = Settings::default();
        assert!(list_plan(&Route::Tracks, None, &options, &ui).is_err());
        assert!(list_plan(&Route::Albums, None, &options, &ui).is_err());
        assert!(matches!(
            list_plan(&Route::Playlists, None, &options, &ui),
            Ok(Some(_))
        ));
    }

    #[test]
    fn pending_albums_take_the_saved_layout_shape() {
        let mut ui = Settings::default();
        let rows = plan(Route::Albums, CollectionCategory::Albums)
            .unwrap()
            .empty(&ui);
        assert!(matches!(
            rows,
            OpenedCollection::Albums(AlbumListPage {
                content: AlbumListContent::Rows(_),
                ..
            })
        ));
        let mut detail = LibraryListSettings::for_key(LibraryListKey::Albums);
        detail.layout = LibraryLayout::Detail;
        ui.library_lists
            .retain(|entry| entry.key != LibraryListKey::Albums);
        ui.library_lists.push(LibraryListSettingsEntry {
            key: LibraryListKey::Albums,
            settings: detail,
        });
        let detail = plan(Route::Albums, CollectionCategory::Albums)
            .unwrap()
            .empty(&ui);
        assert!(matches!(
            detail,
            OpenedCollection::Albums(AlbumListPage {
                content: AlbumListContent::Detail { .. },
                ..
            })
        ));
    }
}
