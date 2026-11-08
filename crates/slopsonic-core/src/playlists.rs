//! Playlist routing and publication.
use crate::runtime::{CatalogChange, SourceEvent};
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
use futures_util::{StreamExt, TryStreamExt};
use library::{Database, PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey};
use sources::{Source, SourceError, SourceId};
use std::{future::Future, sync::Arc};
fn run<T, Work>(
    owner: &SourceOwner,
    work: impl FnOnce(SourceOwner) -> Work + Send + 'static,
) -> Receiver<Result<T, String>>
where
    T: Send + 'static,
    Work: Future<Output = Result<T, String>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = work(owner.clone()).await;
        if let Err(error) = &result {
            owner.shared.warn_nonfatal(error);
        }
        let _ = sender.send(result).await;
    });
    receiver
}

#[derive(Clone, Debug)]
pub struct PlaylistCreation {
    pub playlist_key: Option<PlaylistKey>,
    pub object_id: String,
    pub settings_error: Option<String>,
}

pub async fn create_playlist(
    owner: &SourceOwner,
    source_id: SourceId,
    name: String,
    songs: Arc<library::MemberSelection>,
    public: Option<bool>,
    current: Option<bool>,
) -> sources::SourceResult<Option<PlaylistCreation>> {
    run(owner, move |owner| async move {
        let database = &owner.shared.database;
        let key = database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .ok_or("Source access is unavailable")?
            .source;
        let source = owner.client(&source_id)?;
        let (playlist, object_id) = {
            let tracks = database
                .source_playlist_track_ids(key, None, &songs, false)
                .await
                .map_err(string_error)?;
            let first = &tracks[..tracks.len().min(256)];
            let object = {
                let provider = &source.subsonic;
                provider.create_playlist(&name, first, public).await
            }
            .map_err(string_error)?;
            append_native_tracks(&source, &object, &tracks[first.len()..])
                .await
                .map_err(string_error)?;
            refresh_playlist(&source, database, Some(object.clone()), None)
                .await
                .map_err(string_error)?;
            let playlist = match database
                .playlist_key_by_object(key, &object, &ReadCancellation::new())
                .await
            {
                Ok(playlist) => playlist,
                Err(error) => {
                    owner.shared.warn_nonfatal(&error.to_string());
                    None
                }
            };
            (playlist, object)
        };
        let playlist_id = object_id.clone();
        let settings = owner.shared.settings.clone();
        let settings_error = tokio::task::spawn_blocking(move || {
            settings.update(|stored| {
                stored.ui.sidebar.set_pinned(
                    crate::settings::SidebarPin::Playlist {
                        source_id: Some(source_id),
                        playlist_id,
                    },
                    true,
                );
                if let Some(current) = current {
                    stored.ui.application.new_playlist_current = current;
                }
                Ok(())
            })
        })
        .await
        .map_err(string_error)
        .and_then(|result| result)
        .err();
        Ok(Some(PlaylistCreation {
            playlist_key: playlist,
            object_id,
            settings_error,
        }))
    })
    .recv()
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
    .map_err(SourceError::Other)
}

pub fn playlist_public(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<Option<bool>, String>> {
    owner.reply(move |owner, _database| async move {
        let (source, _, id) = playlist_source(&owner, playlist, None).await?;
        source
            .subsonic
            .playlist_public(&id)
            .await
            .map_err(string_error)
    })
}

#[derive(Clone, Debug)]
pub struct PlaylistArtworkEditing {
    pub binding: Option<Vec<u8>>,
    pub automatic_binding: Option<Vec<u8>>,
    pub writable: bool,
}

pub fn playlist_artwork_editing(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<PlaylistArtworkEditing, String>> {
    owner.reply(move |owner, database| async move {
        let row = database
            .playlists(&[playlist], &ReadCancellation::new())
            .await
            .map_err(string_error)?
            .pop()
            .ok_or("Playlist no longer exists")?;
        let writable = row.metadata_writable
            && row
                .source_id
                .as_ref()
                .map(|id| owner.client(&SourceId::new(id)))
                .transpose()?
                .is_some_and(|source| source.supports_playlist_artwork());
        let binding = row
            .artwork_binding
            .as_deref()
            .map(library::authored_playlist_artwork)
            .transpose()
            .map_err(string_error)?
            .flatten();
        let automatic_binding = row
            .artwork_binding
            .as_deref()
            .map(library::automatic_playlist_artwork)
            .transpose()
            .map_err(string_error)?
            .flatten();
        Ok(PlaylistArtworkEditing {
            binding,
            automatic_binding,
            writable,
        })
    })
}

pub fn regenerate_playlist_artwork(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, database| async move {
        let bindings = database
            .random_playlist_artwork(playlist)
            .await
            .map_err(string_error)?;
        generate_artwork(&owner, bindings).await
    })
}

async fn generate_artwork(
    owner: &SourceOwner,
    bindings: Vec<Vec<u8>>,
) -> Result<Option<Arc<sources::ImageBytes>>, String> {
    let mut images = futures_util::stream::iter(bindings)
        .map(|binding| {
            let request =
                artwork::ArtworkRequest::new(artwork::ArtworkBinding::opaque(&binding), 512);
            owner.shared.artwork.image_bytes(request)
        })
        .buffered(4)
        .try_filter_map(|bytes| async { Ok(bytes) })
        .try_collect::<Vec<_>>()
        .await?;
    if images.is_empty() {
        return Ok(None);
    }
    if images.len() == 1 {
        let bytes = images.pop().unwrap();
        return Ok(Some(Arc::new(sources::ImageBytes {
            content_type: artwork::image_mime(&bytes).map(str::to_owned),
            bytes,
        })));
    }
    let bytes = tokio::task::spawn_blocking(move || artwork::collage_png(&images, 1024))
        .await
        .map_err(string_error)?
        .map_err(string_error)?;
    Ok(Some(Arc::new(sources::ImageBytes {
        bytes,
        content_type: Some("image/png".into()),
    })))
}

pub async fn update_playlist(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: Option<String>,
    public: Option<bool>,
    artwork: Option<sources::ArtworkChange>,
) -> sources::SourceResult<bool> {
    run(owner, move |owner| async move {
        update_playlist_on(&owner, playlist, name.as_deref(), public, artwork.as_ref()).await
    })
    .recv()
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
    .map_err(SourceError::Other)
}

pub(crate) async fn update_playlist_on(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    name: Option<&str>,
    public: Option<bool>,
    artwork: Option<&sources::ArtworkChange>,
) -> Result<bool, String> {
    let (source, _, id) = playlist_source(
        owner,
        playlist,
        (name.is_some() || artwork.is_some()).then_some(false),
    )
    .await?;
    let previous = if artwork.is_some() {
        owner
            .shared
            .database
            .playlist_artwork_binding(playlist)
            .await
            .map_err(string_error)?
    } else {
        None
    };
    let result = async {
        let error = |error: SourceError| sources::SourceMetadataError::Write(error.to_string());
        if artwork.is_some() && !source.supports_playlist_artwork() {
            return Err(sources::SourceMetadataError::Unavailable);
        }
        if name.is_some() || public.is_some() {
            source
                .subsonic
                .update_playlist(&id, name, public)
                .await
                .map_err(error)?
        }
        let result = if let Some(artwork) = artwork {
            source
                .subsonic
                .change_playlist_artwork(&id, artwork)
                .await
                .map_err(error)
        } else {
            Ok(())
        };
        if name.is_none() && artwork.is_none() {
            return result.map(|()| false);
        }
        let refresh = refresh_playlist(&source, &owner.shared.database, Some(id), None)
            .await
            .map_err(|error| sources::SourceMetadataError::SavedRefreshFailed(error.to_string()));
        sources::finish_metadata_save(result, refresh, name.is_some() || public.is_some())
            .map(|()| true)
    }
    .await;
    let changed = match &result {
        Ok(changed) => *changed,
        Err(sources::SourceMetadataError::PartiallySaved { .. })
        | Err(sources::SourceMetadataError::SavedRefreshFailed(_)) => true,
        Err(error) => {
            owner.shared.warn_nonfatal(&error.to_string());
            false
        }
    };
    if changed && let Some(binding) = previous {
        let cache = owner.shared.artwork.clone();
        match tokio::task::spawn_blocking(move || {
            cache.invalidate_image(&artwork::ArtworkBinding::opaque(&binding))
        })
        .await
        {
            Ok(Ok(true)) => {
                owner
                    .artwork_invalidated(
                        source.source_id(),
                        CatalogChange::Playlists(Some(playlist)),
                    )
                    .await;
            }
            Ok(Ok(false)) => {}
            error => owner
                .shared
                .warn_nonfatal(&format!("Playlist artwork cache refresh failed: {error:?}")),
        }
    }
    result
        .map(|changed| changed || public.is_some())
        .map_err(string_error)
}

pub async fn delete_playlist(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> sources::SourceResult<bool> {
    playlist_change(
        &owner,
        playlist,
        false,
        move |source, id, database| async move {
            source.subsonic.delete_playlist(&id).await?;
            refresh_playlist(&source, &database, None, Some(id))
                .await
                .map(|()| true)
        },
    )
    .recv()
    .await
    .map_err(|error| SourceError::Other(error.to_string()))?
    .map_err(SourceError::Other)
}

pub async fn add_playlist_tracks(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    songs: Arc<library::MemberSelection>,
    skip_duplicates: bool,
) -> sources::SourceResult<usize> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = match playlist_source(&owner, playlist, Some(true)).await {
            Ok((source, source_key, id)) => {
                async {
                    let tracks = owner
                        .shared
                        .database
                        .source_playlist_track_ids(
                            source_key,
                            Some(playlist),
                            &songs,
                            skip_duplicates,
                        )
                        .await?;
                    append_native_tracks(&source, &id, &tracks).await?;
                    if !tracks.is_empty() {
                        refresh_playlist(&source, &owner.shared.database, Some(id), None).await?;
                    }
                    Ok(tracks.len())
                }
                .await
            }
            Err(error) => Err(sources::SourceError::Other(error)),
        };
        let reply = match result {
            Ok(accepted) => Ok(accepted),
            Err(error) => {
                if let sources::SourceError::Library(
                    library::LibraryError::PlaylistSourceMismatch(source_id),
                ) = &error
                {
                    owner
                        .shared
                        .send(SourceEvent::PlaylistSourceMismatch(source_id.clone()))
                        .await;
                } else {
                    owner.shared.warn_nonfatal(&error.to_string());
                }
                Err(error.to_string())
            }
        };
        let _ = sender.send(reply).await;
    });
    receiver
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
        .map_err(SourceError::Other)
}

pub fn remove_playlist_entries(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    selection: library::MemberSelection,
) -> Receiver<Result<Vec<library::TrackKey>, String>> {
    run(owner, move |owner| async move {
        let (source, _, id) = playlist_source(&owner, playlist, Some(true)).await?;
        let database = &owner.shared.database;
        let entries = database
            .playlist_edit_entries(playlist, &selection, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        if let library::MemberSelection::Entries(requested) = &selection
            && entries.len() != requested.len()
        {
            return Err("Playlist entry is no longer current".into());
        }
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let songs = entries
            .iter()
            .map(|entry| entry.track_key)
            .collect::<Vec<_>>();
        // Later positions go first because Subsonic identifies entries by position.
        let ids = entries
            .into_iter()
            .rev()
            .map(|entry| entry.object_id)
            .collect::<Vec<_>>();
        for ids in ids.chunks(256) {
            source
                .subsonic
                .remove_playlist_entries(&id, ids)
                .await
                .map_err(string_error)?;
        }
        refresh_playlist(&source, database, Some(id), None)
            .await
            .map_err(string_error)?;
        Ok(songs)
    })
}

pub fn move_playlist_entry(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    entry: PlaylistEntryKey,
    position: usize,
) -> Receiver<Result<bool, String>> {
    playlist_change(
        &owner,
        playlist,
        true,
        move |source, id, database| async move {
            let entry = database
                .playlist_edit_entries(
                    playlist,
                    &library::MemberSelection::Entries(vec![entry].into()),
                    &ReadCancellation::new(),
                )
                .await?
                .pop()
                .ok_or(SourceError::NotFound)?;
            if entry.position == position {
                return Ok(false);
            }
            source
                .subsonic
                .move_playlist_entry(&id, &entry.object_id, position)
                .await?;
            refresh_playlist(&source, &database, Some(id), None)
                .await
                .map(|()| true)
        },
    )
}

fn playlist_change<F, Work>(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: bool,
    change: F,
) -> Receiver<Result<bool, String>>
where
    F: FnOnce(Arc<Source>, String, Arc<Database>) -> Work + Send + 'static,
    Work: Future<Output = Result<bool, SourceError>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = match playlist_source(&owner, playlist, Some(edit_tracks)).await {
            Ok((source, _, id)) => change(source, id, Arc::clone(&owner.shared.database)).await,
            Err(error) => Err(SourceError::Other(error)),
        };
        if let Err(error) = &result {
            owner.shared.warn_nonfatal(&error.to_string());
        }
        let _ = sender.try_send(result.map_err(string_error));
    });
    receiver
}

async fn playlist_source(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: Option<bool>,
) -> Result<(Arc<Source>, SourceKey, String), String> {
    let access = owner
        .shared
        .database
        .playlist_access(playlist, &ReadCancellation::new())
        .await
        .map_err(string_error)?
        .ok_or("Playlist no longer exists")?;
    if edit_tracks.is_some_and(|tracks| {
        if tracks {
            !access.writable
        } else {
            !access.metadata_writable
        }
    }) {
        return Err("The server does not allow this playlist change".into());
    }
    let key = access.source_key.ok_or("Playlist owner is unavailable")?;
    let id = access.source_id.ok_or("Playlist owner is unavailable")?;
    Ok((owner.client(&SourceId::new(id))?, key, access.object_id))
}

async fn append_native_tracks(
    source: &Source,
    playlist: &str,
    tracks: &[String],
) -> Result<(), SourceError> {
    for tracks in tracks.chunks(256) {
        source
            .subsonic
            .add_playlist_tracks(playlist, tracks)
            .await?
    }
    Ok(())
}

async fn refresh_playlist(
    source: &Source,
    database: &Database,
    affected: Option<String>,
    deleted: Option<String>,
) -> Result<(), SourceError> {
    let object = |id: String| {
        let raw = id.rsplit(':').next().unwrap_or(&id);
        format!("playlist:{raw}")
    };
    source
        .apply_items(
            database,
            affected.into_iter().map(object).collect(),
            deleted.into_iter().map(object).collect(),
        )
        .await
}
