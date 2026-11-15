//! Owns the fixed SQLite writer, bounded read pool, and read cancellation.
//! It creates no runtime and contains no product query policy.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqlitePoolOptions};
use sqlx::{Connection, Sqlite};
use tokio::sync::{Mutex, Notify};

use crate::{LibraryError, LibraryResult, schema};

const PAGE_SIZE_BYTES: u32 = 4 * 1024;
const WRITER_STATEMENTS: usize = 64;
const READER_STATEMENTS: usize = 32;
const ROW_BUFFER: usize = 32;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
const READER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PROGRESS_INTERVAL: i32 = 1_000;
const MIN_READERS: u32 = 1;
const MAX_READERS: u32 = 2;

/// Request-local cooperative cancellation for one read.
#[derive(Debug, Default)]
struct CancellationInner {
    cancelled: AtomicBool,
    notify: Notify,
}

#[derive(Clone, Debug, Default)]
pub struct ReadCancellation(Arc<CancellationInner>);

impl ReadCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

type CatalogObserver =
    dyn Fn(crate::CatalogUpdate) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync;

struct DatabaseInner {
    path: PathBuf,
    writer: Arc<Mutex<Option<SqliteConnection>>>,
    readers: SqlitePool,
    export_readers: SqlitePool,
    export_read: tokio::sync::Mutex<()>,
    /// Playback's own reader, so a track change never waits behind general reads.
    playable_readers: SqlitePool,
    distinct_track_covers: AtomicBool,
    catalog_observer: std::sync::Mutex<Option<Arc<CatalogObserver>>>,
    cache_writes: tokio::sync::watch::Sender<()>,
}

/// A pooled reader with its read's progress handler. The handler stays on the connection after
/// release and turns inert when this drops, before the connection goes back to the pool.
pub(crate) struct Reader {
    live: Arc<AtomicBool>,
    connection: PoolConnection<Sqlite>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
    }
}

impl std::ops::Deref for Reader {
    type Target = SqliteConnection;

    fn deref(&self) -> &SqliteConnection {
        &self.connection
    }
}

impl std::ops::DerefMut for Reader {
    fn deref_mut(&mut self) -> &mut SqliteConnection {
        &mut self.connection
    }
}

/// Final ownership of one fixed writer and the bounded read-only pool.
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

impl Database {
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn subscribe_cache_writes(&self) -> tokio::sync::watch::Receiver<()> {
        self.inner.cache_writes.subscribe()
    }

    pub(crate) fn cache_written(&self) {
        self.inner.cache_writes.send_replace(());
    }

    pub fn observe_catalog<F, Work>(&self, observer: F)
    where
        F: Fn(crate::CatalogUpdate) -> Work + Send + Sync + 'static,
        Work: Future<Output = ()> + Send + 'static,
    {
        *self.inner.catalog_observer.lock().unwrap() =
            Some(Arc::new(move |update| Box::pin(observer(update))));
    }

    pub(crate) async fn catalog_updated(&self, update: crate::CatalogUpdate) {
        let changed = update.metadata || update.artwork || update.playlists;
        if changed && let Err(error) = self.refresh_incomplete_playlist_artwork(None).await {
            tracing::warn!(%error, "could not update incomplete playlist artwork");
        }
        let observer = self.inner.catalog_observer.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer(update).await;
        }
    }

    pub async fn set_distinct_track_covers(&self, enabled: bool) -> LibraryResult<()> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        let changed = sqlx::query(
            "UPDATE sources SET distinct_track_covers=?1 WHERE distinct_track_covers<>?1",
        )
        .bind(enabled)
        .execute(&mut *transaction)
        .await?
        .rows_affected()
            > 0;
        if changed {
            let mut after = 0;
            loop {
                let keys: Vec<i64> = sqlx::query_scalar("SELECT track_key FROM tracks WHERE object_id IS NOT NULL AND track_key>?1 ORDER BY track_key LIMIT 128")
                    .bind(after).fetch_all(&mut *transaction).await?;
                let Some(last) = keys.last() else { break };
                after = *last;
                if crate::catalog::artwork::refresh_track_choices(&mut transaction, &keys, &[], &[])
                    .await?
                {
                    crate::artwork::refresh_mosaics(&mut transaction, &keys, &[], &[]).await?;
                }
            }
        }
        transaction.commit().await?;
        self.inner
            .distinct_track_covers
            .store(enabled, Ordering::Release);
        drop(writer);
        if changed {
            let mut update = crate::CatalogUpdate::default();
            update.artwork = true;
            self.publish_catalog_update(update).await;
        }
        Ok(())
    }

    pub fn distinct_track_covers(&self) -> bool {
        self.inner.distinct_track_covers.load(Ordering::Acquire)
    }
    pub async fn open(path: impl AsRef<Path>) -> LibraryResult<Self> {
        Self::open_final(path.as_ref()).await
    }

    pub async fn close(self) -> LibraryResult<()> {
        tokio::join!(
            self.inner.export_readers.close(),
            self.inner.playable_readers.close(),
            self.inner.readers.close()
        );
        if let Some(writer) = self.inner.writer.lock().await.take() {
            writer.close().await?;
        }
        Ok(())
    }

    pub(crate) async fn open_final(path: &Path) -> LibraryResult<Self> {
        let mut writer = open_writer(path).await?;
        let initialized = async {
            schema::initialize_durable(&mut writer).await?;
            sqlx::raw_sql("PRAGMA optimize=0x10002")
                .execute(&mut writer)
                .await?;
            LibraryResult::Ok(())
        }
        .await;
        if let Err(error) = initialized {
            writer.close().await?;
            return Err(error);
        }
        let readers = open_readers(path, MIN_READERS, MAX_READERS, ROW_BUFFER).await?;
        let export_readers = open_readers(path, 0, 1, 1).await?;
        let playable_readers = open_readers(path, 0, 1, ROW_BUFFER).await?;
        Ok(Self {
            inner: Arc::new(DatabaseInner {
                path: path.to_owned(),
                writer: Arc::new(Mutex::new(Some(writer))),
                readers,
                export_readers,
                export_read: tokio::sync::Mutex::new(()),
                playable_readers,
                distinct_track_covers: AtomicBool::new(false),
                catalog_observer: std::sync::Mutex::new(None),
                cache_writes: tokio::sync::watch::channel(()).0,
            }),
        })
    }

    pub(crate) async fn has_server(&self) -> LibraryResult<bool> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sources)")
            .fetch_one(&mut *connection)
            .await?)
    }

    /// Forgets the server: its user state and undelivered edits go; referenced metadata stays hidden.
    pub async fn remove_server(&self) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql(
            "DELETE FROM user_media_state;
             DELETE FROM native_media_state;
             DELETE FROM favorite_outbox;
             DELETE FROM rating_outbox;
             UPDATE tracks SET indexed=0,artwork_kind=CASE WHEN native_artwork IS NULL THEN 0 ELSE 1 END,artwork_artist_key=NULL;
             UPDATE albums SET indexed=0;
             UPDATE artists SET indexed=0;
             DELETE FROM home_entries;
             DELETE FROM playlists;
             DELETE FROM download_files;
             UPDATE tracks SET object_id=NULL WHERE object_id IS NOT NULL;
             DELETE FROM albums;
             DELETE FROM artists;
             DELETE FROM genres;
             DELETE FROM moods;
             DELETE FROM folders;",
        )
        .execute(&mut *transaction)
        .await?;
        let removed = sqlx::query("DELETE FROM sources")
            .execute(&mut *transaction)
            .await?
            .rows_affected()
            > 0;
        transaction.commit().await?;
        Ok(removed)
    }

    pub async fn writer(
        &self,
    ) -> LibraryResult<tokio::sync::MutexGuard<'_, Option<SqliteConnection>>> {
        let writer = self.inner.writer.lock().await;
        if writer.is_none() {
            return Err(LibraryError::WriterUnavailable);
        }
        Ok(writer)
    }

    /// A pooled reader whose statements stop once `cancellation` fires.
    pub(crate) async fn acquire_general(
        &self,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Reader> {
        let mut connection = tokio::select! {
            connection = self.inner.readers.acquire() => connection?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let live = Arc::new(AtomicBool::new(true));
        let (read, cancellation) = (Arc::clone(&live), cancellation.clone());
        connection
            .lock_handle()
            .await?
            .set_progress_handler(PROGRESS_INTERVAL, move || {
                !(read.load(Ordering::Acquire) && cancellation.is_cancelled())
            });
        Ok(Reader { live, connection })
    }

    /// A pooled reader for a read nothing cancels. A handler a cancellable read left on the
    /// connection is inert, so this takes no handle lock.
    pub(crate) async fn acquire_reader(&self) -> LibraryResult<PoolConnection<Sqlite>> {
        Ok(self.inner.readers.acquire().await?)
    }

    pub(crate) async fn acquire_playable(&self) -> LibraryResult<PoolConnection<Sqlite>> {
        Ok(self.inner.playable_readers.acquire().await?)
    }

    pub(crate) async fn acquire_export(
        &self,
    ) -> LibraryResult<(tokio::sync::MutexGuard<'_, ()>, PoolConnection<Sqlite>)> {
        let permit = self.inner.export_read.lock().await;
        Ok((permit, self.inner.export_readers.acquire().await?))
    }

    #[cfg(test)]
    pub(crate) fn reader_pool(&self) -> &SqlitePool {
        &self.inner.readers
    }
}

#[allow(unsafe_code)]
pub(crate) async fn open_writer(path: &Path) -> LibraryResult<SqliteConnection> {
    let options = base_options(path)
        .create_if_missing(true)
        .statement_cache_capacity(WRITER_STATEMENTS)
        .thread_name(|id| format!("slopsonic-library-writer-{id}"));
    let mut connection = SqliteConnection::connect_with(&options).await?;
    {
        let mut handle = connection.lock_handle().await?;
        // Replanning each bound UPSERT recompiles all its update triggers for every source row.
        // SAFETY: SQLx owns the live handle and excludes its worker while this guard is held.
        let result = unsafe {
            libsqlite3_sys::sqlite3_db_config(
                handle.as_raw_handle().as_ptr(),
                libsqlite3_sys::SQLITE_DBCONFIG_ENABLE_QPSG,
                1,
                std::ptr::null_mut::<i32>(),
            )
        };
        debug_assert_eq!(result, libsqlite3_sys::SQLITE_OK);
    }
    // Own the handle before configuring it so failures can await its release before replacement.
    let result = sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "PRAGMA page_size={PAGE_SIZE_BYTES}; PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA temp_store=FILE; PRAGMA mmap_size=0;"
    )))
    .execute(&mut connection)
    .await;
    if let Err(error) = result {
        connection.close().await?;
        return Err(error.into());
    }
    Ok(connection)
}

fn reader_options(path: &Path) -> SqliteConnectOptions {
    base_options(path)
        .pragma("temp_store", "FILE")
        .pragma("mmap_size", "0")
        .with_regexp()
        .read_only(true)
        .create_if_missing(false)
        .statement_cache_capacity(READER_STATEMENTS)
        .thread_name(|id| format!("slopsonic-library-reader-{id}"))
}

fn base_options(path: &Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .shared_cache(false)
        .busy_timeout(BUSY_TIMEOUT)
        .row_buffer_size(ROW_BUFFER)
}

async fn open_readers(
    path: &Path,
    minimum: u32,
    maximum: u32,
    row_buffer: usize,
) -> LibraryResult<SqlitePool> {
    let options = SqlitePoolOptions::new()
        .min_connections(minimum)
        .max_connections(maximum)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(READER_IDLE_TIMEOUT)
        .max_lifetime(None)
        .test_before_acquire(true)
        .after_connect(move |connection, _| {
            Box::pin(async move {
                sqlx::query("PRAGMA query_only=ON")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        });
    if minimum == 0 {
        Ok(options.connect_lazy_with(reader_options(path).row_buffer_size(row_buffer)))
    } else {
        Ok(options
            .connect_with(reader_options(path).row_buffer_size(row_buffer))
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;
    use tokio::time::{Instant, sleep, timeout};

    async fn database() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().expect("temporary Store directory");
        let database = Database::open(directory.path().join("library.sqlite3"))
            .await
            .expect("open SQLx Library");
        (directory, database)
    }

    #[tokio::test]
    async fn close_waits_for_exports_and_closes_both_reader_pools() {
        use futures_util::FutureExt;

        let (_directory, database) = database().await;
        let export = database.acquire_export().await.unwrap();
        let mut closing = Box::pin(database.clone().close());
        assert!(closing.as_mut().now_or_never().is_none());
        assert!(database.inner.readers.is_closed());
        assert!(database.inner.export_readers.is_closed());
        drop(export);
        timeout(Duration::from_secs(1), closing)
            .await
            .unwrap()
            .unwrap();
        assert!(database.acquire_export().await.is_err());
    }

    #[tokio::test]
    async fn fixed_writer_and_steady_reader_have_explicit_limits() {
        let (_directory, database) = database().await;
        assert_eq!(database.reader_pool().size(), MIN_READERS);
        assert_eq!(
            database.reader_pool().options().get_min_connections(),
            MIN_READERS
        );
        assert_eq!(
            database.reader_pool().options().get_max_connections(),
            MAX_READERS
        );
        assert_eq!(
            database.reader_pool().options().get_acquire_timeout(),
            ACQUIRE_TIMEOUT
        );
        assert_eq!(
            database.reader_pool().options().get_idle_timeout(),
            Some(READER_IDLE_TIMEOUT)
        );

        let cancellation = ReadCancellation::new();
        let mut reader = database
            .acquire_general(&cancellation)
            .await
            .expect("acquire general reader");
        let row = sqlx::query(
            "SELECT
                 (SELECT journal_mode FROM pragma_journal_mode),
                 (SELECT query_only FROM pragma_query_only),
                 (SELECT temp_store FROM pragma_temp_store),
                 (SELECT page_size FROM pragma_page_size)",
        )
        .fetch_one(&mut *reader)
        .await
        .expect("read fixed reader options");
        assert_eq!(row.get::<String, _>(0), "wal");
        assert_eq!(row.get::<i64, _>(1), 1);
        assert_eq!(row.get::<i64, _>(2), 1);
        assert_eq!(row.get::<i64, _>(3), i64::from(PAGE_SIZE_BYTES));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA mmap_size")
                .fetch_one(&mut *reader)
                .await
                .expect("read reader mmap bound"),
            0
        );
        assert!(
            sqlx::query("CREATE TABLE reader_write(value INTEGER)")
                .execute(&mut *reader)
                .await
                .is_err(),
            "pooled reader accepted a write"
        );

        let mut writer = database.writer().await.expect("acquire fixed writer");
        let writer = writer.as_mut().expect("writer remains available");
        let writer_options = sqlx::query_as::<_, (String, i64)>(
            "SELECT
                 (SELECT journal_mode FROM pragma_journal_mode),
                 (SELECT temp_store FROM pragma_temp_store)",
        )
        .fetch_one(&mut *writer)
        .await
        .expect("read fixed writer options");
        assert_eq!(writer_options, ("wal".to_string(), 1));
        let synchronous = sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
            .fetch_one(&mut *writer)
            .await
            .expect("read writer synchronous mode");
        assert_eq!(synchronous, 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("PRAGMA mmap_size")
                .fetch_one(&mut *writer)
                .await
                .expect("read writer mmap bound"),
            0
        );
    }

    #[tokio::test]
    async fn cancelled_read_returns_a_usable_connection() {
        let (_directory, database) = database().await;
        let cancellation = ReadCancellation::new();
        let task_database = database.clone();
        let task_cancellation = cancellation.clone();
        let read = tokio::spawn(async move {
            let mut connection = task_database.acquire_general(&task_cancellation).await?;
            sqlx::query_scalar::<_, i64>(
                "WITH RECURSIVE count(value) AS (
                     VALUES(0) UNION ALL SELECT value + 1 FROM count
                 ) SELECT sum(value) FROM count",
            )
            .fetch_one(&mut *connection)
            .await
            .map_err(LibraryError::from)
        });
        sleep(Duration::from_millis(20)).await;
        cancellation.cancel();
        let error = timeout(Duration::from_secs(2), read)
            .await
            .expect("cancelled SQLite read completed")
            .expect("cancelled read task joined")
            .expect_err("progress handler interrupted the read");
        assert!(error.to_string().contains("interrupted"), "{error}");

        let mut connection = database
            .acquire_reader()
            .await
            .expect("reacquire the reader");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "WITH RECURSIVE count(value) AS (
                     VALUES(0) UNION ALL SELECT value + 1 FROM count WHERE value < 99999
                 ) SELECT count(*) FROM count",
            )
            .fetch_one(&mut *connection)
            .await
            .expect("reader remains usable"),
            100_000
        );
    }

    #[tokio::test]
    async fn dynamic_reader_skips_journal_mode_and_is_idle_reapable() {
        let (directory, database) = database().await;
        let cancellation = ReadCancellation::new();
        let general = database
            .acquire_general(&cancellation)
            .await
            .expect("hold steady reader");
        let mut writer = database.writer().await.expect("acquire writer");
        sqlx::raw_sql("BEGIN IMMEDIATE")
            .execute(writer.as_mut().expect("writer available"))
            .await
            .expect("hold writer transaction");
        let mut optional = database
            .acquire_reader()
            .await
            .expect("open optional reader without journal write");
        assert_eq!(
            sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
                .fetch_one(&mut *optional)
                .await
                .expect("optional reader observes WAL"),
            "wal"
        );
        sqlx::raw_sql("ROLLBACK")
            .execute(writer.as_mut().expect("writer available"))
            .await
            .expect("release writer transaction");
        drop(writer);
        drop(optional);
        drop(general);
        assert_eq!(
            database.reader_pool().options().get_idle_timeout(),
            Some(READER_IDLE_TIMEOUT)
        );

        let idle_timeout = Duration::from_millis(100);
        let pool = SqlitePoolOptions::new()
            .min_connections(MIN_READERS)
            .max_connections(MAX_READERS)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .idle_timeout(idle_timeout)
            .max_lifetime(None)
            .connect_with(reader_options(&directory.path().join("library.sqlite3")))
            .await
            .expect("open short-deadline reader pool");
        let first = pool.acquire().await.expect("acquire steady reader");
        let second = pool.acquire().await.expect("acquire optional reader");
        drop(first);
        drop(second);
        sleep(idle_timeout * 3).await;
        assert_eq!(pool.size(), MIN_READERS);
        pool.close().await;
    }

    #[tokio::test]
    async fn reader_replacement_preserves_idempotent_reads() {
        let (_directory, database) = database().await;
        {
            let mut writer = database.writer().await.expect("acquire writer");
            sqlx::query("INSERT INTO sources(object_id,display_name) VALUES ('source','Source')")
                .execute(writer.as_mut().expect("writer available"))
                .await
                .expect("insert idempotent read row");
        }
        let mut reader = database
            .acquire_reader()
            .await
            .expect("acquire reader to replace");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
                .fetch_one(&mut *reader)
                .await
                .expect("first idempotent read"),
            1
        );
        reader.close_on_drop();
        drop(reader);

        let deadline = Instant::now() + Duration::from_secs(2);
        while database.reader_pool().size() < MIN_READERS && Instant::now() < deadline {
            sleep(Duration::from_millis(20)).await;
        }
        let mut replacement = database
            .acquire_reader()
            .await
            .expect("acquire replacement reader");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sources")
                .fetch_one(&mut *replacement)
                .await
                .expect("replacement idempotent read"),
            1
        );
    }
}

/// The one server row's fixed identity.
pub(crate) const SERVER_OBJECT_ID: &str = "server";

/// Creates the server row, which holds the catalog's freshness, revision and counts.
pub(crate) async fn write_source_identity(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::query("INSERT INTO sources(object_id) VALUES(?1) ON CONFLICT(object_id) DO NOTHING")
        .bind(SERVER_OBJECT_ID)
        .execute(connection)
        .await?;
    Ok(())
}
