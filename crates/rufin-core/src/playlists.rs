//! Playlist routing and publication.
use crate::runtime::{CatalogChange, CatalogPublication, SourceEvent};
use crate::source::{SourceOwner, string_error};
use async_channel::Receiver;
use futures_util::{StreamExt, TryStreamExt};
use library::{Database, PlaylistEntryKey, PlaylistKey, ReadCancellation, SourceKey};
use sources::{Implementation, Source, SourceError, SourceId};
use std::{
    future::Future,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
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
    source_id: Option<SourceId>,
    name: String,
    songs: Arc<library::MemberSelection>,
    public: Option<bool>,
    current: Option<bool>,
) -> sources::SourceResult<Option<PlaylistCreation>> {
    run(owner, move |owner| async move {
        let database = &owner.shared.database;
        let pin_source = source_id.clone();
        let target = match source_id {
            None => PlaylistOwner::Local(None),
            Some(id) => {
                let cached = database
                    .cached_source(id.as_str(), &ReadCancellation::new())
                    .await
                    .map_err(string_error)?
                    .ok_or("Source access is unavailable")?;
                configured_playlist_owner(&owner, &id, cached.source)?
            }
        };
        let source_key = target.source_key();
        let local = matches!(target, PlaylistOwner::Local(_));
        let (playlist, object_id) = match target {
            PlaylistOwner::Local(key) => {
                let Some((playlist, object)) = database
                    .create_playlist(key, &name, &songs)
                    .await
                    .map_err(string_error)?
                else {
                    return Ok(None);
                };
                (Some(playlist), object)
            }
            PlaylistOwner::Server(source, key) => {
                let tracks = database
                    .source_playlist_track_ids(key, None, &songs, false)
                    .await
                    .map_err(string_error)?;
                let first = &tracks[..tracks.len().min(256)];
                let object = match &source.implementation {
                    Implementation::OpenSubsonic(provider) => {
                        provider.create_playlist(&name, first, public).await
                    }
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
            }
        };
        if local {
            local_playlist_changed(&owner, source_key, playlist).await;
        }
        let playlist_id = object_id.clone();
        let settings = owner.shared.settings.clone();
        let settings_error = tokio::task::spawn_blocking(move || {
            settings.update(|stored| {
                stored.ui.sidebar.set_pinned(
                    crate::settings::SidebarPin::Playlist {
                        source_id: pin_source,
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
        let (target, id) = playlist_source(&owner, playlist, None).await?;
        match target {
            PlaylistOwner::Local(_) => Ok(None),
            PlaylistOwner::Server(source, _) => match &source.implementation {
                Implementation::OpenSubsonic(provider) => provider.playlist_public(&id).await,
            }
            .map_err(string_error),
        }
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
            && (!row.source_owned
                || row
                    .source_id
                    .as_ref()
                    .map(|id| owner.client(&SourceId::new(id)))
                    .transpose()?
                    .is_some_and(|source| source.supports_playlist_artwork()));
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

pub fn regenerate_smart_playlist_artwork(
    owner: &SourceOwner,
    key: library::SmartPlaylistKey,
) -> Receiver<Result<Option<Arc<sources::ImageBytes>>, String>> {
    owner.reply(move |owner, database| async move {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let bindings = database
            .random_smart_playlist_artwork(key, now)
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
    let (target, id) = playlist_source(
        owner,
        playlist,
        (name.is_some() || artwork.is_some()).then_some(false),
    )
    .await?;
    let source_key = target.source_key();
    let native_source = match &target {
        PlaylistOwner::Local(_) => None,
        PlaylistOwner::Server(source, _) => Some(source.source_id().clone()),
    };
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
    let result = match target {
        PlaylistOwner::Local(source) => {
            let image = artwork.map(|change| match change {
                sources::ArtworkChange::Replace(image) => {
                    Some((image.bytes.as_slice(), image.content_type.as_deref()))
                }
                sources::ArtworkChange::Remove => None,
            });
            owner
                .shared
                .database
                .update_playlist(source, playlist, name, image)
                .await
                .map_err(|error| sources::SourceMetadataError::Write(error.to_string()))
        }
        PlaylistOwner::Server(source, _) => {
            async {
                let error =
                    |error: SourceError| sources::SourceMetadataError::Write(error.to_string());
                if artwork.is_some() && !source.supports_playlist_artwork() {
                    return Err(sources::SourceMetadataError::Unavailable);
                }
                if name.is_some() || public.is_some() {
                    match &source.implementation {
                        Implementation::OpenSubsonic(provider) => provider
                            .update_playlist(&id, name, public)
                            .await
                            .map_err(error)?,
                    }
                }
                let result = if let Some(artwork) = artwork {
                    match &source.implementation {
                        Implementation::OpenSubsonic(provider) => {
                            provider.change_playlist_artwork(&id, artwork).await
                        }
                    }
                    .map_err(error)
                } else {
                    Ok(())
                };
                if name.is_none() && artwork.is_none() {
                    return result.map(|()| false);
                }
                let refresh = refresh_playlist(&source, &owner.shared.database, Some(id), None)
                    .await
                    .map_err(|error| {
                        sources::SourceMetadataError::SavedRefreshFailed(error.to_string())
                    });
                sources::finish_metadata_save(result, refresh, name.is_some() || public.is_some())
                    .map(|()| true)
            }
            .await
        }
    };
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
                if let Some(source) = &native_source {
                    owner
                        .artwork_invalidated(source, CatalogChange::Playlists(Some(playlist)))
                        .await;
                }
            }
            Ok(Ok(false)) => {}
            error => owner
                .shared
                .warn_nonfatal(&format!("Playlist artwork cache refresh failed: {error:?}")),
        }
    }
    if changed {
        if native_source.is_none() {
            local_playlist_changed(owner, source_key, Some(playlist)).await;
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
        move |target, id, database| async move {
            let result = match target {
                PlaylistOwner::Local(source_key) => database
                    .delete_playlist(source_key, playlist)
                    .await
                    .map_err(SourceError::from),
                PlaylistOwner::Server(source, _) => {
                    match &source.implementation {
                        Implementation::OpenSubsonic(provider) => {
                            provider.delete_playlist(&id).await?
                        }
                    }
                    refresh_playlist(&source, &database, None, Some(id))
                        .await
                        .map(|()| true)
                }
            };
            result
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
        let target = playlist_source(&owner, playlist, Some(true)).await;
        let source_key = target
            .as_ref()
            .ok()
            .and_then(|(target, _)| target.source_key());
        let local = matches!(&target, Ok((PlaylistOwner::Local(_), _)));
        let result = match target {
            Ok((PlaylistOwner::Local(source_key), _)) => owner
                .shared
                .database
                .add_playlist_tracks(source_key, playlist, &songs, skip_duplicates)
                .await
                .map_err(sources::SourceError::from),
            Ok((PlaylistOwner::Server(source, source_key), id)) => {
                async {
                    let skip = skip_duplicates;
                    let tracks = owner
                        .shared
                        .database
                        .source_playlist_track_ids(source_key, Some(playlist), &songs, skip)
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
            Ok(accepted) => {
                if accepted > 0 {
                    if local {
                        local_playlist_changed(&owner, source_key, Some(playlist)).await;
                    }
                }
                Ok(accepted)
            }
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
        let (target, id) = playlist_source(&owner, playlist, Some(true)).await?;
        let source_key = target.source_key();
        let local = matches!(target, PlaylistOwner::Local(_));
        let database = &owner.shared.database;
        let entries = database
            .playlist_edit_entries(playlist, &selection, &ReadCancellation::new())
            .await
            .map_err(string_error)?;
        if !local
            && let library::MemberSelection::Entries(requested) = &selection
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
        match target {
            PlaylistOwner::Local(source) => {
                let keys = entries
                    .iter()
                    .map(|entry| entry.playlist_entry_key)
                    .collect::<Vec<_>>();
                if database
                    .remove_playlist_entries(source, playlist, &keys)
                    .await
                    .map_err(string_error)?
                    == 0
                {
                    return Ok(Vec::new());
                }
            }
            PlaylistOwner::Server(source, _) => {
                // Later positions go first because Subsonic identifies entries by position.
                let ids = entries
                    .into_iter()
                    .rev()
                    .map(|entry| entry.object_id)
                    .collect::<Vec<_>>();
                for ids in ids.chunks(256) {
                    match &source.implementation {
                        Implementation::OpenSubsonic(provider) => {
                            provider.remove_playlist_entries(&id, ids).await
                        }
                    }
                    .map_err(string_error)?;
                }
                refresh_playlist(&source, database, Some(id), None)
                    .await
                    .map_err(string_error)?;
            }
        }
        if local {
            local_playlist_changed(&owner, source_key, Some(playlist)).await;
        }
        Ok(songs)
    })
}

pub fn can_remove_unavailable(playlist: &library::PlaylistRow) -> bool {
    !playlist.source_owned && playlist.writable && playlist.source_id.is_none()
}

pub fn remove_unavailable_tracks(
    owner: &SourceOwner,
    playlist: PlaylistKey,
) -> Receiver<Result<usize, String>> {
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let result = async {
            let (PlaylistOwner::Local(source), _) =
                playlist_source(&owner, playlist, Some(true)).await?
            else {
                return Err("This playlist belongs to a server".to_string());
            };
            let database = &owner.shared.database;
            let mut removals = library::PlaylistEntryRemovals::new().map_err(string_error)?;
            let mut after = -1;
            loop {
                let page = database
                    .playlist_entry_uri_page(playlist, after)
                    .await
                    .map_err(string_error)?;
                let Some(last) = page.last() else { break };
                after = last.1;
                let uris: Vec<_> = page.iter().map(|(_, _, uri)| uri.clone()).collect();
                let missing = owner.missing_media(&uris).await?;
                for (entry, _, uri) in page {
                    if missing.contains(&uri) {
                        removals.push(entry).map_err(string_error)?;
                    }
                }
            }
            let removed = removals
                .apply(database, source, playlist)
                .await
                .map_err(string_error)?;
            if removed > 0 {
                local_playlist_changed(&owner, source, Some(playlist)).await;
            }
            Ok(removed)
        }
        .await;
        if let Err(error) = &result {
            owner.shared.warn_nonfatal(error);
        }
        let _ = sender.send(result).await;
    });
    receiver
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
        move |target, id, database| async move {
            match target {
                PlaylistOwner::Local(source_key) => database
                    .move_playlist_entry(source_key, playlist, entry, position)
                    .await
                    .map_err(SourceError::from),
                PlaylistOwner::Server(source, _) => {
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
                    match &source.implementation {
                        Implementation::OpenSubsonic(provider) => {
                            provider
                                .move_playlist_entry(&id, &entry.object_id, position)
                                .await?
                        }
                    }
                    refresh_playlist(&source, &database, Some(id), None)
                        .await
                        .map(|()| true)
                }
            }
        },
    )
}

enum PlaylistOwner {
    Local(Option<SourceKey>),
    Server(Arc<Source>, SourceKey),
}

impl PlaylistOwner {
    fn source_key(&self) -> Option<SourceKey> {
        match self {
            Self::Local(key) => *key,
            Self::Server(_, key) => Some(*key),
        }
    }
}

fn playlist_change<F, Work>(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: bool,
    change: F,
) -> Receiver<Result<bool, String>>
where
    F: FnOnce(PlaylistOwner, String, Arc<Database>) -> Work + Send + 'static,
    Work: Future<Output = Result<bool, SourceError>> + Send + 'static,
{
    let (sender, receiver) = async_channel::bounded(1);
    owner.spawn_serialized(move |owner| async move {
        let target = playlist_source(&owner, playlist, Some(edit_tracks)).await;
        let source_key = target
            .as_ref()
            .ok()
            .and_then(|(target, _)| target.source_key());
        let local = matches!(&target, Ok((PlaylistOwner::Local(_), _)));
        let result = match target {
            Ok((target, id)) => change(target, id, Arc::clone(&owner.shared.database)).await,
            Err(error) => Err(SourceError::Other(error)),
        };
        match &result {
            Ok(true) => {
                if local {
                    local_playlist_changed(&owner, source_key, Some(playlist)).await;
                }
            }
            Err(error) => owner.shared.warn_nonfatal(&error.to_string()),
            Ok(false) => {}
        }
        let _ = sender.try_send(result.map_err(string_error));
    });
    receiver
}

async fn playlist_source(
    owner: &SourceOwner,
    playlist: PlaylistKey,
    edit_tracks: Option<bool>,
) -> Result<(PlaylistOwner, String), String> {
    let access = owner
        .shared
        .database
        .playlist_access(playlist, &ReadCancellation::new())
        .await
        .map_err(string_error)?
        .ok_or("Playlist no longer exists")?;
    if access.source_owned
        && edit_tracks.is_some_and(|tracks| {
            if tracks {
                !access.writable
            } else {
                !access.metadata_writable
            }
        })
    {
        return Err("The server does not allow this playlist change".into());
    }
    let target = if access.source_owned {
        let key = access.source_key.ok_or("Playlist owner is unavailable")?;
        let id = access.source_id.ok_or("Playlist owner is unavailable")?;
        PlaylistOwner::Server(owner.client(&SourceId::new(id))?, key)
    } else {
        PlaylistOwner::Local(access.source_key)
    };
    Ok((target, access.object_id))
}

fn configured_playlist_owner(
    owner: &SourceOwner,
    id: &SourceId,
    key: SourceKey,
) -> Result<PlaylistOwner, String> {
    owner
        .client(id)
        .map(|source| PlaylistOwner::Server(source, key))
}

async fn local_playlist_changed(
    owner: &SourceOwner,
    source: Option<SourceKey>,
    playlist: Option<PlaylistKey>,
) {
    if let Err(error) = owner
        .shared
        .database
        .refresh_incomplete_playlist_artwork(playlist, false)
        .await
    {
        owner.shared.warn_nonfatal(&error.to_string());
    }
    owner
        .shared
        .send(SourceEvent::CatalogPublished(CatalogPublication {
            source_key: source,
            favorite: None,
            change: CatalogChange::Playlists(playlist),
        }))
        .await;
}

pub fn smart_playlist_display_name(object_id: &str, name: &str) -> String {
    match object_id {
        "builtin:most_played" => "Most Played".to_string(),
        "builtin:never_played" => "Never Played".to_string(),
        "builtin:most_skipped" => "Most Skipped".to_string(),
        _ => name.to_owned(),
    }
}

pub async fn playlist_folders_changed(owner: &SourceOwner) {
    owner
        .shared
        .send(SourceEvent::CatalogPublished(CatalogPublication {
            source_key: None,
            favorite: None,
            change: CatalogChange::Playlists(None),
        }))
        .await;
}

pub async fn create_playlist_folder(
    owner: &SourceOwner,
    name: String,
    smart: bool,
) -> sources::SourceResult<library::PlaylistFolderKey> {
    owner
        .reply(move |owner, database| async move {
            let key = database.create_playlist_folder(&name, smart).await?;
            playlist_folders_changed(&owner).await;
            Ok::<_, SourceError>(key)
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

pub async fn rename_playlist_folder(
    owner: &SourceOwner,
    key: library::PlaylistFolderKey,
    name: String,
) -> sources::SourceResult<()> {
    owner
        .reply(move |owner, database| async move {
            database.rename_playlist_folder(key, &name).await?;
            playlist_folders_changed(&owner).await;
            Ok::<_, SourceError>(())
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

pub async fn delete_playlist_folder(
    owner: &SourceOwner,
    key: library::PlaylistFolderKey,
) -> sources::SourceResult<()> {
    owner
        .reply(move |owner, database| async move {
            let folder = database.delete_playlist_folder(key).await?;
            playlist_folders_changed(&owner).await;
            if let Some((folder_id, smart)) = folder {
                let settings = owner.shared.settings.clone();
                tokio::task::spawn_blocking(move || {
                    settings.update(|stored| {
                        stored.ui.sidebar.set_pinned(
                            crate::settings::SidebarPin::PlaylistFolder { folder_id, smart },
                            false,
                        );
                        Ok(())
                    })
                })
                .await
                .map_err(|error| SourceError::Other(error.to_string()))?
                .map_err(SourceError::Other)?;
            }
            Ok::<_, SourceError>(())
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

pub async fn move_playlist_to_folder(
    owner: &SourceOwner,
    member: library::PlaylistFolderMember,
    folder: Option<library::PlaylistFolderKey>,
) -> sources::SourceResult<()> {
    owner
        .reply(move |owner, database| async move {
            database.move_playlist_to_folder(member, folder).await?;
            playlist_folders_changed(&owner).await;
            Ok::<_, SourceError>(())
        })
        .recv()
        .await
        .map_err(|error| SourceError::Other(error.to_string()))?
}

#[derive(Clone)]
pub enum SmartPlaylistChange {
    Create {
        name: String,
        definition: library::SmartPlaylistDefinition,
    },
    Update {
        key: library::SmartPlaylistKey,
        name: String,
        definition: library::SmartPlaylistDefinition,
        artwork: Option<sources::ArtworkChange>,
    },
    Delete(library::SmartPlaylistKey),
}

async fn append_native_tracks(
    source: &Source,
    playlist: &str,
    tracks: &[String],
) -> Result<(), SourceError> {
    for tracks in tracks.chunks(256) {
        match &source.implementation {
            Implementation::OpenSubsonic(provider) => {
                provider.add_playlist_tracks(playlist, tracks).await?
            }
        }
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
        if matches!(source.implementation, Implementation::OpenSubsonic(_)) {
            format!("playlist:{raw}")
        } else {
            raw.to_owned()
        }
    };
    source
        .apply_items(
            database,
            affected.into_iter().map(object).collect(),
            deleted.into_iter().map(object).collect(),
        )
        .await
}
