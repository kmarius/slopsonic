//! Owns the outbox of scrobbles waiting for delivery to external services.

use sqlx::{Connection, FromRow};

use crate::{Database, LibraryError, LibraryResult, ListenOutboxKey, ReadCancellation};

const DELIVERY_LIMIT: usize = 100;

/// A qualified play, as handed to the external scrobbling services.
#[derive(Clone, Debug, PartialEq)]
pub struct ListenWrite {
    pub play_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_millis: i64,
    pub started_at: i64,
    pub listened_millis: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenDeliveryTarget {
    pub service: String,
    pub account_id: String,
    pub next_attempt_at: Option<i64>,
}

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct PendingListenDelivery {
    pub outbox_key: ListenOutboxKey,
    pub service: String,
    pub account_id: String,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub track_title: String,
    pub artist_name: String,
    pub album_title: String,
    pub started_at: i64,
    pub duration_millis: i64,
}

impl Database {
    /// Queues one delivery per target; queuing the same play again is a no-op.
    pub async fn queue_scrobbles(
        &self,
        listen: &ListenWrite,
        deliveries: &[ListenDeliveryTarget],
    ) -> LibraryResult<()> {
        validate_listen(listen, deliveries)?;
        if deliveries.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        let mut transaction = connection.begin().await?;
        for delivery in deliveries {
            sqlx::query(
                "INSERT OR IGNORE INTO scrobble_outbox(play_id,service,account_id,track_title,artist_name,album_title,started_at,duration_millis,next_attempt_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            )
            .bind(&listen.play_id)
            .bind(&delivery.service)
            .bind(&delivery.account_id)
            .bind(&listen.title)
            .bind(&listen.artist)
            .bind(&listen.album)
            .bind(listen.started_at)
            .bind(listen.duration_millis)
            .bind(delivery.next_attempt_at)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn due_listen_deliveries(
        &self,
        now: i64,
        limit: usize,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<PendingListenDelivery>> {
        let limit = limit.clamp(1, DELIVERY_LIMIT) as i64;
        let mut connection = self.acquire_general(cancellation).await?;
        Ok(sqlx::query_as::<_, PendingListenDelivery>(
            "SELECT outbox_key,service,account_id,attempts,next_attempt_at,last_error,
                    track_title,artist_name,album_title,started_at,duration_millis
             FROM scrobble_outbox
             WHERE next_attempt_at IS NOT NULL AND next_attempt_at<=?1
             ORDER BY next_attempt_at,outbox_key LIMIT ?2",
        )
        .bind(now)
        .bind(limit)
        .fetch_all(&mut *connection)
        .await?)
    }

    pub async fn complete_listen_delivery(&self, outbox: ListenOutboxKey) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("DELETE FROM scrobble_outbox WHERE outbox_key=?1")
                .bind(outbox)
                .execute(connection)
                .await?
                .rows_affected()
                == 1,
        )
    }

    pub async fn defer_listen_delivery(
        &self,
        outbox: ListenOutboxKey,
        next_attempt_at: i64,
        last_error: Option<&str>,
    ) -> LibraryResult<bool> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("UPDATE scrobble_outbox SET attempts=attempts+1,next_attempt_at=?2,last_error=?3 WHERE outbox_key=?1")
            .bind(outbox).bind(next_attempt_at).bind(last_error).execute(connection).await?.rows_affected()==1)
    }

    pub async fn block_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
        last_error: &str,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("UPDATE scrobble_outbox SET attempts=attempts+1,next_attempt_at=NULL,last_error=?3 WHERE service=?1 AND account_id=?2")
            .bind(service).bind(account_id).bind(last_error).execute(connection).await?.rows_affected())
    }

    pub async fn remove_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(
            sqlx::query("DELETE FROM scrobble_outbox WHERE service=?1 AND account_id=?2")
                .bind(service)
                .bind(account_id)
                .execute(connection)
                .await?
                .rows_affected(),
        )
    }

    pub async fn wake_listen_deliveries(
        &self,
        service: &str,
        account_id: &str,
        now: i64,
    ) -> LibraryResult<u64> {
        let mut writer = self.writer().await?;
        let connection = writer.as_mut().ok_or(LibraryError::WriterUnavailable)?;
        Ok(sqlx::query("UPDATE scrobble_outbox SET next_attempt_at=?3,last_error=NULL WHERE service=?1 AND account_id=?2 AND next_attempt_at IS NULL")
            .bind(service).bind(account_id).bind(now).execute(connection).await?.rows_affected())
    }
}

fn validate_listen(listen: &ListenWrite, deliveries: &[ListenDeliveryTarget]) -> LibraryResult<()> {
    if listen.play_id.is_empty() {
        return Err(LibraryError::InvalidRequest(
            "listen identities cannot be empty".to_string(),
        ));
    }
    if listen.started_at < 0 || listen.duration_millis < 0 || listen.listened_millis < 0 {
        return Err(LibraryError::InvalidRequest(
            "listen time values cannot be negative".to_string(),
        ));
    }
    if deliveries
        .iter()
        .any(|target| target.service.is_empty() || target.account_id.is_empty())
    {
        return Err(LibraryError::InvalidRequest(
            "listen delivery identity cannot be empty".to_string(),
        ));
    }
    Ok(())
}
