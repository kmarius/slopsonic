//! Reads persisted source ReplayGain facts.

use crate::{Database, LibraryError, LibraryResult, NativeGain, ReadCancellation};

#[derive(sqlx::FromRow)]
struct GainScalar {
    entity_kind: String,
    replay_gain_db: Option<f64>,
    replay_gain_peak: Option<f64>,
}

impl Database {
    /// The audio key a download of this track records as its revision.
    pub async fn audio_revision(&self, uri: &str) -> LibraryResult<Option<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(
            sqlx::query_scalar("SELECT hex(audio_key) FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_optional(&mut *connection)
                .await?,
        )
    }

    /// Source ReplayGain for a track and its album.
    pub async fn playback_loudness(
        &self,
        media_uri: &str,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<(Option<NativeGain>, Option<NativeGain>)> {
        let mut connection = tokio::select! {
            connection = self.acquire_playable() => connection?,
            () = cancellation.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        let rows = sqlx::query_as::<_, GainScalar>(sqlx::AssertSqlSafe(format!(
            "WITH owner AS(SELECT * FROM tracks WHERE media_uri=?1),
             native AS(SELECT 'track' entity_kind,json_extract(metadata,'$.source_loudness') gain FROM owner
             UNION ALL SELECT 'album',{} FROM owner)
             SELECT entity_kind,json_extract(gain,'$.replay_gain_db') replay_gain_db,json_extract(gain,'$.replay_gain_peak') replay_gain_peak FROM native WHERE gain IS NOT NULL AND gain<>'null'",
            native_album_sql("owner"))))
            .bind(media_uri).fetch_all(&mut *connection).await?;
        let (mut track, mut album) = (None, None);
        for row in rows {
            let gain = NativeGain {
                replay_gain_db: row.replay_gain_db,
                replay_gain_peak: row.replay_gain_peak,
            };
            let selected = if row.entity_kind == "track" {
                &mut track
            } else {
                &mut album
            };
            if selected.is_none() {
                *selected = Some(gain);
            }
        }
        Ok((track, album))
    }
}

fn native_album_sql(owner: &str) -> String {
    format!(
        "(SELECT json_extract(member.metadata,'$.album_loudness') FROM tracks member WHERE member.album_key={owner}.album_key AND member.indexed=1 AND json_type(member.metadata,'$.album_loudness')='object' AND json_extract(member.metadata,'$.album_loudness.replay_gain_db') IS NOT NULL ORDER BY member.source_path,member.disc_number,member.track_number,member.object_id LIMIT 1)"
    )
}
