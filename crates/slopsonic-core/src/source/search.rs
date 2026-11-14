use crate::runtime::SelectedLibrary;
use library::{ReadCancellation, SearchRequest, SearchResults};

pub async fn acquire_search(
    owner: &super::SourceOwner,
    database: &library::Database,
    selected: Option<&SelectedLibrary>,
    query: String,
    limit: usize,
    cancellation: ReadCancellation,
) -> Result<SearchResults, String> {
    let selected = selected.ok_or_else(|| "No source selected".to_owned())?;
    if let Some(source) = owner.cached_client() {
        let live = source.live_search(database, &query, limit);
        let result = tokio::select! {
            result = live => result,
            () = cancellation.cancelled() => return Err(library::LibraryError::ReadCancelled.to_string()),
        };
        if let Ok(mut rows) = result {
            rows.playlists = database
                .search_playlists(
                    selected.music_folder_key,
                    &SearchRequest::with_limit(query, limit),
                    &cancellation,
                )
                .await
                .unwrap_or_default();
            return Ok(rows);
        }
    }
    database
        .search(
            selected.music_folder_key,
            false,
            &SearchRequest::with_limit(query, limit),
            &cancellation,
        )
        .await
        .map_err(|error| error.to_string())
}
