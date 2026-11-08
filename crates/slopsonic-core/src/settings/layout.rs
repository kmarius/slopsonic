use serde::{Deserialize, Deserializer, Serialize};

pub const DEFAULT_WINDOW_WIDTH: i32 = 1_500;
pub const DEFAULT_WINDOW_HEIGHT: i32 = 900;
pub const MIN_RESTORED_WINDOW_WIDTH: i32 = 450;
pub const MIN_RESTORED_WINDOW_HEIGHT: i32 = 400;
pub const MAX_RESTORED_WINDOW_WIDTH: i32 = 3_400;
pub const MAX_RESTORED_WINDOW_HEIGHT: i32 = 2_000;
fn default_narrow_layout_enabled() -> bool {
    true
}
fn default_narrow_layout_threshold() -> i32 {
    1_300
}
pub fn sanitized_window_size(width: Option<i32>, height: Option<i32>) -> Option<(i32, i32)> {
    let (width, height) = (width?, height?);
    if width < MIN_RESTORED_WINDOW_WIDTH || height < MIN_RESTORED_WINDOW_HEIGHT {
        return None;
    }
    Some((
        width.clamp(MIN_RESTORED_WINDOW_WIDTH, MAX_RESTORED_WINDOW_WIDTH),
        height.clamp(MIN_RESTORED_WINDOW_HEIGHT, MAX_RESTORED_WINDOW_HEIGHT),
    ))
}

pub const MIN_NARROW_LAYOUT_THRESHOLD: i32 = 700;
pub const MAX_NARROW_LAYOUT_THRESHOLD: i32 = 3_400;
pub const DEFAULT_LEFT_SIDEBAR_WIDTH: i32 = 230;
pub const MIN_LEFT_SIDEBAR_WIDTH: i32 = 210;
pub const MAX_LEFT_SIDEBAR_WIDTH: i32 = 400;
pub const DEFAULT_RIGHT_SIDEBAR_WIDTH: i32 = 300;
pub const MIN_RIGHT_SIDEBAR_WIDTH: i32 = 250;
pub const MAX_RIGHT_SIDEBAR_WIDTH: i32 = 500;
pub const MIN_TABLE_COLUMN_WIDTH: i32 = 24;
pub const MAX_TABLE_COLUMN_WIDTH: i32 = 4_096;
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum LeftSidebarMode {
    #[default]
    Full,
    Compact,
    Hidden,
}
impl<'de> Deserialize<'de> for LeftSidebarMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "Full" => Self::Full,
            "Compact" => Self::Compact,
            "Hidden" => Self::Hidden,
            _ => Self::default(),
        })
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum RightSidebarMode {
    Hidden,
    #[default]
    Visible,
}
impl RightSidebarMode {
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LayoutProfile {
    #[serde(default)]
    pub left_sidebar: LeftSidebarMode,
    #[serde(default)]
    pub right_sidebar: RightSidebarMode,
}
impl LayoutProfile {
    pub fn new(left_sidebar: LeftSidebarMode, right_sidebar: RightSidebarMode) -> Self {
        Self {
            left_sidebar,
            right_sidebar,
        }
    }
}
impl Default for LayoutProfile {
    fn default() -> Self {
        Self::new(LeftSidebarMode::Full, RightSidebarMode::Visible)
    }
}
fn default_narrow_layout_profile() -> LayoutProfile {
    LayoutProfile::new(LeftSidebarMode::Compact, RightSidebarMode::Visible)
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LayoutSettings {
    #[serde(default)]
    pub default_profile: LayoutProfile,
    #[serde(default = "default_narrow_layout_enabled")]
    pub narrow_enabled: bool,
    #[serde(default = "default_narrow_layout_threshold")]
    pub narrow_threshold: i32,
    #[serde(default = "default_narrow_layout_profile")]
    pub narrow_profile: LayoutProfile,
    pub preferred_left_sidebar_width: i32,
    pub preferred_right_sidebar_width: i32,
}
impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            default_profile: LayoutProfile::default(),
            narrow_enabled: true,
            narrow_threshold: default_narrow_layout_threshold(),
            narrow_profile: default_narrow_layout_profile(),
            preferred_left_sidebar_width: DEFAULT_LEFT_SIDEBAR_WIDTH,
            preferred_right_sidebar_width: DEFAULT_RIGHT_SIDEBAR_WIDTH,
        }
    }
}
impl LayoutSettings {
    pub fn sanitize(&mut self) {
        self.narrow_threshold = self
            .narrow_threshold
            .clamp(MIN_NARROW_LAYOUT_THRESHOLD, MAX_NARROW_LAYOUT_THRESHOLD);
        self.preferred_left_sidebar_width = self
            .preferred_left_sidebar_width
            .clamp(MIN_LEFT_SIDEBAR_WIDTH, MAX_LEFT_SIDEBAR_WIDTH);
        self.preferred_right_sidebar_width = self
            .preferred_right_sidebar_width
            .clamp(MIN_RIGHT_SIDEBAR_WIDTH, MAX_RIGHT_SIDEBAR_WIDTH);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum StoredRightSidebarMode {
    Hidden,
    Compact,
    #[default]
    Default,
    Comfortable,
    Spacious,
}

impl StoredRightSidebarMode {
    fn presentation(self) -> RightSidebarMode {
        if matches!(self, Self::Hidden) {
            RightSidebarMode::Hidden
        } else {
            RightSidebarMode::Visible
        }
    }
}

impl<'de> Deserialize<'de> for StoredRightSidebarMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "Hidden" => Self::Hidden,
            "Compact" => Self::Compact,
            "Comfortable" => Self::Comfortable,
            "Spacious" => Self::Spacious,
            "Visible" | "Shown" | "Default" => Self::Default,
            _ => Self::default(),
        })
    }
}

#[derive(Deserialize)]
struct StoredLayoutProfile {
    #[serde(default)]
    left_sidebar: LeftSidebarMode,
    #[serde(default)]
    right_sidebar: StoredRightSidebarMode,
}

impl Default for StoredLayoutProfile {
    fn default() -> Self {
        Self {
            left_sidebar: LeftSidebarMode::Full,
            right_sidebar: StoredRightSidebarMode::Default,
        }
    }
}

fn default_stored_narrow_layout_profile() -> StoredLayoutProfile {
    StoredLayoutProfile {
        left_sidebar: LeftSidebarMode::Compact,
        ..StoredLayoutProfile::default()
    }
}

#[derive(Deserialize)]
struct StoredLayoutSettings {
    #[serde(default)]
    default_profile: StoredLayoutProfile,
    #[serde(default = "default_narrow_layout_enabled")]
    narrow_enabled: bool,
    #[serde(default = "default_narrow_layout_threshold")]
    narrow_threshold: i32,
    #[serde(default = "default_stored_narrow_layout_profile")]
    narrow_profile: StoredLayoutProfile,
    #[serde(default)]
    preferred_left_sidebar_width: Option<i32>,
    #[serde(default)]
    preferred_right_sidebar_width: Option<i32>,
}

impl<'de> Deserialize<'de> for LayoutSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let stored = StoredLayoutSettings::deserialize(deserializer)?;
        let mut settings = Self {
            default_profile: LayoutProfile::new(
                stored.default_profile.left_sidebar,
                stored.default_profile.right_sidebar.presentation(),
            ),
            narrow_enabled: stored.narrow_enabled,
            narrow_threshold: stored.narrow_threshold,
            narrow_profile: LayoutProfile::new(
                stored.narrow_profile.left_sidebar,
                stored.narrow_profile.right_sidebar.presentation(),
            ),
            preferred_left_sidebar_width: stored
                .preferred_left_sidebar_width
                .unwrap_or(DEFAULT_LEFT_SIDEBAR_WIDTH),
            preferred_right_sidebar_width: stored
                .preferred_right_sidebar_width
                .unwrap_or(DEFAULT_RIGHT_SIDEBAR_WIDTH),
        };
        settings.sanitize();
        Ok(settings)
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(from = "StoredThemePreference")]
pub enum ThemePreference {
    System,
    Light,
    Dark,
}
#[derive(Deserialize)]
enum StoredThemePreference {
    System,
    Light,
    Dark,
    Named(serde::de::IgnoredAny),
}
impl From<StoredThemePreference> for ThemePreference {
    fn from(stored: StoredThemePreference) -> Self {
        match stored {
            StoredThemePreference::Light => Self::Light,
            StoredThemePreference::Dark => Self::Dark,
            StoredThemePreference::System | StoredThemePreference::Named(_) => Self::System,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum AccentPreference {
    #[default]
    System,
    Blue,
    Teal,
    Green,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Slate,
}
impl AccentPreference {
    pub fn color(self) -> Option<&'static str> {
        match self {
            Self::System => None,
            Self::Blue => Some("#3584e4"),
            Self::Teal => Some("#2190a4"),
            Self::Green => Some("#3a944a"),
            Self::Yellow => Some("#c88800"),
            Self::Orange => Some("#ed5b00"),
            Self::Red => Some("#e62d42"),
            Self::Pink => Some("#d56199"),
            Self::Purple => Some("#9141ac"),
            Self::Slate => Some("#6f8396"),
        }
    }
    pub const ALL: [Self; 10] = [
        Self::System,
        Self::Blue,
        Self::Teal,
        Self::Green,
        Self::Yellow,
        Self::Orange,
        Self::Red,
        Self::Pink,
        Self::Purple,
        Self::Slate,
    ];
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
pub enum LibraryLayout {
    Row,
    Grid,
    Detail,
}
impl<'de> Deserialize<'de> for LibraryLayout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "Row" | "row" | "Table" | "table" => Self::Row,
            "Detail" | "detail" => Self::Detail,
            "Grid" | "grid" => Self::Grid,
            _ => Self::Grid,
        })
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LibraryListKey {
    Albums,
    Artists,
    AlbumArtists,
    Tracks,
    Folders,
    FavoriteTracks,
    Genres,
    Moods,
    Playlists,
    AlbumDetailTracks,
    ArtistAlbums,
    ArtistTracks,
    GenreTracks,
    MoodTracks,
    PlaylistTracks,
    Queue,
    FullscreenTracks,
}
impl LibraryListKey {
    pub fn all() -> [Self; 17] {
        [
            Self::Albums,
            Self::Artists,
            Self::AlbumArtists,
            Self::Tracks,
            Self::Folders,
            Self::FavoriteTracks,
            Self::Genres,
            Self::Moods,
            Self::Playlists,
            Self::AlbumDetailTracks,
            Self::ArtistAlbums,
            Self::ArtistTracks,
            Self::GenreTracks,
            Self::MoodTracks,
            Self::PlaylistTracks,
            Self::Queue,
            Self::FullscreenTracks,
        ]
    }

    pub fn supports_layout(self, layout: LibraryLayout) -> bool {
        match layout {
            LibraryLayout::Detail => matches!(self, Self::Albums),
            LibraryLayout::Grid => !matches!(
                self,
                Self::AlbumDetailTracks | Self::Queue | Self::FullscreenTracks | Self::Folders
            ),
            LibraryLayout::Row => true,
        }
    }

    fn default_layout(self) -> LibraryLayout {
        match self {
            Self::Albums => LibraryLayout::Grid,
            Self::Queue
            | Self::FullscreenTracks
            | Self::Tracks
            | Self::Folders
            | Self::FavoriteTracks
            | Self::AlbumDetailTracks
            | Self::ArtistTracks
            | Self::GenreTracks
            | Self::MoodTracks
            | Self::PlaylistTracks => LibraryLayout::Row,
            Self::Artists
            | Self::AlbumArtists
            | Self::Genres
            | Self::Moods
            | Self::Playlists
            | Self::ArtistAlbums => LibraryLayout::Grid,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum LibraryField {
    RowIndex,
    Image,
    Title,
    TitleMerged,
    Artist,
    AlbumArtist,
    Album,
    Year,
    ReleaseDate,
    DateAdded,
    LastPlayed,
    PlayCount,
    UserRating,
    Genre,
    Bpm,
    Bitrate,
    SampleRate,
    BitDepth,
    Channels,
    Format,
    FilePath,
    Source,
    TrackNumber,
    DiscNumber,
    SongCount,
    AlbumCount,
    Duration,
    Favorite,
    Tools,
}
impl LibraryField {
    pub fn catalog_sort(self) -> library::CatalogSort {
        use library::CatalogSort;
        match self {
            Self::RowIndex => CatalogSort::Position,
            Self::TrackNumber => CatalogSort::TrackNumber,
            Self::Artist => CatalogSort::Artist,
            Self::AlbumArtist => CatalogSort::AlbumArtist,
            Self::Album => CatalogSort::Album,
            Self::Year => CatalogSort::Year,
            Self::ReleaseDate => CatalogSort::ReleaseDate,
            Self::DateAdded => CatalogSort::DateAdded,
            Self::LastPlayed => CatalogSort::LastPlayed,
            Self::PlayCount => CatalogSort::PlayCount,
            Self::UserRating => CatalogSort::UserRating,
            Self::Genre => CatalogSort::Genre,
            Self::Bpm => CatalogSort::Bpm,
            Self::Duration => CatalogSort::Duration,
            Self::Favorite => CatalogSort::Favorite,
            Self::SongCount => CatalogSort::TrackCount,
            Self::AlbumCount => CatalogSort::AlbumCount,
            _ => CatalogSort::Title,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum DisplaySize {
    Compact,
    #[default]
    Default,
    Large,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DisplaySettings {
    pub size: DisplaySize,
    pub grid_spacing: DisplaySize,
    pub show_header: bool,
    pub hover_highlight: bool,
    pub alternate_rows: bool,
    pub row_borders: bool,
    pub column_borders: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            size: DisplaySize::Default,
            grid_spacing: DisplaySize::Default,
            show_header: true,
            hover_highlight: true,
            alternate_rows: false,
            row_borders: false,
            column_borders: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LibraryListSettings {
    #[serde(default)]
    pub display: DisplaySettings,
    pub layout: LibraryLayout,
    pub row_fields: Vec<LibraryField>,
    pub grid_fields: Vec<LibraryField>,
    pub detail_track_fields: Vec<LibraryField>,
    pub sort_key: LibraryField,
    pub descending: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LibraryListSettingsEntry {
    pub key: LibraryListKey,
    pub settings: LibraryListSettings,
}
impl LibraryListSettings {
    pub fn for_key(key: LibraryListKey) -> Self {
        Self {
            display: DisplaySettings::default(),
            layout: key.default_layout(),
            row_fields: default_row_fields(key),
            grid_fields: default_grid_fields(key),
            detail_track_fields: default_detail_track_fields(),
            sort_key: default_sort_key(key),
            descending: false,
        }
    }

    pub fn sanitize(&mut self, key: LibraryListKey) {
        if !key.supports_layout(self.layout) {
            self.layout = key.default_layout();
        }
        sanitize_required_fields(
            &mut self.row_fields,
            available_row_fields(key),
            default_row_fields(key),
        );
        ensure_usable_row_field(&mut self.row_fields, default_row_fields(key));
        sanitize_optional_fields(&mut self.grid_fields, available_grid_fields(key));
        sanitize_required_fields(
            &mut self.detail_track_fields,
            available_detail_track_fields(),
            default_detail_track_fields(),
        );
        ensure_usable_row_field(&mut self.detail_track_fields, default_detail_track_fields());
        if !available_sort_fields(key).contains(&self.sort_key) {
            self.sort_key = default_sort_key(key);
            self.descending = false;
        }
    }
}
pub fn default_library_list_settings() -> Vec<LibraryListSettingsEntry> {
    LibraryListKey::all()
        .into_iter()
        .map(|key| LibraryListSettingsEntry {
            key,
            settings: LibraryListSettings::for_key(key),
        })
        .collect()
}
pub fn available_row_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks => &[
            LibraryField::RowIndex,
            LibraryField::AlbumArtist,
            LibraryField::DiscNumber,
            LibraryField::TrackNumber,
            LibraryField::ReleaseDate,
            LibraryField::Bitrate,
            LibraryField::SampleRate,
            LibraryField::BitDepth,
            LibraryField::Channels,
            LibraryField::Format,
            LibraryField::FilePath,
            LibraryField::Source,
            LibraryField::TitleMerged,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Duration,
            LibraryField::Favorite,
            LibraryField::Tools,
        ],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::TitleMerged,
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Favorite,
            LibraryField::Tools,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Favorite,
            LibraryField::Tools,
        ],
        LibraryListKey::Genres => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Moods => &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Playlists => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Tracks
        | LibraryListKey::Folders
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks => &[
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::TitleMerged,
            LibraryField::Artist,
            LibraryField::AlbumArtist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::Bpm,
            LibraryField::Bitrate,
            LibraryField::SampleRate,
            LibraryField::BitDepth,
            LibraryField::Channels,
            LibraryField::Format,
            LibraryField::FilePath,
            LibraryField::Source,
            LibraryField::DiscNumber,
            LibraryField::TrackNumber,
            LibraryField::Duration,
            LibraryField::Favorite,
            LibraryField::Tools,
        ],
    }
}
pub fn available_grid_fields(key: LibraryListKey) -> &'static [LibraryField] {
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks | LibraryListKey::Folders => &[],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
        ],
        LibraryListKey::Genres => &[LibraryField::AlbumCount, LibraryField::SongCount],
        LibraryListKey::Moods => &[LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::Playlists => &[LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks => &[
            LibraryField::Artist,
            LibraryField::AlbumArtist,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Genre,
            LibraryField::Duration,
        ],
    }
}

pub fn available_sort_fields(key: LibraryListKey) -> &'static [LibraryField] {
    const TRACK_SORT_FIELDS: &[LibraryField] = &[
        LibraryField::RowIndex,
        LibraryField::TrackNumber,
        LibraryField::Title,
        LibraryField::Artist,
        LibraryField::AlbumArtist,
        LibraryField::Album,
        LibraryField::Year,
        LibraryField::ReleaseDate,
        LibraryField::DateAdded,
        LibraryField::LastPlayed,
        LibraryField::PlayCount,
        LibraryField::UserRating,
        LibraryField::Genre,
        LibraryField::Bpm,
        LibraryField::Duration,
        LibraryField::Favorite,
    ];
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks => &[LibraryField::RowIndex],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => &[
            LibraryField::Title,
            LibraryField::AlbumArtist,
            LibraryField::Year,
            LibraryField::ReleaseDate,
            LibraryField::DateAdded,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::SongCount,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => &[
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
            LibraryField::LastPlayed,
            LibraryField::PlayCount,
            LibraryField::UserRating,
            LibraryField::Favorite,
        ],
        LibraryListKey::Genres => &[
            LibraryField::Title,
            LibraryField::AlbumCount,
            LibraryField::SongCount,
        ],
        LibraryListKey::Moods => &[
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Playlists => &[
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::PlaylistTracks => TRACK_SORT_FIELDS,
        LibraryListKey::Tracks
        | LibraryListKey::Folders
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks => &TRACK_SORT_FIELDS[1..],
    }
}
fn default_row_fields(key: LibraryListKey) -> Vec<LibraryField> {
    let mut fields = combined_row_fields(key);
    if available_row_fields(key).contains(&LibraryField::Favorite)
        && let Some(position) = fields
            .iter()
            .position(|field| *field == LibraryField::Tools)
    {
        fields.insert(position, LibraryField::Favorite);
    }
    fields
}

fn combined_row_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Queue => vec![LibraryField::TitleMerged, LibraryField::Year],
        LibraryListKey::FullscreenTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Year,
            LibraryField::Tools,
        ],
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::Tools,
        ],
        LibraryListKey::Genres => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Moods => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Tools,
        ],
        LibraryListKey::Playlists => vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::Tools,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        LibraryListKey::Tracks | LibraryListKey::Folders | LibraryListKey::FavoriteTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Tools,
        ],
        LibraryListKey::AlbumDetailTracks => vec![
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Duration,
            LibraryField::Tools,
        ],
        LibraryListKey::ArtistTracks => vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Tools,
        ],
        LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks => {
            vec![
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::Album,
                LibraryField::Tools,
            ]
        }
    }
}
fn default_grid_fields(key: LibraryListKey) -> Vec<LibraryField> {
    match key {
        LibraryListKey::Queue | LibraryListKey::FullscreenTracks | LibraryListKey::Folders => {
            Vec::new()
        }
        LibraryListKey::Albums | LibraryListKey::ArtistAlbums => {
            vec![LibraryField::AlbumArtist, LibraryField::Year]
        }
        LibraryListKey::Artists | LibraryListKey::AlbumArtists => Vec::new(),
        LibraryListKey::Genres => Vec::new(),
        LibraryListKey::Moods => vec![LibraryField::SongCount, LibraryField::Duration],
        LibraryListKey::Playlists => vec![LibraryField::SongCount],
        LibraryListKey::Tracks
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks
        | LibraryListKey::PlaylistTracks => {
            vec![
                LibraryField::Artist,
                LibraryField::Album,
                LibraryField::Duration,
            ]
        }
    }
}
pub fn available_detail_track_fields() -> &'static [LibraryField] {
    &[
        LibraryField::AlbumArtist,
        LibraryField::DiscNumber,
        LibraryField::ReleaseDate,
        LibraryField::Bitrate,
        LibraryField::SampleRate,
        LibraryField::BitDepth,
        LibraryField::Channels,
        LibraryField::Format,
        LibraryField::FilePath,
        LibraryField::Source,
        LibraryField::RowIndex,
        LibraryField::TrackNumber,
        LibraryField::Title,
        LibraryField::Duration,
        LibraryField::Favorite,
        LibraryField::Tools,
    ]
}
fn default_detail_track_fields() -> Vec<LibraryField> {
    vec![
        LibraryField::RowIndex,
        LibraryField::Title,
        LibraryField::Duration,
    ]
}
fn default_sort_key(key: LibraryListKey) -> LibraryField {
    match key {
        LibraryListKey::Albums
        | LibraryListKey::Artists
        | LibraryListKey::AlbumArtists
        | LibraryListKey::Genres
        | LibraryListKey::Moods
        | LibraryListKey::ArtistAlbums
        | LibraryListKey::Tracks
        | LibraryListKey::Folders
        | LibraryListKey::FavoriteTracks
        | LibraryListKey::Playlists => LibraryField::Title,
        LibraryListKey::Queue
        | LibraryListKey::FullscreenTracks
        | LibraryListKey::PlaylistTracks => LibraryField::RowIndex,
        LibraryListKey::AlbumDetailTracks
        | LibraryListKey::ArtistTracks
        | LibraryListKey::GenreTracks
        | LibraryListKey::MoodTracks => LibraryField::TrackNumber,
    }
}
fn sanitize_optional_fields(fields: &mut Vec<LibraryField>, available: &[LibraryField]) {
    let mut seen = Vec::new();
    fields.retain(|field| {
        if !available.contains(field) || seen.contains(field) {
            return false;
        }
        seen.push(*field);
        true
    });
}
fn sanitize_required_fields(
    fields: &mut Vec<LibraryField>,
    available: &[LibraryField],
    fallback: Vec<LibraryField>,
) {
    sanitize_optional_fields(fields, available);
    if fields.is_empty() {
        *fields = fallback;
    }
}
fn ensure_usable_row_field(fields: &mut Vec<LibraryField>, fallback: Vec<LibraryField>) {
    if fields.iter().any(|field| row_field_is_usable(*field)) {
        return;
    }
    if let Some(field) = fallback
        .into_iter()
        .find(|field| row_field_is_usable(*field))
    {
        fields.push(field);
    }
}
fn row_field_is_usable(field: LibraryField) -> bool {
    !matches!(
        field,
        LibraryField::RowIndex
            | LibraryField::Image
            | LibraryField::TrackNumber
            | LibraryField::DiscNumber
            | LibraryField::Favorite
            | LibraryField::Tools
    )
}

#[cfg(test)]
mod disc_tests {
    use super::*;
    #[test]
    fn named_theme_preference_restores_as_system() {
        #[derive(Deserialize)]
        struct Stored {
            theme: ThemePreference,
        }
        let stored: Stored = toml::from_str("theme = { Named = \"builtin:nord\" }").unwrap();
        assert_eq!(stored.theme, ThemePreference::System);
    }
    #[test]
    fn album_detail_uses_rows_when_grid_was_saved() {
        let mut settings = LibraryListSettings::for_key(LibraryListKey::AlbumDetailTracks);
        settings.layout = LibraryLayout::Grid;
        let fields = settings.row_fields.clone();
        settings.sanitize(LibraryListKey::AlbumDetailTracks);
        assert_eq!(settings.layout, LibraryLayout::Row);
        assert_eq!(settings.row_fields, fields);
        assert!(!LibraryListKey::AlbumDetailTracks.supports_layout(LibraryLayout::Grid));
        assert!(LibraryListKey::Tracks.supports_layout(LibraryLayout::Grid));
        assert!(LibraryListKey::Albums.supports_layout(LibraryLayout::Grid));
    }

    #[test]
    fn favorite_tracks_row_fields_match_tracks_route_defaults() {
        assert_eq!(
            default_row_fields(LibraryListKey::FavoriteTracks),
            default_row_fields(LibraryListKey::Tracks)
        );
    }

    #[test]
    fn default_row_fields_do_not_exceed_five_columns() {
        for key in LibraryListKey::all() {
            let defaults = default_row_fields(key);
            assert!(
                defaults.len() <= 5,
                "key {:?} has {} default row columns: {:?}",
                key,
                defaults.len(),
                defaults
            );
        }
    }
}
