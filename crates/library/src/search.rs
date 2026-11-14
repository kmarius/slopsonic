//! Owns bounded cached Search and complete route-filter orders over normalized facts.
//! Callers receive final rows and never merge related Store results.

use sqlx::Connection;

use crate::query::{OrderedQuery, RequestedKeys};
use crate::{
    AlbumView, ArtistView, CatalogSort, Collection, Database, FolderKey, LibraryResult, ListKind,
    ListRead, MemberRead, PlaylistRow, ReadCancellation, TrackView,
};

const SEARCH_LIMIT: usize = 100;
const SEARCH_TEXT_LIMIT: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    query: String,
    limit: usize,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self::with_limit(query, 20)
    }

    pub fn with_limit(query: impl Into<String>, limit: usize) -> Self {
        let query = query
            .into()
            .trim()
            .to_lowercase()
            .chars()
            .take(SEARCH_TEXT_LIMIT)
            .collect();
        Self {
            query,
            limit: limit.clamp(1, SEARCH_LIMIT),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<TrackView>,
    pub albums: Vec<AlbumView>,
    pub artists: Vec<ArtistView>,
    pub playlists: Vec<PlaylistRow>,
}

impl Database {
    pub async fn search_rows_by_objects(
        &self,
        _folder: Option<FolderKey>,
        album_artists: bool,
        track_objects: &[String],
        album_objects: &[String],
        artist_objects: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        if [
            track_objects.len(),
            album_objects.len(),
            artist_objects.len(),
        ]
        .into_iter()
        .any(|len| len > SEARCH_LIMIT)
        {
            return Err(crate::LibraryError::InvalidRequest(
                "Search identity page exceeds 100 rows".into(),
            ));
        }
        let mut connection = self.acquire_general(cancellation).await?;
        let source = crate::db::server_key(&mut connection).await?;
        let mut transaction = connection.begin().await?;
        let request = |kind: crate::CatalogKind,
                       objects: &[String]|
         -> LibraryResult<RequestedKeys> {
            let (table, name) = kind.sql();
            let batch = RequestedKeys::batch(objects)?;
            Ok(RequestedKeys {
                sql: format!(
                    "SELECT entity.{name}_key value,request.ordinal FROM ({}) request JOIN {table} entity ON entity.source_key={} AND entity.object_id=request.value",
                    batch.sql,
                    source.raw()
                ),
                arguments: batch.arguments,
            })
        };
        let result = SearchResults {
            tracks: crate::tracks::load_track_requested(
                &mut transaction,
                request(crate::CatalogKind::Track, track_objects)?,
            )
            .await?,
            albums: crate::collections::load_album_requested(
                &mut transaction,
                request(crate::CatalogKind::Album, album_objects)?,
                false,
            )
            .await?,
            artists: crate::collections::load_artist_requested(
                &mut transaction,
                request(crate::CatalogKind::Artist, artist_objects)?,
                album_artists,
                false,
            )
            .await?,
            playlists: Vec::new(),
        };
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn search_playlists(
        &self,
        folder: Option<FolderKey>,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PlaylistRow>> {
        if request.query.is_empty() {
            return Ok(Vec::new());
        }
        let mut connection = self.acquire_general(cancellation).await?;
        Self::load_playlist_requested(&mut connection, playlist_search_keys(folder, request)?).await
    }

    pub async fn search(
        &self,
        folder: Option<FolderKey>,
        album_artists: bool,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        if request.query.is_empty() {
            return Ok(SearchResults::default());
        }
        let mut connection = self.acquire_general(cancellation).await?;
        let mut transaction = connection.begin().await?;
        let mut tracks = MemberRead::new(Collection::Library);
        tracks.folder = folder;
        let tracks = crate::query::member_query(&tracks)?;
        let mut albums = ListRead::new(ListKind::Albums, CatalogSort::Title);
        albums.folder = folder;
        let albums = crate::collections::collection_query(&albums);
        let mut artists = ListRead::new(
            ListKind::Artists {
                album_artist: album_artists,
            },
            CatalogSort::Title,
        );
        artists.folder = folder;
        let artists = crate::collections::collection_query(&artists);
        let album_condition = format!(
            "({} OR CAST(album.year_sort AS TEXT)=term.value OR EXISTS(SELECT 1 FROM album_artists credit JOIN artists artist USING(artist_key) WHERE credit.album_key=album.album_key AND {}) OR EXISTS(SELECT 1 FROM album_genres credit JOIN genres genre USING(genre_key) WHERE credit.album_key=album.album_key AND {}))",
            term_match("album.title_search||' '||COALESCE(album.artist_search,'')"),
            term_match("artist.normalized_name"),
            term_match("genre.normalized_name")
        );
        let terms_match = |mut relation: OrderedQuery, condition: &str| {
            relation
                .predicate
                .push_str(&all_terms_match(request, condition)?);
            LibraryResult::Ok(relation.requested_keys(0, request.limit))
        };
        let result = SearchResults {
            tracks: crate::tracks::load_track_requested(
                &mut transaction,
                terms_match(tracks, &term_match("track.normalized_search"))?,
            )
            .await?,
            albums: crate::collections::load_album_requested(
                &mut transaction,
                terms_match(albums, &album_condition)?,
                false,
            )
            .await?,
            artists: crate::collections::load_artist_requested(
                &mut transaction,
                terms_match(artists, &term_match("artist.normalized_name"))?,
                album_artists,
                false,
            )
            .await?,
            playlists: Self::load_playlist_requested(
                &mut transaction,
                playlist_search_keys(folder, request)?,
            )
            .await?,
        };
        transaction.commit().await?;
        Ok(result)
    }
}

fn playlist_search_keys(
    folder: Option<FolderKey>,
    request: &SearchRequest,
) -> LibraryResult<RequestedKeys> {
    let mut playlists =
        crate::playlists::playlist_list_query(folder, CatalogSort::Title, false, "");
    playlists.predicate.push_str(&all_terms_match(
        request,
        &term_match("playlist.normalized_name"),
    )?);
    Ok(playlists.requested_keys(0, request.limit))
}

fn all_terms_match(request: &SearchRequest, condition: &str) -> LibraryResult<String> {
    let terms = request.query.split_whitespace().collect::<Vec<_>>();
    let terms = crate::query::quote(&serde_json::to_string(&terms)?);
    Ok(format!(
        " AND NOT EXISTS(SELECT 1 FROM json_each({terms}) term WHERE NOT COALESCE({condition},0))"
    ))
}

fn term_match(text: &str) -> String {
    format!(
        "CASE WHEN length(term.value)=1 THEN instr(' '||{text}||' ',' '||term.value||' ') ELSE instr({text},term.value) END>0"
    )
}
