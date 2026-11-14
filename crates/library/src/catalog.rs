//! Canonical observations and concrete source membership writes.
use crate::{Database, LibraryError, LibraryResult, ReadCancellation};
use sqlx::{Connection, FromRow, SqliteConnection};
use std::sync::Arc;
pub(crate) mod artwork;
pub(crate) mod cache;
pub(crate) mod collections;
pub(crate) mod index;
mod observations;
pub(crate) use cache::remember_track;
pub use observations::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Freshness(Vec<u8>);

impl Freshness {
    pub fn new(marker: impl Into<Vec<u8>>) -> LibraryResult<Self> {
        let marker = marker.into();
        if marker.len() > 64 * 1024 {
            return Err(LibraryError::InvalidRequest(
                "freshness marker exceeds 64 KiB".into(),
            ));
        }
        Ok(Self(marker))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogUpdate {
    pub metadata: bool,
    pub activity: bool,
    pub playlists: bool,
    pub artwork: bool,
    pub home: bool,
    pub track_rekeys: Arc<[(crate::TrackKey, crate::TrackKey)]>,
}

impl CatalogUpdate {
    pub fn changed(&self) -> bool {
        self.metadata || self.activity || self.playlists || self.artwork || self.home
    }

    pub fn merge(&mut self, other: Self) {
        self.metadata |= other.metadata;
        self.activity |= other.activity;
        self.playlists |= other.playlists;
        self.artwork |= other.artwork;
        self.home |= other.home;
        if !other.track_rekeys.is_empty() {
            let mut pairs = self.track_rekeys.to_vec();
            pairs.extend_from_slice(&other.track_rekeys);
            self.track_rekeys = pairs.into();
        }
    }
}

pub struct Catalog<'a> {
    pub database: &'a Database,
    pub observed: Option<i64>,
    pub(crate) changes: CatalogUpdate,
}

impl Catalog<'_> {
    pub fn changes(&self) -> &CatalogUpdate {
        &self.changes
    }

    pub fn into_changes(self) -> CatalogUpdate {
        self.changes
    }
}

#[derive(Clone, Debug, FromRow, Eq, PartialEq)]
pub struct CachedSource {
    pub display_name: String,
    pub catalog_complete: bool,
}

impl Database {
    pub async fn catalog(&self, observed: Option<i64>) -> LibraryResult<Catalog<'_>> {
        if !self.has_server().await? {
            return Err(LibraryError::InvalidRequest(
                "source is no longer configured".into(),
            ));
        }
        Ok(Catalog {
            database: self,
            observed,
            changes: CatalogUpdate::default(),
        })
    }

    pub async fn publish_catalog_update(&self, update: CatalogUpdate) {
        if !update.changed() && update.track_rekeys.is_empty() {
            return;
        }
        match self.has_server().await {
            Ok(true) => self.catalog_updated(update).await,
            Ok(false) => {
                tracing::warn!("could not publish catalog update for removed source")
            }
            Err(error) => tracing::warn!(%error,"could not read source for catalog update"),
        }
    }

    pub async fn reconcile_source(&self, kind: &str, name: &str) -> LibraryResult<CachedSource> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        crate::db::write_source_identity(&mut transaction).await?;
        sqlx::query("UPDATE sources SET kind=?1,display_name=?2,normalized_name=?3,distinct_track_covers=?4 WHERE (kind,display_name,normalized_name,distinct_track_covers) IS NOT (?1,?2,?3,?4)")
            .bind(kind).bind(name).bind(name.to_lowercase()).bind(self.distinct_track_covers()).execute(&mut *transaction).await?;
        let row = sqlx::query_as("SELECT display_name,catalog_complete FROM sources")
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn cached_source(
        &self,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Option<CachedSource>> {
        let mut connection = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
            connection = self.acquire_reader() => connection?,
        };
        Ok(
            sqlx::query_as("SELECT display_name,catalog_complete FROM sources")
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    pub async fn accept_freshness(
        &self,
        freshness: &Freshness,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<bool> {
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sources WHERE catalog_complete AND freshness=?1)",
        )
        .bind(freshness.as_bytes())
        .fetch_one(&mut *connection)
        .await?)
    }

    pub async fn invalidate_source_observation(&self) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        sqlx::query("UPDATE sources SET observation_stamp=observation_stamp+1,freshness=NULL")
            .execute(writer.as_mut().ok_or(LibraryError::WriterUnavailable)?)
            .await?;
        Ok(())
    }
}

impl Database {
    pub async fn catalog_missing_media(&self, media: &[String]) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT requested.value FROM json_each(?1) requested
            WHERE EXISTS(SELECT 1 FROM main.sources WHERE catalog_complete=1)
              AND NOT EXISTS(SELECT 1 FROM main.tracks WHERE media_uri=requested.value AND indexed=1)")
            .bind(serde_json::to_string(media)?).fetch_all(&mut *connection).await?)
    }

    pub async fn source_counts(
        &self,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(usize, usize)> {
        let mut connection = self.acquire_general(cancellation).await?;
        let(album,track)=sqlx::query_as::<_,(i64,i64)>("SELECT (SELECT count(*) FROM albums album WHERE (album.indexed=1 OR EXISTS(SELECT 1 FROM tracks admitted WHERE admitted.album_key=album.album_key AND admitted.indexed=1))),(SELECT count(*) FROM tracks WHERE indexed=1)").fetch_one(&mut *connection).await?;
        Ok((
            usize::try_from(album).unwrap_or_default(),
            usize::try_from(track).unwrap_or_default(),
        ))
    }
}
