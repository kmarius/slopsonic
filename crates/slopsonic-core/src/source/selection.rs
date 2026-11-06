use super::*;

impl SourceOwner {
    pub(super) async fn publish_operation(&self, operation: SourceOperation) {
        self.shared.publish_operation(operation);
    }

    pub(super) async fn select_now(
        &self,
        source_id: SourceId,
        cancelled: Arc<AtomicBool>,
        refresh: bool,
    ) -> Result<(), String> {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let configured = configured_source(&self.shared.settings.load().sources, &source_id)?;
        let publication = match self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
            .map_err(string_error)?
        {
            Some(cached) => cached,
            None => self
                .shared
                .database
                .reconcile_source(
                    &source_id,
                    configured.configuration.kind.source_id(),
                    &configured.configuration.name,
                )
                .await
                .map_err(string_error)?,
        };
        let cached_start = publication.catalog_complete;
        self.install_selected(
            configured.clone(),
            None,
            publication,
            Arc::clone(&cancelled),
        )
        .await?;
        if !self.shared.selection_is_current(&cancelled) {
            return Ok(());
        }
        if self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&source_id)
            .is_some_and(|cached| cached.observer.is_some())
        {
            return Ok(());
        }
        let Some(selected) = self.selected_library() else {
            return Ok(());
        };
        let owner = self.clone();
        let task = self.shared.runtime.spawn(async move {
            let source = owner.open_client(source_id.clone()).await;
            if !owner.shared.selection_is_current(&cancelled) {
                return;
            }
            let Some(selected) = owner
                .selected_library()
                .filter(|current| current.source_key == selected.source_key)
            else {
                return;
            };
            if let Err(error) = owner
                .shared
                .downloads
                .attach(
                    source_id.clone(),
                    selected.source_key,
                    source.as_ref().ok().cloned(),
                    selected.music_folder_key,
                )
                .await
            {
                owner.shared.warn_nonfatal(&error);
            }
            if !owner.shared.selection_is_current(&cancelled) {
                return;
            }
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    owner.shared.warn_nonfatal(&error);
                    return;
                }
            };
            let activated =
                owner.start_observer(selected.clone(), source.clone(), refresh && !cached_start);
            owner.shared.present_refresh(&source_id);
            if refresh && activated && !cached_start {
                let refresh_owner = owner.clone();
                let refresh_source = source.clone();
                owner.shared.runtime.spawn(async move {
                    let _ = refresh_owner
                        .refresh_now(&source_id, &refresh_source, false, true)
                        .await;
                });
            }
            if let Ok(playback) = owner.shared.playback() {
                playback.catalog_changed();
            }
        });
        *self
            .shared
            .connection
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(task.abort_handle());
        Ok(())
    }

    pub(super) async fn selected_folders(
        &self,
        configured: &ConfiguredSource,
        source_key: SourceKey,
    ) -> Result<(Arc<[FolderChoice]>, Option<FolderKey>), String> {
        let folders = self
            .shared
            .database
            .music_folders(source_key)
            .await
            .map_err(string_error)?;
        let selected = match configured.music_folder_id.as_deref() {
            Some(object_id) => self
                .shared
                .database
                .folder_identity_key(source_key, object_id)
                .await
                .map_err(string_error)?,
            None => None,
        };
        Ok((folders.into(), selected))
    }

    pub(super) async fn install_selected(
        &self,
        configured: ConfiguredSource,
        mut source: Option<Arc<Source>>,
        source_state: library::CachedSource,
        acquisition: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (music_folders, music_folder_key) = self
            .selected_folders(&configured, source_state.source)
            .await?;
        let selected = SelectedLibrary {
            source_id: configured.configuration.source_id.clone(),
            source_key: source_state.source,
            music_folder_key,
            music_folder_object_id: configured.music_folder_id.clone(),
            music_folders,
        };
        if !self.shared.selection_is_current(&acquisition) {
            return Ok(());
        }
        self.cancel_selected_jobs();
        source = source.map(|source| self.cache_connected(&configured, source));
        *self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(selected.clone());
        self.shared
            .send(SourceEvent::Selected {
                configured: self.shared.source_status(Some(&selected)),
                selected: selected.clone(),
            })
            .await;
        self.publish_operation(SourceOperation::Idle).await;
        self.shared.present_refresh(&selected.source_id);
        if let Some(source) = source {
            self.start_observer(selected.clone(), source.clone(), true);
            let owner = self.clone();
            self.shared.runtime.spawn(async move {
                if let Err(error) = owner
                    .shared
                    .downloads
                    .attach(
                        selected.source_id.clone(),
                        selected.source_key,
                        Some(source),
                        selected.music_folder_key,
                    )
                    .await
                {
                    owner.shared.warn_nonfatal(&error);
                }
            });
        }
        Ok(())
    }

    pub(super) async fn release_selected(&self) {
        self.shared.cancel_selection();
        self.cancel_selected_jobs();
        let released = self
            .shared
            .selected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .is_some();
        if released {
            self.shared.send(SourceEvent::ReleaseSelected).await;
        }
    }

    pub(super) fn cancel_selected_jobs(&self) {
        if let Some(job) = self
            .shared
            .connection
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            job.abort();
        }
    }

    pub(super) fn start_observer(
        &self,
        selected: SelectedLibrary,
        source: Arc<Source>,
        initial_scan_pending: bool,
    ) -> bool {
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(cached) = clients.get_mut(&selected.source_id) else {
            return false;
        };
        if cached.observer.is_some() {
            return false;
        }
        let observer = source.start_selected_feed(&self.shared.runtime, initial_scan_pending);
        let shared = Arc::downgrade(&self.shared);
        let task = self.shared.runtime.spawn(Self::consume_selected_feed(
            shared,
            selected.source_id,
            selected.source_key,
            source,
            observer.clone(),
        ));
        cached.observer = Some((observer, task.abort_handle()));
        true
    }

    pub(super) async fn consume_selected_feed(
        shared: Weak<Shared>,
        source_id: SourceId,
        source_key: SourceKey,
        source: Arc<Source>,
        observer: Arc<SelectedFeed>,
    ) {
        let mut interval = tokio::time::interval(SOURCE_CHECK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            let event = tokio::select! {
                changed = observer.wait_for_change() => { if !changed { return; } true },
                _ = interval.tick() => false,
            };
            if observer.cancellation().load(Ordering::Acquire) {
                return;
            }
            let Some(shared) = shared.upgrade() else {
                observer.cancel();
                return;
            };
            let owner = Self { shared };
            if owner.shared.is_refreshing(&source_id) {
                continue;
            }
            if !event {
                owner.check_remote_freshness(&source_id, &source).await;
                continue;
            }
            match observer
                .apply_pending(&owner.shared.database, source_key)
                .await
            {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    if source_error_is_cancelled(&error) {
                        return;
                    }
                    warn!(%error, "bounded source change failed; refreshing after feed boundary loss");
                }
            }
            let _ = owner
                .refresh_now(&source_id, &source, true, !source.needs_periodic_refresh())
                .await;
        }
    }

    pub(super) async fn refresh_now(
        &self,
        source_id: &SourceId,
        source: &Arc<Source>,
        automatic: bool,
        force: bool,
    ) -> Result<(), String> {
        if source.is_retired() {
            return Err(SourceError::Cancelled.to_string());
        }
        let Some(token) = self.shared.begin_refresh(source_id, !automatic, !automatic) else {
            return Ok(());
        };
        if !automatic {
            self.shared.present_refresh(source_id);
        }
        let progress = |value| self.shared.refresh_progress(source_id, &token, value);
        let outcome = source
            .acquire_catalog(&self.shared.database, &progress, token.clone(), force)
            .await;
        let outcome = outcome.map(|_| ());
        let result = outcome
            .as_ref()
            .map(|_| ())
            .map_err(source_refresh_error_text);
        self.finish_source_refresh(source_id, &token, outcome).await;
        result
    }

    pub(crate) async fn finish_source_refresh(
        &self,
        source_id: &SourceId,
        token: &Arc<AtomicBool>,
        result: sources::SourceResult<()>,
    ) {
        if !self.shared.refresh_is_current(source_id, token) {
            return;
        }
        if result.is_ok() {
            self.shared.maintenance.notify_one();
        }
        {
            let mut clients = self
                .shared
                .clients
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = clients.get_mut(source_id)
                && cached
                    .refresh
                    .as_ref()
                    .is_some_and(|refresh| Arc::ptr_eq(&refresh.cancelled, token))
            {
                cached.refresh = None;
            }
        }
        let observer = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .and_then(|cached| {
                cached
                    .observer
                    .as_ref()
                    .map(|(observer, _)| observer.clone())
            });
        if let Some(observer) = observer {
            observer.wake_pending();
        }
        let presented = matches!(&*self.shared.operation.borrow(),
            SourceOperation::Refreshing { source_id: current, .. } if current == source_id);
        if presented {
            self.finish_refresh(source_id, result).await;
        } else if let Err(error) = result
            && !source_error_is_cancelled(&error)
        {
            warn!(%error, "background source refresh failed");
        }
    }

    pub(crate) async fn finish_refresh(
        &self,
        source_id: &SourceId,
        result: sources::SourceResult<()>,
    ) {
        let operation = match result {
            Ok(_) => SourceOperation::Idle,
            Err(error) if source_error_is_cancelled(&error) => SourceOperation::Idle,
            Err(error) => SourceOperation::Failed {
                source_id: Some(source_id.clone()),
                message: {
                    warn!(%error, "source refresh failed");
                    source_refresh_error_text(&error)
                },
                add_form: false,
            },
        };
        self.publish_operation(operation).await;
    }

    pub(super) async fn install_connected_edit(
        &self,
        configured: ConfiguredSource,
        connected: Box<sources::ConnectedSource>,
    ) -> Result<(), String> {
        let (configuration, source, credential) = (*connected).into_parts();
        let result = async {
            let current = configured_source(
                &self.shared.settings.load().sources,
                &configured.configuration.source_id,
            )?;
            if current.configuration != configured.configuration
                || current.credential != configured.credential
            {
                return Err(SourceError::Cancelled.to_string());
            }
            let previous = configured.clone();
            let mut replacement = configured;
            replacement.configuration = configuration;
            if !same_native_input(&previous.configuration, &replacement.configuration)
                || credential.is_some()
            {
                self.shared
                    .database
                    .invalidate_source_observation(&replacement.configuration.source_id)
                    .await
                    .map_err(string_error)?;
            }
            replacement = self.persist_connected_source(&replacement, credential)?;
            let source = self.cache_connected(&replacement, Arc::new(source));
            let publication = self
                .shared
                .database
                .reconcile_source(
                    &replacement.configuration.source_id,
                    replacement.configuration.kind.source_id(),
                    &replacement.configuration.name,
                )
                .await
                .map_err(string_error)?;
            let selected = self
                .shared
                .selected()
                .is_some_and(|selected| selected.source_id == replacement.configuration.source_id);
            let refresh = selected;
            if selected && !self.shared.operation.borrow().blocks_library() {
                let intent = self.shared.begin_selection();
                self.install_selected(replacement, Some(Arc::clone(&source)), publication, intent)
                    .await?;
            }
            if refresh {
                let id = source.source_id().clone();
                let owner = self.clone();
                self.shared.runtime.spawn(async move {
                    if owner.configuration(&id).is_some() {
                        let _ = owner.refresh_now(&id, &source, false, true).await;
                    }
                });
            } else {
                self.publish_operation(SourceOperation::Idle).await;
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.publish_operation(SourceOperation::Idle).await;
        }
        result
    }

    pub(super) fn cache_connected(
        &self,
        configured: &ConfiguredSource,
        source: Arc<Source>,
    ) -> Arc<Source> {
        let mut clients = self
            .shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(cached) = clients.get(&configured.configuration.source_id)
            && cached.matches(configured)
        {
            return cached.source.clone();
        }
        clients.insert(
            configured.configuration.source_id.clone(),
            CachedSource {
                configuration: configured.configuration.clone(),
                credential: configured.credential.clone(),
                source: source.clone(),
                refresh: None,
                observer: None,
            },
        );
        source
    }

    pub(crate) fn persist_connected_source(
        &self,
        configured: &ConfiguredSource,
        credential: Option<String>,
    ) -> Result<ConfiguredSource, String> {
        let mut configured = configured.clone();
        if let Some(secret) = credential {
            if configured
                .credential
                .as_ref()
                .and_then(|credential| {
                    load_provider_secret_or_none(self.shared.secrets.as_ref(), credential)
                })
                .as_deref()
                != Some(secret.as_str())
            {
                configured.credential =
                    Some(seal_credential(self.shared.secrets.as_ref(), &secret)?);
            }
        }
        self.shared.settings.update(|stored| {
            stored.sources.configured = Some(configured.clone());
            Ok(())
        })?;
        self.shared
            .clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|id, cached| {
                id != &configured.configuration.source_id || cached.matches(&configured)
            });
        Ok(configured)
    }

    pub(super) async fn edit_configured_source(
        &self,
        source_id: SourceId,
        input: SourceSetup,
    ) -> Result<(), String> {
        let configured = configured_source(&self.shared.settings.load().sources, &source_id)?;
        let credential = configured.credential.as_ref().and_then(|reference| {
            load_provider_secret_or_none(self.shared.secrets.as_ref(), reference)
        });
        let edit = Source::edit(configured.configuration.clone(), credential, input)
            .await
            .map_err(string_error)?;
        match edit {
            sources::SourceEditResult::Unchanged => return Ok(()),
            sources::SourceEditResult::ConfigurationOnly(configuration) => {
                let current = configured_source(&self.shared.settings.load().sources, &source_id)?;
                if current.configuration != configured.configuration
                    || current.credential != configured.credential
                {
                    return Err(SourceError::Cancelled.to_string());
                }
                if !same_native_input(&configured.configuration, &configuration) {
                    self.shared
                        .database
                        .invalidate_source_observation(&source_id)
                        .await
                        .map_err(string_error)?;
                    self.shared
                        .clients
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&source_id);
                }
                self.shared.settings.update(|stored| {
                    if let Some(item) = stored
                        .sources
                        .configured
                        .iter_mut()
                        .find(|item| item.configuration.source_id == source_id)
                    {
                        item.configuration = configuration.clone();
                    }
                    Ok(())
                })?;
                self.shared
                    .database
                    .reconcile_source(
                        &source_id,
                        configuration.kind.source_id(),
                        &configuration.name,
                    )
                    .await
                    .map_err(string_error)?;
                Ok(())
            }
            sources::SourceEditResult::Connected(connected) => {
                self.install_connected_edit(configured, connected).await
            }
        }?;
        self.shared
            .send(SourceEvent::Configured(
                self.shared.source_status(self.shared.selected().as_ref()),
            ))
            .await;
        self.request_asset_downloads();
        Ok(())
    }

    pub(super) async fn catalog_updated(
        &self,
        source_id: SourceId,
        source: library::SourceKey,
        update: CatalogUpdate,
    ) {
        let source_id = &source_id;
        if !update.track_rekeys.is_empty()
            && let Ok(playback) = self.shared.playback()
        {
            playback.rekey_tracks(update.track_rekeys.clone());
        }
        if !update.changed() {
            return;
        }
        let change = if update.metadata
            || update.artwork
            || (update.playlists && (update.home || update.activity))
        {
            CatalogChange::Broad
        } else if update.playlists {
            CatalogChange::Playlists(None)
        } else if update.activity {
            CatalogChange::Activity
        } else {
            CatalogChange::Home
        };
        {
            if update.metadata
                && let Some(selected) = self.shared.selected()
                && selected.source_key == source
                && let Ok(configured) =
                    configured_source(&self.shared.settings.load().sources, source_id)
            {
                match self.selected_folders(&configured, source).await {
                    Ok((folders, folder_key)) => {
                        if let Some(current) = self
                            .shared
                            .selected
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_mut()
                            .filter(|current| current.source_key == source)
                        {
                            current.music_folders = folders;
                            current.music_folder_key = folder_key;
                            current.music_folder_object_id = configured.music_folder_id;
                        }
                    }
                    Err(error) => self.shared.warn_nonfatal(&error),
                }
            }
        }
        if update.metadata || update.artwork || update.playlists {
            self.request_asset_downloads();
        }
        if update.metadata || update.playlists {
            self.shared.downloads.library_changed(source_id.clone());
        }
        if update.metadata {
            if let Err(error) = self.shared.refresh_source_counts(source_id, source).await {
                self.shared.warn_nonfatal(&error);
            }
            let _ = self
                .shared
                .outputs
                .events
                .send(SourceEvent::Configured(
                    self.shared.source_status(self.shared.selected().as_ref()),
                ))
                .await;
        }
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(source),
                favorite: None,
                change,
            }))
            .await;
    }

    pub(crate) async fn artwork_invalidated(&self, source_id: &SourceId, change: CatalogChange) {
        let source = match self
            .shared
            .database
            .cached_source(source_id.as_str(), &ReadCancellation::new())
            .await
        {
            Ok(Some(source)) => source.source,
            Ok(None) => return,
            Err(error) => {
                self.shared.warn_nonfatal(&error.to_string());
                return;
            }
        };
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(source),
                favorite: None,
                change,
            }))
            .await;
    }

    pub(super) async fn check_remote_freshness(&self, source_id: &SourceId, source: &Arc<Source>) {
        if source.needs_periodic_refresh() {
            let _ = self.refresh_now(source_id, source, true, false).await;
        }
    }

    pub(super) async fn publish_catalog(
        &self,
        selected: &SelectedLibrary,
        favorite: Option<FavoriteSettlement>,
        change: CatalogChange,
    ) {
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: Some(selected.source_key),
                favorite,
                change,
            }))
            .await;
    }

    pub(crate) async fn publish_current_catalog(
        &self,
        favorite: Option<FavoriteSettlement>,
        change: CatalogChange,
    ) {
        self.shared
            .send(SourceEvent::CatalogPublished(CatalogPublication {
                source_key: self.shared.selected().map(|selected| selected.source_key),
                favorite,
                change,
            }))
            .await;
    }
}
