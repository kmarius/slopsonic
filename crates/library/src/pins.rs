//! Indexed sidebar facts.
use crate::{Database, LibraryResult, ReadCancellation, SourceId};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, FromRow};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum SidebarPin {
    Album {
        source_id: SourceId,
        album_id: String,
    },
    Artist {
        source_id: SourceId,
        artist_id: String,
        #[serde(default, skip_serializing_if = "is_false")]
        album_artist: bool,
    },
    Genre {
        source_id: SourceId,
        genre_id: String,
    },
    Playlist {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<SourceId>,
        playlist_id: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarPinRow {
    pub pin: SidebarPin,
    pub key: i64,
    pub name: String,
    pub totals: Option<(i64, i64)>,
    pub artwork: Vec<Vec<u8>>,
}

const REQUESTED: &str = "WITH requested AS (
    SELECT CAST(pin.key AS INTEGER) ordinal,kind.key kind,
    json_extract(kind.value,'$.source_id') source_id,
    coalesce(json_extract(kind.value,'$.album_id'),json_extract(kind.value,'$.artist_id'),
        json_extract(kind.value,'$.genre_id'),json_extract(kind.value,'$.playlist_id')) object_id,
    coalesce(json_extract(kind.value,'$.album_artist'),0) role
    FROM json_each(?1) pin CROSS JOIN json_each(pin.value) kind)";

impl Database {
    pub async fn sidebar_pin_rows(
        &self,
        pins: &[SidebarPin],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<SidebarPinRow>> {
        if pins.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(FromRow)]
        struct Row {
            ordinal: i64,
            entity_key: i64,
            name: String,
            track_count: Option<i64>,
            duration_millis: Option<i64>,
            artwork_binding: Option<Vec<u8>>,
            gallery: String,
        }
        let mut parts = Vec::new();
        for (kind, table, key, name, totals_kind) in [
            ("Album", "albums", "album_key", "title", "album"),
            ("Artist", "artists", "artist_key", "name", "artist"),
            ("Genre", "genres", "genre_key", "name", "genre"),
            ("Playlist", "playlists", "playlist_key", "name", "playlist"),
        ] {
            let (scope, totals, count, duration): (String, String, String, String) = match kind {
                "Album" => ("AND item.source_key IS source.source_key AND source.source_key IS NOT NULL".into(), String::new(), "item.track_count".into(), "item.duration_millis".into()),
                _ => (
                    "AND item.source_key IS source.source_key AND (requested.source_id IS NULL OR source.source_key IS NOT NULL)".into(),
                    format!("LEFT JOIN collection_totals totals ON totals.kind='{totals_kind}' AND totals.entity_key=item.{key} AND totals.role=requested.role AND coalesce(totals.folder_key,'')=''"),
                    "coalesce(totals.track_count,0)".into(), "coalesce(totals.duration_millis,0)".into(),
                ),
            };
            let artwork = crate::artwork::preview_sql(if kind == "Genre" {
                "item.native_artwork"
            } else {
                "item.artwork_binding"
            });
            let gallery = if kind == "Genre" {
                format!(
                    "CASE WHEN item.native_artwork IS NULL THEN {} ELSE '[]' END",
                    crate::collections::gallery_sql("genre", None)
                        .replace("genre.genre_key", "item.genre_key")
                )
            } else {
                "'[]'".into()
            };
            parts.push(format!("SELECT requested.ordinal,item.{key} entity_key,item.{name} name,{count} track_count,{duration} duration_millis,{artwork} artwork_binding,{gallery} gallery
                FROM requested LEFT JOIN sources source ON source.object_id=requested.source_id
                CROSS JOIN {table} item ON item.object_id=requested.object_id {scope} {totals}
                WHERE requested.kind='{kind}' AND item.{name} IS NOT NULL"));
        }
        let mut connection = self.acquire_general(cancellation).await?;
        let rows = sqlx::query_as::<_, Row>(AssertSqlSafe(format!(
            "{REQUESTED} SELECT * FROM ({}) ORDER BY ordinal",
            parts.join(" UNION ALL ")
        )))
        .bind(serde_json::to_string(pins)?)
        .fetch_all(&mut *connection)
        .await?;
        rows.into_iter()
            .map(|row| {
                let pin = pins[row.ordinal as usize].clone();
                let owner = match &pin {
                    SidebarPin::Playlist {
                        source_id,
                        playlist_id,
                    } => Some((
                        source_id.as_ref().map(ToString::to_string),
                        playlist_id.clone(),
                    )),
                    _ => None,
                };
                let artwork = if let Some(owner) = owner {
                    Some(crate::artwork::playlist_artwork(
                        row.artwork_binding,
                        owner,
                        (None, None),
                    )?)
                } else {
                    row.artwork_binding
                };
                Ok(SidebarPinRow {
                    pin,
                    key: row.entity_key,
                    name: row.name,
                    totals: row.track_count.zip(row.duration_millis),
                    artwork: if let Some(artwork) = artwork {
                        vec![artwork]
                    } else {
                        serde_json::from_str::<Vec<String>>(&row.gallery)?
                            .into_iter()
                            .map(String::into_bytes)
                            .collect()
                    },
                })
            })
            .collect()
    }
}
