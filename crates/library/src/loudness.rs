//! Owns persisted source ReplayGain facts and the keys that decide when they still describe the audio.

use sqlx::SqliteConnection;

use crate::{AlbumKey, Database, LibraryError, LibraryResult, NativeGain, ReadCancellation};

#[derive(sqlx::FromRow)]
struct GainScalar {
    entity_kind: String,
    replay_gain_db: Option<f64>,
    replay_gain_peak: Option<f64>,
}

impl Database {
    pub async fn audio_analysis_key(&self, uri: &str) -> LibraryResult<Option<[u8; 32]>> {
        let mut connection = self.acquire_reader().await?;
        let key: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT loudness_analysis_key FROM tracks WHERE media_uri=?1")
                .bind(uri)
                .fetch_optional(&mut *connection)
                .await?;
        key.map(|key| {
            key.try_into()
                .map_err(|_| LibraryError::InvalidStore("invalid audio analysis key".into()))
        })
        .transpose()
    }

    /// Source ReplayGain for a track and its album, when it still describes the stored audio.
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
            "WITH owner AS(SELECT track.*,album.source_loudness_analysis_key album_source_token,album.loudness_analysis_key album_token FROM tracks track LEFT JOIN albums album USING(album_key) WHERE track.media_uri=?1),
             native AS(SELECT 'track' entity_kind,CASE WHEN source_loudness_analysis_key=loudness_analysis_key THEN json_extract(metadata,'$.source_loudness') END gain FROM owner
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

pub(crate) async fn invalidate_album_audio(
    connection: &mut SqliteConnection,
    albums: &[AlbumKey],
    update_source: bool,
) -> LibraryResult<bool> {
    if albums.is_empty() {
        return Ok(false);
    }
    let input = serde_json::to_string(albums)?;
    if update_source {
        sqlx::query("UPDATE albums SET source_loudness_analysis_key=randomblob(32) WHERE album_key IN(SELECT value FROM json_each(?1))").bind(&input).execute(&mut *connection).await?;
    }
    Ok(sqlx::query("UPDATE albums SET loudness_analysis_key=CASE WHEN EXISTS(SELECT 1 FROM tracks WHERE tracks.album_key=albums.album_key AND indexed=1 AND source_loudness_analysis_key<>loudness_analysis_key) THEN randomblob(32) ELSE source_loudness_analysis_key END WHERE album_key IN(SELECT value FROM json_each(?1))").bind(input).execute(connection).await?.rows_affected()>0)
}

fn native_album_sql(owner: &str) -> String {
    format!(
        "CASE WHEN {owner}.album_source_token={owner}.album_token THEN (SELECT json_extract(member.metadata,'$.album_loudness') FROM tracks member WHERE member.album_key={owner}.album_key AND member.indexed=1 AND json_type(member.metadata,'$.album_loudness')='object' AND json_extract(member.metadata,'$.album_loudness.replay_gain_db') IS NOT NULL ORDER BY member.source_path,member.disc_number,member.track_number,member.object_id LIMIT 1) END"
    )
}
