//! Constructs Rufin's product owners and connects their concrete event lanes.

use std::sync::Arc;

use crate::runtime::{DiagnosticsHandle, ProductHandles, ProductReceivers, RuntimeInputs};
use ::scrobbling::Scrobbler;
use async_channel::unbounded;
use playback::PlaybackBackend;
use secrets::SwitchableSecretStore;
use tracing::warn;

use crate::paths::Paths;
use crate::playback::PlaybackOwner;
use crate::scrobbling::ScrobblingOwner;
use crate::settings::{SettingsFile, SettingsOwner, platform_secret_store};
use crate::source::{SourceOutputs, SourceOwner};

/// Runs an application host with Rufin's worker runtime available on its calling thread.
/// The host shuts down playback before returning; workers are stopped afterwards.
pub fn with_runtime<T>(host: impl FnOnce(&tokio::runtime::Runtime) -> T) -> std::io::Result<T> {
    let runtime = worker_runtime()?;
    let result = {
        let _guard = runtime.enter();
        host(&runtime)
    };
    runtime.shutdown_timeout(std::time::Duration::from_secs(1));
    Ok(result)
}

pub fn worker_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rufin-async")
        .build()
}

pub async fn runtime_inputs<StartBackend>(
    diagnostics: DiagnosticsHandle,
    settings: SettingsFile,
    paths: Paths,
    start_backend: StartBackend,
) -> Result<RuntimeInputs, String>
where
    StartBackend: Fn(
            tokio::sync::watch::Receiver<playback::ProcessingRequest>,
        ) -> Result<Box<dyn PlaybackBackend>, String>
        + Send
        + Sync
        + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    let secret_storage_fallbacks = settings.secret_storage_fallbacks.1.clone();
    let stored = settings.load();
    let secrets = Arc::new(SwitchableSecretStore::new(platform_secret_store(&settings)));
    let store_path = paths.store_file();
    let (database, temporary_store) = match library::Database::open(&store_path).await {
        Ok(database) => (database, false),
        Err(error) if error.is_store_unavailable() => {
            warn!(%error, path=%store_path.display(), "could not access the durable Store; startup will continue with temporary storage");
            let directory = tempfile::Builder::new()
                .prefix("rufin-store-")
                .tempdir()
                .map_err(string_error)?
                .keep();
            (
                library::Database::open(directory.join("store.sqlite"))
                    .await
                    .map_err(string_error)?,
                true,
            )
        }
        Err(error) => return Err(string_error(error)),
    };
    let library = Arc::new(database);
    library
        .set_distinct_track_covers(stored.ui.application.prefer_distinct_track_covers)
        .await
        .map_err(string_error)?;
    let scrobbling_library = library.as_ref().clone();
    let scrobbling_runtime = runtime.clone();
    let scrobbling_settings = stored.scrobbling_runtime_settings();
    let scrobbler = Arc::new(
        runtime
            .spawn_blocking(move || {
                Scrobbler::new(scrobbling_library, scrobbling_runtime, scrobbling_settings)
            })
            .await
            .map_err(string_error)??,
    );

    let (source_events, source_receiver) = unbounded();
    let (download_events, download_receiver) = unbounded();
    let artwork = match artwork::Artwork::new(paths.artwork_dir(), runtime.clone()) {
        Ok(artwork) => artwork,
        Err(error) => {
            warn!(%error, "could not use the artwork cache; startup will continue with a temporary cache");
            artwork::Artwork::new(temporary_artwork_dir(), runtime.clone()).map_err(string_error)?
        }
    };
    let downloads = downloads::Downloads::new(
        artwork.clone(),
        paths.downloads_dir(),
        library.as_ref().clone(),
        runtime.clone(),
        download_events,
        stored.ui.downloads.clone(),
    );

    let source = SourceOwner::open_dormant(
        artwork.clone(),
        library.clone(),
        downloads.clone(),
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        SourceOutputs {
            events: source_events.clone(),
        },
    );
    if let Err(error) = artwork.install_database(Arc::clone(&library)).await {
        warn!(%error, "could not import cached artwork previews");
    }
    if !stored.ui.recent_search_results.is_empty() {
        let uris = stored
            .ui
            .recent_search_results
            .iter()
            .map(|row| row.media_uri.clone())
            .collect::<Vec<_>>();
        match library
            .artwork_bindings(&uris, &library::ReadCancellation::new())
            .await
        {
            Ok(bindings) => {
                if let Err(error) = settings.update(|stored| {
                    for (recent, binding) in
                        stored.ui.recent_search_results.iter_mut().zip(bindings)
                    {
                        if binding.is_some() {
                            recent.artwork_binding = binding;
                        }
                    }
                    Ok(())
                }) {
                    warn!(%error, "could not update recent search artwork");
                }
            }
            Err(error) => warn!(%error, "could not read recent search artwork"),
        }
    }
    let artwork_source = Arc::downgrade(&source);
    artwork.install_source_resolver(move |source_id| {
        artwork_source
            .upgrade()
            .and_then(|owner| owner.client(source_id).ok())
    });
    let playback = PlaybackOwner::new(
        library.clone(),
        settings.clone(),
        runtime.clone(),
        Arc::clone(&scrobbler),
        start_backend,
    );
    playback.install_source_owner(&source);
    playback.start().await?;
    let scrobbling = ScrobblingOwner::new(
        settings.clone(),
        Arc::clone(&secrets),
        runtime.clone(),
        scrobbler,
    );
    scrobbling.start();

    source.attach_playback(&playback);

    let settings_playback = Arc::clone(&playback);
    let settings_downloads = downloads.clone();
    let settings_source = Arc::clone(&source);
    let settings_scrobbling = Arc::clone(&scrobbling);
    let settings_library = Arc::clone(&library);
    let settings_runtime = runtime.clone();
    let settings_handle = SettingsOwner::new(
        settings.clone(),
        Arc::clone(&secrets),
        move |previous, current, credentials_changed| {
            if previous.ui.application.prefer_distinct_track_covers
                != current.ui.application.prefer_distinct_track_covers
            {
                let library = Arc::clone(&settings_library);
                let enabled = current.ui.application.prefer_distinct_track_covers;
                settings_runtime.spawn(async move {
                    if let Err(error) = library.set_distinct_track_covers(enabled).await {
                        warn!(%error, "could not update artwork preference");
                    }
                });
            }
            if previous.ui.playback != current.ui.playback {
                settings_playback.playback_settings_changed(current.ui.playback.clone());
            }
            if previous.ui.application.auto_dj_refill_threshold
                != current.ui.application.auto_dj_refill_threshold
            {
                settings_playback.auto_dj_threshold_changed(
                    current.ui.auto_dj_enabled,
                    current.ui.application.auto_dj_refill_threshold,
                );
            }
            if previous.scrobbling != current.scrobbling || credentials_changed {
                settings_scrobbling.settings_changed(credentials_changed);
            }
            if !previous.ui.application.download_artwork_automatically
                && current.ui.application.download_artwork_automatically
            {
                settings_source.request_asset_downloads();
            }
            if previous.ui.downloads != current.ui.downloads {
                settings_downloads.settings_changed(current.ui.downloads.clone());
            }
        },
    );

    let stored = settings.load();

    if let Some(configured) = &stored.sources.configured {
        if let Err(error) = library
            .reconcile_source(
                &configured.configuration.source_id,
                configured.configuration.kind.source_id(),
                &configured.configuration.name,
            )
            .await
        {
            warn!(%error, source_id=%configured.configuration.source_id, "could not reconcile configured source mapping");
        }
    }
    source.start()?;
    let source_handle: crate::runtime::SourceHandle = source.clone();

    Ok(RuntimeInputs {
        temporary_store,
        secret_storage_fallbacks,
        diagnostics,
        products: ProductHandles {
            settings: Arc::clone(&settings_handle),
            appearance: tokio::sync::watch::channel(crate::themes::controller_colors(
                &stored.ui,
                &paths.config_dir().join("themes"),
            ))
            .0,
            library,
            runtime,
            source: source_handle,
            downloads,
            playback,
            artwork,
            scrobbling,
        },
        settings: settings_handle,
        receivers: ProductReceivers {
            source: source_receiver,
            downloads: download_receiver,
        },
        configured_sources: source.list_sources(),
        source_operation: source.operation().borrow().clone(),
    })
}

pub fn startup_settings(paths: &Paths) -> SettingsFile {
    if let Err(error) = paths.prepare() {
        warn!(%error, "could not prepare every Slopsonic data directory");
    }
    SettingsFile::open(paths.settings_file()).unwrap_or_else(|error| {
        warn!(%error, "could not use saved settings; startup will continue with defaults");
        SettingsFile::memory_at(paths.config_dir())
    })
}

fn string_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn temporary_artwork_dir() -> std::path::PathBuf {
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("rufin-artwork-{}-{started}", std::process::id()))
}
