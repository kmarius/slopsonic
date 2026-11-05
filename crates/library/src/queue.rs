//! Queue source reads, metadata, and saved playback state. Playback owns edits.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqlx::{Connection, Row, SqliteConnection};

use crate::{Database, LibraryError, LibraryResult, ReadCancellation, SourceKey, TrackReference};

pub const QUEUE_CONTEXT_LIMIT: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct QueueContextTitle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Arc<str>>,
    pub title: Arc<str>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueProvenance {
    Context {
        context_id: Arc<str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_title: Option<Arc<QueueContextTitle>>,
        source_rank: usize,
    },
    Manual,
    Random,
    Radio,
    AutoDj,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum QueueRepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRow {
    #[serde(skip)]
    pub occurrence_id: Option<u64>,
    pub position: usize,
    pub track: crate::TrackView,
    pub provenance: QueueProvenance,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueEntry {
    pub track_key: crate::TrackKey,
    pub provenance: QueueProvenance,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueRestore {
    #[serde(skip)]
    pub ids: Arc<[u64]>,
    pub entries: Arc<[QueueEntry]>,
    pub order: Arc<[u32]>,
    #[serde(skip)]
    pub rows: Vec<Arc<QueueRow>>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct QueueOrderUpdate {
    pub rows: Vec<(usize, u64)>,
    pub total: usize,
}

impl QueueOrderUpdate {
    pub fn coalesce(&mut self, mut newer: Self) {
        if newer.total < self.total {
            self.rows.truncate(
                self.rows
                    .partition_point(|(position, _)| *position < newer.total),
            );
        }
        newer.rows.truncate(
            newer
                .rows
                .partition_point(|(position, _)| *position < newer.total),
        );
        if self.rows.is_empty() {
            *self = newer;
            return;
        }
        self.total = newer.total;
        if newer.rows.is_empty() || self.rows.last().unwrap().0 < newer.rows[0].0 {
            self.rows.extend(newer.rows);
            return;
        }
        let mut rows = Vec::with_capacity(self.rows.len() + newer.rows.len());
        let mut older = std::mem::take(&mut self.rows).into_iter().peekable();
        for row in newer.rows {
            while older.peek().is_some_and(|old| old.0 < row.0) {
                rows.push(older.next().unwrap());
            }
            if older.peek().is_some_and(|old| old.0 == row.0) {
                older.next();
            }
            rows.push(row);
        }
        rows.extend(older);
        self.rows = rows;
    }
}

#[derive(Clone, Debug, Default)]
pub struct QueueUpdate {
    pub replace: bool,
    pub entries: HashMap<u64, QueueEntry>,
    pub removed: HashSet<u64>,
    pub ordinary: Option<QueueOrderUpdate>,
    pub order: Option<QueueOrderUpdate>,
    pub current_index: Option<usize>,
    pub progress_millis: i64,
    pub repeat_mode: QueueRepeatMode,
    pub shuffled: bool,
}

impl QueueUpdate {
    pub fn coalesce(&mut self, mut newer: Self) {
        if newer.replace {
            *self = newer;
            return;
        }
        for id in newer.removed.drain() {
            self.entries.remove(&id);
            self.removed.insert(id);
        }
        for (id, entry) in newer.entries.drain() {
            self.removed.remove(&id);
            self.entries.insert(id, entry);
        }
        for (old, next) in [
            (&mut self.ordinary, newer.ordinary),
            (&mut self.order, newer.order),
        ] {
            if let Some(next) = next {
                if let Some(old) = old {
                    old.coalesce(next);
                } else {
                    *old = Some(next);
                }
            }
        }
        self.current_index = newer.current_index;
        self.progress_millis = newer.progress_millis;
        self.repeat_mode = newer.repeat_mode;
        self.shuffled = newer.shuffled;
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum SavedContext<S> {
    Titled {
        id: S,
        title: Arc<QueueContextTitle>,
    },
    Untitled(S),
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueueLoad {
    Tracks(Vec<(crate::MemberSelection, QueueProvenance)>),
    Items(Vec<(TrackReference, QueueProvenance)>),
    Uris(Vec<String>, QueueProvenance),
    Captured(Arc<[QueueEntry]>),
}

impl QueueLoad {
    pub fn tracks(selection: crate::MemberSelection) -> Self {
        Self::Tracks(vec![(selection, QueueProvenance::Manual)])
    }
    pub fn track_keys(keys: Vec<crate::TrackKey>, provenance: QueueProvenance) -> Self {
        Self::Captured(
            keys.into_iter()
                .map(|track_key| QueueEntry {
                    track_key,
                    provenance: provenance.clone(),
                })
                .collect(),
        )
    }
    pub fn items(items: Vec<(TrackReference, QueueProvenance)>) -> Self {
        Self::Items(items)
    }
    pub fn media_uris(uris: impl IntoIterator<Item = String>, provenance: QueueProvenance) -> Self {
        Self::Uris(uris.into_iter().collect(), provenance)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueEntries {
    pub entries: Vec<QueueEntry>,
    pub anchor_index: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueuePlacement {
    Replace { anchor_index: usize },
    AfterCurrent,
    End,
}
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum QueueReorderTarget {
    Before(usize),
    After(usize),
    BeforeOccurrence(u64),
    AfterOccurrence(u64),
    End,
}
impl QueueRepeatMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "none",
            Self::One => "one",
            Self::All => "all",
        }
    }

    fn parse(value: &str) -> LibraryResult<Self> {
        match value {
            "none" => Ok(Self::Off),
            "one" => Ok(Self::One),
            "all" => Ok(Self::All),
            _ => Err(LibraryError::InvalidStore(
                "invalid queue repeat mode".to_string(),
            )),
        }
    }
}

impl Database {
    pub async fn track_references_for_uris(
        &self,
        media_uris: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<TrackReference>> {
        if media_uris.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(format!(
                "Queue materialization is limited to {QUEUE_CONTEXT_LIMIT} media URIs"
            )));
        }
        let mut connection = tokio::select! {result=self.acquire_reader()=>result?,()=cancellation.cancelled()=>return Err(LibraryError::ReadCancelled)};
        track_references_for_uris_on(&mut connection, media_uris).await
    }
}

impl Database {
    pub async fn queue_search_page(
        &self,
        queue: &QueueRestore,
        filter: &str,
        offset: u64,
        limit: u32,
    ) -> LibraryResult<(Vec<usize>, u64)> {
        let (_permit, mut reader) = self.acquire_general(&ReadCancellation::new()).await?;
        let mut transaction = reader.begin().await?;
        let filter = filter.trim().to_lowercase();
        let mut positions = Vec::new();
        let mut total = 0;
        for (page, order) in queue.order.chunks(4096).enumerate() {
            let keys = order
                .iter()
                .map(|i| queue.entries[*i as usize].track_key)
                .collect::<Vec<_>>();
            let matches=sqlx::query_scalar::<_,i64>("SELECT CAST(requested.key AS INTEGER) FROM json_each(?1) requested JOIN tracks ON track_key=requested.value WHERE instr(normalized_search,?2)>0 ORDER BY CAST(requested.key AS INTEGER)")
                .bind(serde_json::to_string(&keys)?).bind(&filter).fetch_all(&mut *transaction).await?;
            for position in matches {
                if total >= offset && positions.len() < limit as usize {
                    positions.push(page * 4096 + position as usize);
                }
                total += 1;
            }
        }
        Ok((positions, total))
    }

    pub async fn remember_media_uris(
        &self,
        uris: &[String],
    ) -> LibraryResult<Vec<crate::TrackKey>> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        let mut keys = Vec::with_capacity(uris.len());
        for uri in uris {
            keys.push(
                crate::catalog::remember_track(
                    &mut transaction,
                    &TrackReference {
                        media_uri: uri.clone(),
                        ..Default::default()
                    },
                )
                .await?,
            );
        }
        transaction.commit().await?;
        Ok(keys)
    }

    pub async fn queue_entries(
        &self,
        input: QueueLoad,
        selected: Option<(crate::TrackKey, Option<crate::PlaylistEntryKey>)>,
    ) -> LibraryResult<QueueEntries> {
        let mut entries = Vec::new();
        let mut anchor = None;
        match input {
            QueueLoad::Tracks(groups) => {
                let mut reader = self.acquire_reader().await?;
                let mut transaction = reader.begin().await?;
                for (selection, provenance) in groups {
                    let resolve_anchor = !matches!(selection, crate::MemberSelection::Concat(_));
                    let songs =
                        crate::selection::selected_tracks_on(&mut transaction, &selection).await?;
                    for (rank, (track_key, entry_key)) in songs.into_iter().enumerate() {
                        if resolve_anchor
                            && selected.is_some_and(|(key, entry)| {
                                entry.map_or(key == track_key, |entry| Some(entry) == entry_key)
                            })
                        {
                            anchor.get_or_insert(entries.len());
                        }
                        let mut provenance = provenance.clone();
                        if let QueueProvenance::Context { source_rank, .. } = &mut provenance {
                            *source_rank += rank;
                        }
                        entries.push(QueueEntry {
                            track_key,
                            provenance,
                        });
                    }
                }
                transaction.commit().await?;
            }
            QueueLoad::Items(_) | QueueLoad::Uris(_, _) => {
                let mut writer = self.writer().await?;
                let mut transaction = writer
                    .as_mut()
                    .ok_or(LibraryError::WriterUnavailable)?
                    .begin()
                    .await?;
                match input {
                    QueueLoad::Items(items) => {
                        for (item, provenance) in items {
                            let track_key = persist_metadata(&mut transaction, &item).await?;
                            entries.push(QueueEntry {
                                track_key,
                                provenance,
                            });
                        }
                    }
                    QueueLoad::Uris(uris, provenance) => {
                        for (rank, uri) in uris.iter().enumerate() {
                            let track_key = crate::catalog::remember_track(
                                &mut transaction,
                                &TrackReference {
                                    media_uri: uri.clone(),
                                    ..Default::default()
                                },
                            )
                            .await?;
                            let mut provenance = provenance.clone();
                            if let QueueProvenance::Context { source_rank, .. } = &mut provenance {
                                *source_rank += rank;
                            }
                            entries.push(QueueEntry {
                                track_key,
                                provenance,
                            });
                        }
                    }
                    _ => unreachable!(),
                }
                transaction.commit().await?;
            }
            QueueLoad::Captured(captured) => entries.extend(captured.iter().cloned()),
        }
        Ok(QueueEntries {
            entries,
            anchor_index: anchor,
        })
    }

    pub async fn queue_rows_for_entries(
        &self,
        entries: &[(usize, QueueEntry)],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Arc<QueueRow>>> {
        if entries.len() > QUEUE_CONTEXT_LIMIT {
            return Err(LibraryError::InvalidRequest(
                "Queue window exceeds its context".into(),
            ));
        }
        let mut connection = tokio::select! {result=self.acquire_reader()=>result?,()=cancellation.cancelled()=>return Err(LibraryError::ReadCancelled)};
        let keys = entries
            .iter()
            .map(|(_, entry)| entry.track_key)
            .collect::<Vec<_>>();
        let songs = crate::tracks::load_track_rows(&mut connection, &keys).await?;
        Ok(entries
            .iter()
            .zip(songs)
            .map(|((position, entry), song)| {
                Arc::new(QueueRow {
                    occurrence_id: None,
                    position: *position,
                    track: song,
                    provenance: entry.provenance.clone(),
                })
            })
            .collect())
    }

    pub async fn save_queue(&self, state: &QueueRestore) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        save_queue_on(&mut transaction, state).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn apply_queue_update(&self, update: &QueueUpdate) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let mut transaction = writer
            .as_mut()
            .ok_or(LibraryError::WriterUnavailable)?
            .begin()
            .await?;
        apply_queue_update_on(&mut transaction, update).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn restore_queue(&self) -> LibraryResult<QueueRestore> {
        let mut reader = self.acquire_reader().await?;
        let mut transaction = reader.begin().await?;
        let mut state = restore_queue_on(&mut transaction).await?;
        transaction.commit().await?;
        drop(reader);
        let start = state.current_index.unwrap_or(0).saturating_sub(10);
        let entries = state
            .order
            .iter()
            .enumerate()
            .skip(start)
            .take(QUEUE_CONTEXT_LIMIT)
            .map(|(position, index)| (position, state.entries[*index as usize].clone()))
            .collect::<Vec<_>>();
        state.rows = self
            .queue_rows_for_entries(&entries, &ReadCancellation::new())
            .await?;
        for row in &mut state.rows {
            Arc::make_mut(row).occurrence_id = Some(state.ids[state.order[row.position] as usize]);
        }
        Ok(state)
    }

    pub async fn queue_positions_for_source(
        &self,
        queue: &QueueRestore,
        source: SourceKey,
    ) -> LibraryResult<Vec<usize>> {
        let mut reader = self.acquire_reader().await?;
        let mut positions = Vec::new();
        for (page, order) in queue.order.chunks(4096).enumerate() {
            let keys = order
                .iter()
                .map(|i| queue.entries[*i as usize].track_key)
                .collect::<Vec<_>>();
            let matching=sqlx::query_scalar::<_,i64>("SELECT CAST(requested.key AS INTEGER) FROM json_each(?1) requested JOIN tracks ON track_key=requested.value WHERE source_key=?2 ORDER BY CAST(requested.key AS INTEGER)")
                .bind(serde_json::to_string(&keys)?).bind(source).fetch_all(&mut *reader).await?;
            positions.extend(
                matching
                    .into_iter()
                    .map(|position| page * 4096 + position as usize),
            );
        }
        Ok(positions)
    }
}

async fn save_settings(
    connection: &mut sqlx::SqliteConnection,
    current: Option<usize>,
    progress: i64,
    repeat: QueueRepeatMode,
    shuffled: bool,
) -> LibraryResult<()> {
    sqlx::query("INSERT INTO queue_state(singleton,current_index,progress_millis,repeat_mode,shuffled) VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET current_index=excluded.current_index,progress_millis=excluded.progress_millis,repeat_mode=excluded.repeat_mode,shuffled=excluded.shuffled")
        .bind(current.map(|index| index as i64)).bind(progress.max(0)).bind(repeat.as_str()).bind(shuffled).execute(connection).await?;
    Ok(())
}
pub(crate) async fn save_queue_on(
    connection: &mut sqlx::SqliteConnection,
    state: &QueueRestore,
) -> LibraryResult<()> {
    let ids = if state.ids.is_empty() {
        (1..=state.entries.len() as u64).collect::<Vec<_>>()
    } else if state.ids.len() == state.entries.len() {
        state.ids.to_vec()
    } else {
        return Err(LibraryError::InvalidRequest(
            "Queue IDs do not match its entries".into(),
        ));
    };
    let order = state
        .order
        .iter()
        .enumerate()
        .map(|(position, index)| {
            ids.get(*index as usize)
                .copied()
                .map(|id| (position, id))
                .ok_or_else(|| LibraryError::InvalidRequest("Invalid queue play order".into()))
        })
        .collect::<LibraryResult<Vec<_>>>()?;
    apply_queue_update_on(
        connection,
        &QueueUpdate {
            replace: true,
            entries: ids
                .iter()
                .copied()
                .zip(state.entries.iter().cloned())
                .collect(),
            ordinary: Some(QueueOrderUpdate {
                rows: ids.iter().copied().enumerate().collect(),
                total: ids.len(),
            }),
            order: Some(QueueOrderUpdate {
                rows: order,
                total: state.order.len(),
            }),
            current_index: state.current_index,
            progress_millis: state.progress_millis,
            repeat_mode: state.repeat_mode,
            shuffled: state.shuffled,
            ..Default::default()
        },
    )
    .await
}

async fn apply_queue_update_on(
    connection: &mut sqlx::SqliteConnection,
    update: &QueueUpdate,
) -> LibraryResult<()> {
    if update.replace {
        sqlx::raw_sql(
            "DELETE FROM queue_positions; DELETE FROM queue_entries; DELETE FROM queue_contexts;",
        )
        .execute(&mut *connection)
        .await?;
    } else {
        for removed in update.removed.iter().collect::<Vec<_>>().chunks(1000) {
            let contexts = sqlx::query_scalar::<_, Option<i64>>(
                "DELETE FROM queue_entries WHERE entry_id IN (SELECT value FROM json_each(?1)) RETURNING context_key",
            ).bind(serde_json::to_string(removed)?).fetch_all(&mut *connection).await?;
            sqlx::query("DELETE FROM queue_contexts WHERE context_key IN (SELECT value FROM json_each(?1)) AND NOT EXISTS(SELECT 1 FROM queue_entries WHERE context_key=queue_contexts.context_key)")
                .bind(serde_json::to_string(&contexts)?).execute(&mut *connection).await?;
        }
    }
    let mut contexts = HashMap::new();
    let mut entries = update.entries.iter().collect::<Vec<_>>();
    entries.sort_unstable_by_key(|(id, _)| *id);
    for entries in entries.chunks(1000) {
        let mut rows = Vec::with_capacity(entries.len());
        for (id, entry) in entries {
            let (context_key, source_rank, provenance) = match &entry.provenance {
                QueueProvenance::Context {
                    context_id,
                    context_title,
                    source_rank,
                } => {
                    let key = (context_id.as_ref(), context_title.as_deref());
                    let context_key = if let Some(key) = contexts.get(&key) {
                        *key
                    } else {
                        let value = serde_json::to_string(&match context_title {
                            Some(title) => SavedContext::Titled {
                                id: context_id.as_ref(),
                                title: title.clone(),
                            },
                            None => SavedContext::Untitled(context_id.as_ref()),
                        })?;
                        sqlx::query("INSERT INTO queue_contexts(context) VALUES(?1) ON CONFLICT(context) DO NOTHING")
                            .bind(&value).execute(&mut *connection).await?;
                        let stored: i64 = sqlx::query_scalar(
                            "SELECT context_key FROM queue_contexts WHERE context=?1",
                        )
                        .bind(value)
                        .fetch_one(&mut *connection)
                        .await?;
                        contexts.insert(key, stored);
                        stored
                    };
                    (Some(context_key), Some(*source_rank as i64), None)
                }
                provenance => (None, None, Some(serde_json::to_string(provenance)?)),
            };
            rows.push((
                **id as i64,
                entry.track_key.raw(),
                provenance,
                context_key,
                source_rank,
            ));
        }
        sqlx::query("INSERT INTO queue_entries(entry_id,track_key,provenance,context_key,source_rank)
            SELECT json_extract(value,'$[0]'),json_extract(value,'$[1]'),json_extract(value,'$[2]'),json_extract(value,'$[3]'),json_extract(value,'$[4]') FROM json_each(?1) WHERE true ON CONFLICT(entry_id) DO UPDATE SET track_key=excluded.track_key")
            .bind(serde_json::to_string(&rows)?).execute(&mut *connection).await?;
    }
    for (kind, order) in [(0_i64, &update.ordinary), (1, &update.order)] {
        if let Some(order) = order {
            sqlx::query("DELETE FROM queue_positions WHERE order_kind=?1 AND position>=?2")
                .bind(kind)
                .bind(order.total as i64)
                .execute(&mut *connection)
                .await?;
            for rows in order.rows.chunks(1000) {
                sqlx::query("INSERT INTO queue_positions(order_kind,position,entry_id)
                    SELECT ?1,json_extract(value,'$[0]'),json_extract(value,'$[1]') FROM json_each(?2) WHERE true
                    ON CONFLICT(order_kind,position) DO UPDATE SET entry_id=excluded.entry_id")
                    .bind(kind).bind(serde_json::to_string(rows)?).execute(&mut *connection).await?;
            }
        }
    }
    save_settings(
        connection,
        update.current_index,
        update.progress_millis,
        update.repeat_mode,
        update.shuffled,
    )
    .await
}

async fn restore_queue_on(connection: &mut sqlx::SqliteConnection) -> LibraryResult<QueueRestore> {
    let contexts =
        sqlx::query_as::<_, (i64, String)>("SELECT context_key,context FROM queue_contexts")
            .fetch_all(&mut *connection)
            .await?
            .into_iter()
            .map(|(key, json)| Ok((key, serde_json::from_str::<SavedContext<Arc<str>>>(&json)?)))
            .collect::<LibraryResult<HashMap<_, _>>>()?;
    let rows = sqlx::query("SELECT entry.* FROM queue_positions position JOIN queue_entries entry USING(entry_id) WHERE order_kind=0 ORDER BY position")
        .fetch_all(&mut *connection).await?;
    let mut entries = Vec::with_capacity(rows.len());
    let mut ids = Vec::with_capacity(rows.len());
    for row in rows {
        ids.push(row.try_get::<i64, _>("entry_id")? as u64);
        let provenance = match row.try_get::<Option<i64>, _>("context_key")? {
            Some(key) => {
                let (context_id, context_title) = match &contexts[&key] {
                    SavedContext::Titled { id, title } => (id.clone(), Some(title.clone())),
                    SavedContext::Untitled(id) => (id.clone(), None),
                };
                QueueProvenance::Context {
                    context_id,
                    context_title,
                    source_rank: row.try_get::<i64, _>("source_rank")? as usize,
                }
            }
            None => serde_json::from_str(&row.try_get::<String, _>("provenance")?)?,
        };
        entries.push(QueueEntry {
            track_key: row.try_get("track_key")?,
            provenance,
        });
    }
    let canonical = ids
        .iter()
        .enumerate()
        .map(|(position, id)| (*id, position as u32))
        .collect::<HashMap<_, _>>();
    let order = sqlx::query_scalar::<_, i64>(
        "SELECT entry_id FROM queue_positions WHERE order_kind=1 ORDER BY position",
    )
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|id| canonical[&(id as u64)])
    .collect();
    let mut state = QueueRestore {
        ids: ids.into(),
        entries: entries.into(),
        order,
        ..Default::default()
    };
    read_settings(connection, &mut state).await?;
    Ok(state)
}

async fn read_settings(
    connection: &mut sqlx::SqliteConnection,
    state: &mut QueueRestore,
) -> LibraryResult<()> {
    if let Some((current, progress, repeat, shuffled)) = sqlx::query_as::<_, (Option<i64>, i64, String, bool)>("SELECT current_index,progress_millis,repeat_mode,shuffled FROM queue_state WHERE singleton=1").fetch_optional(connection).await? {
        state.current_index = current.map(|index| index as usize);
        state.progress_millis = progress;
        state.repeat_mode = QueueRepeatMode::parse(&repeat)?;
        state.shuffled = shuffled;
    }
    Ok(())
}

#[allow(non_upper_case_globals)]
impl QueuePlacement {
    pub const Now: Self = Self::Replace { anchor_index: 0 };
    pub const Next: Self = Self::AfterCurrent;
    pub const Last: Self = Self::End;
    pub const fn with_anchor(self, anchor_index: usize) -> Self {
        match self {
            Self::Replace { .. } => Self::Replace { anchor_index },
            other => other,
        }
    }
}

async fn persist_metadata(
    connection: &mut SqliteConnection,
    item: &TrackReference,
) -> LibraryResult<crate::TrackKey> {
    crate::catalog::remember_track(connection, item).await
}

/// Fisher-Yates shuffle with SplitMix64, shared by queue capture and playback order.
pub fn shuffle_order<T>(values: &mut [T], mut seed: u64) {
    for i in (1..values.len()).rev() {
        seed = seed.wrapping_add(0x9e3779b97f4a7c15);
        let mut n = seed;
        n = (n ^ (n >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        n = (n ^ (n >> 27)).wrapping_mul(0x94d049bb133111eb);
        n ^= n >> 31;
        values.swap(i, ((u128::from(n) * (i as u128 + 1)) >> 64) as usize);
    }
}

async fn track_references_for_uris_on(
    connection: &mut SqliteConnection,
    uris: &[String],
) -> LibraryResult<Vec<TrackReference>> {
    let keys = sqlx::query_scalar::<_, crate::TrackKey>("SELECT track_key FROM json_each(?1) requested JOIN tracks ON media_uri=requested.value ORDER BY CAST(requested.key AS INTEGER)")
        .bind(serde_json::to_string(uris)?).fetch_all(&mut *connection).await?;
    let songs = crate::tracks::load_track_rows(connection, &keys).await?;
    let songs = songs
        .into_iter()
        .map(|song| (song.media_uri.clone(), TrackReference::from(song)))
        .collect::<HashMap<_, _>>();
    Ok(uris
        .iter()
        .map(|uri| {
            songs
                .get(uri)
                .cloned()
                .unwrap_or_else(|| TrackReference::direct(uri.clone(), uri.clone(), "", "", 0))
        })
        .collect())
}

impl QueueRow {
    pub fn same_row(&self, other: Arc<QueueRow>) -> bool {
        std::ptr::eq(self, other.as_ref()) || self == other.as_ref()
    }

    pub fn track(&self) -> crate::TrackView {
        self.track.clone()
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn context_id(&self) -> Option<String> {
        match &self.provenance {
            QueueProvenance::Context { context_id, .. } => Some(context_id.to_string()),
            _ => None,
        }
    }
    pub fn source_rank(&self) -> Option<usize> {
        match &self.provenance {
            QueueProvenance::Context { source_rank, .. } => Some(*source_rank),
            _ => None,
        }
    }
    pub fn context_title(&self) -> Option<String> {
        match &self.provenance {
            QueueProvenance::Context { context_title, .. } => context_title
                .as_ref()
                .map(|caption| caption.title.to_string()),
            _ => None,
        }
    }
    pub fn context_category(&self) -> Option<String> {
        match &self.provenance {
            QueueProvenance::Context { context_title, .. } => context_title
                .as_ref()
                .and_then(|caption| caption.kind.as_deref().map(str::to_owned)),
            _ => None,
        }
    }
    pub fn context_kind(&self) -> String {
        match self.provenance {
            QueueProvenance::Context { .. } => "context",
            QueueProvenance::Manual => "manual",
            QueueProvenance::Random => "random",
            QueueProvenance::Radio => "radio",
            QueueProvenance::AutoDj => "auto_dj",
        }
        .into()
    }
}
