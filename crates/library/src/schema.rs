//! The durable library schema owns source facts and application references.
use crate::LibraryResult;
use sqlx::{Connection, SqliteConnection};

pub(crate) async fn pragma(
    connection: &mut SqliteConnection,
    name: &'static str,
) -> LibraryResult<i64> {
    Ok(
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("PRAGMA {name}")))
            .fetch_one(connection)
            .await?,
    )
}
pub(crate) const STORE_SCHEMA: &str = r#"PRAGMA application_id = 1381320270;

CREATE TABLE IF NOT EXISTS queue_contexts (
    context_key INTEGER PRIMARY KEY,
    context TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE IF NOT EXISTS queue_entries (
    entry_id INTEGER PRIMARY KEY,
    track_key INTEGER NOT NULL REFERENCES tracks,
    provenance TEXT,
    context_key INTEGER REFERENCES queue_contexts,
    source_rank INTEGER
) STRICT;
CREATE INDEX IF NOT EXISTS queue_entries_track_idx ON queue_entries(track_key);
CREATE INDEX IF NOT EXISTS queue_entries_context_idx ON queue_entries(context_key) WHERE context_key IS NOT NULL;
CREATE TABLE IF NOT EXISTS queue_positions (
    order_kind INTEGER NOT NULL,
    position INTEGER NOT NULL,
    entry_id INTEGER NOT NULL,
    PRIMARY KEY(order_kind,position)
) STRICT, WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS playlists (
    playlist_key INTEGER PRIMARY KEY,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    indexed INTEGER NOT NULL DEFAULT 1 CHECK(indexed IN (0,1)),
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    name TEXT,
    normalized_name TEXT,
    sort_text TEXT,
    position INTEGER NOT NULL CHECK (position >= 0),
    artwork_binding BLOB,
    writable INTEGER NOT NULL DEFAULT 1,
    metadata_writable INTEGER NOT NULL DEFAULT 1,
    provider_revision TEXT,
    valid_until INTEGER,
    UNIQUE (object_id)
) STRICT;


CREATE INDEX IF NOT EXISTS playlists_order_idx ON playlists(sort_text, playlist_key);
CREATE INDEX IF NOT EXISTS playlists_membership_idx ON playlists(indexed,playlist_key);

CREATE UNIQUE INDEX IF NOT EXISTS playlists_position_idx ON playlists(position);

CREATE TABLE IF NOT EXISTS playlist_entries (
    playlist_entry_key INTEGER PRIMARY KEY,
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL CHECK (object_id <> ''),
    track_key INTEGER NOT NULL REFERENCES tracks,
    position INTEGER NOT NULL CHECK (position >= 0),
    UNIQUE (playlist_key, object_id),
    UNIQUE (playlist_key, position)
) STRICT;
CREATE INDEX IF NOT EXISTS playlist_entries_order_idx ON playlist_entries(playlist_key, position);
CREATE INDEX IF NOT EXISTS playlist_entries_shuffle_idx ON playlist_entries(playlist_key,((playlist_entry_key*1103515245)%2147483647),playlist_entry_key);
CREATE INDEX IF NOT EXISTS playlist_entries_track_idx ON playlist_entries(track_key,playlist_key);

CREATE TABLE IF NOT EXISTS favorite_outbox (
    outbox_key INTEGER PRIMARY KEY,
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    favorite INTEGER NOT NULL CHECK (favorite IN (0, 1)),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0)
) STRICT;

CREATE INDEX IF NOT EXISTS favorite_outbox_due_idx ON favorite_outbox(next_attempt_at, outbox_key);

CREATE TABLE IF NOT EXISTS rating_outbox (
    outbox_key INTEGER PRIMARY KEY,
    media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
    rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 5),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0)
) STRICT;

CREATE INDEX IF NOT EXISTS rating_outbox_due_idx ON rating_outbox(next_attempt_at, outbox_key);

CREATE TABLE IF NOT EXISTS user_media_state (
    media_uri TEXT PRIMARY KEY CHECK (media_uri <> ''),
    favorite INTEGER CHECK (favorite IS NULL OR favorite IN (0,1)),
    rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 5)
) STRICT;

CREATE INDEX IF NOT EXISTS user_media_favorite_idx ON user_media_state(favorite,media_uri) WHERE favorite=1;

CREATE TABLE IF NOT EXISTS scrobble_outbox (
    outbox_key INTEGER PRIMARY KEY,
    play_id TEXT NOT NULL CHECK (play_id <> ''),
    service TEXT NOT NULL CHECK (service <> ''),
    account_id TEXT NOT NULL CHECK (account_id <> ''),
    track_title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    started_at INTEGER NOT NULL CHECK (started_at >= 0),
    duration_millis INTEGER NOT NULL CHECK (duration_millis >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at INTEGER CHECK (next_attempt_at IS NULL OR next_attempt_at >= 0),
    last_error TEXT,
    UNIQUE (service, account_id, play_id)
) STRICT;

CREATE INDEX IF NOT EXISTS scrobble_outbox_due_idx ON scrobble_outbox(next_attempt_at, outbox_key)
    WHERE next_attempt_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS queue_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    current_index INTEGER,
    progress_millis INTEGER NOT NULL DEFAULT 0 CHECK (progress_millis >= 0),
    repeat_mode TEXT NOT NULL DEFAULT 'none' CHECK (repeat_mode IN ('none', 'one', 'all')),
    shuffled INTEGER NOT NULL DEFAULT 0 CHECK (shuffled IN (0, 1))
) STRICT;

CREATE TABLE IF NOT EXISTS download_files (
    media_uri TEXT NOT NULL,
    encoding TEXT NOT NULL,
    revision TEXT NOT NULL,
    path TEXT NOT NULL,
    managed INTEGER NOT NULL,
    owners TEXT NOT NULL DEFAULT '[]',
    storage_root TEXT NOT NULL DEFAULT '',
    relative_path TEXT NOT NULL DEFAULT '',
    completed_size INTEGER,
    retired_documents TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (media_uri, encoding)
) STRICT;

CREATE INDEX IF NOT EXISTS download_files_path_idx ON download_files(path,encoding,revision);

CREATE TABLE IF NOT EXISTS sources (
    source_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL UNIQUE CHECK (object_id <> ''),
    display_name TEXT NOT NULL DEFAULT '', freshness BLOB,
    distinct_track_covers INTEGER NOT NULL DEFAULT 0 CHECK (distinct_track_covers IN (0,1)),
    catalog_revision INTEGER NOT NULL DEFAULT 0 CHECK (catalog_revision >= 0),
    catalog_complete INTEGER NOT NULL DEFAULT 0 CHECK (catalog_complete IN (0,1)),
    observation_stamp INTEGER NOT NULL DEFAULT 0,
    album_count INTEGER NOT NULL DEFAULT 0, track_count INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE IF NOT EXISTS native_media_state (
    media_uri TEXT PRIMARY KEY,
    favorite INTEGER, rating INTEGER, play_count INTEGER, last_played INTEGER
) STRICT;

CREATE INDEX IF NOT EXISTS native_media_favorite_idx ON native_media_state(favorite,media_uri) WHERE favorite=1;

CREATE TABLE IF NOT EXISTS albums (
    album_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL, media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    input_metadata BLOB,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    title_sort TEXT NOT NULL DEFAULT '', title_search TEXT NOT NULL DEFAULT '', artist_search TEXT NOT NULL DEFAULT '',
    album_artist_sort TEXT, year_sort INTEGER, release_sort TEXT, added_sort INTEGER,
    release_class INTEGER NOT NULL DEFAULT 0,
    track_count INTEGER NOT NULL DEFAULT 0, duration_millis INTEGER NOT NULL DEFAULT 0,
    downloaded_count INTEGER NOT NULL DEFAULT 0, play_count INTEGER NOT NULL DEFAULT 0, last_played INTEGER,
    native_metadata INTEGER NOT NULL DEFAULT 0,
    normalized_title TEXT,
    native_artwork BLOB,
    artwork_binding BLOB, artwork_track_key INTEGER, artwork_artist_key INTEGER,
    first_seen_at INTEGER,
    release_lookup_identity TEXT, release_identity TEXT,
    title TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.title')) VIRTUAL,
    display_artist TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.artist')) VIRTUAL,
    sort_text TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.sort_text')) VIRTUAL,
    year INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.year')) VIRTUAL,
    release_date TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.release_date')) VIRTUAL,
    date_added TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.date_added')) VIRTUAL,
    musicbrainz_release_id TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.musicbrainz_release_id')) VIRTUAL,
    musicbrainz_release_group_id TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.musicbrainz_release_group_id')) VIRTUAL,
    is_compilation INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.is_compilation')) VIRTUAL,
    UNIQUE(object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS albums_native_artwork_idx ON albums(native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS albums_artwork_track_idx ON albums(artwork_track_key) WHERE artwork_track_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS albums_artwork_artist_idx ON albums(artwork_artist_key) WHERE artwork_artist_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS albums_source_idx ON albums(indexed,album_key);

CREATE TABLE IF NOT EXISTS tracks (
    track_key INTEGER PRIMARY KEY,
    object_id TEXT,
    media_uri TEXT NOT NULL UNIQUE,
    album_key INTEGER REFERENCES albums ON DELETE SET NULL,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    source_path TEXT,
    first_seen_at INTEGER,
    native_artwork BLOB,
    artwork_kind INTEGER NOT NULL DEFAULT 0, artwork_artist_key INTEGER,
    audio_key BLOB NOT NULL DEFAULT (zeroblob(32)),
    album_title_sort TEXT, album_artist_sort TEXT, album_year_sort INTEGER,
    displayed_play_count INTEGER NOT NULL DEFAULT 0, displayed_last_played INTEGER,
    is_downloaded INTEGER NOT NULL DEFAULT 0,
    title TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.title')) VIRTUAL,
    display_artist TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.artist')) VIRTUAL,
    display_album TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.album')) VIRTUAL,
    sort_text TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.sort_text')) VIRTUAL,
    normalized_search TEXT NOT NULL DEFAULT '',
    duration_millis INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.duration_millis'),0)) STORED,
    disc_number INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.disc_number'),0)) VIRTUAL,
    track_number INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.track_number'),0)) VIRTUAL,
    year INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.year')) VIRTUAL,
    release_date TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.release_date')) VIRTUAL,
    date_added TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.date_added')) VIRTUAL,
    source_format TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.source_format')) VIRTUAL,
    bpm INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.bpm')) VIRTUAL,
    UNIQUE(object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS tracks_native_artwork_idx ON tracks(native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_artwork_artist_idx ON tracks(artwork_artist_key) WHERE artwork_artist_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_order_idx ON tracks(indexed,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_downloaded_order_idx ON tracks(indexed,sort_text,track_key) WHERE is_downloaded=1;
CREATE INDEX IF NOT EXISTS tracks_duration_idx ON tracks(track_key,duration_millis);
CREATE INDEX IF NOT EXISTS tracks_duration_order_idx ON tracks(indexed,duration_millis,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_duration_desc_order_idx ON tracks(indexed,duration_millis DESC,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_source_path_idx ON tracks(source_path,album_key);
CREATE INDEX IF NOT EXISTS tracks_album_order_idx ON tracks(album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_artist_order_idx ON tracks(indexed,display_artist,(album_year_sort IS NULL),album_year_sort,album_title_sort,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_artist_desc_order_idx ON tracks(indexed,display_artist DESC,(album_year_sort IS NULL),album_year_sort,album_title_sort,(album_key IS NULL),album_key,disc_number,track_number,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_year_idx ON tracks(indexed,year,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_release_idx ON tracks(indexed,release_date,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_added_idx ON tracks(indexed,(COALESCE(unixepoch(date_added),first_seen_at) IS NULL),COALESCE(unixepoch(date_added),first_seen_at),sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_added_desc_idx ON tracks(indexed,(COALESCE(unixepoch(date_added),first_seen_at) IS NULL),COALESCE(unixepoch(date_added),first_seen_at) DESC,sort_text,track_key);
CREATE INDEX IF NOT EXISTS tracks_raw_album_year_idx ON tracks(album_key,source_path,disc_number,track_number,object_id) WHERE indexed=1 AND year IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_raw_album_release_idx ON tracks(album_key,source_path,disc_number,track_number,object_id) WHERE indexed=1 AND release_date IS NOT NULL;
CREATE INDEX IF NOT EXISTS tracks_global_shuffle_idx ON tracks(((track_key*1103515245)%2147483647),media_uri);
CREATE INDEX IF NOT EXISTS tracks_indexed_idx ON tracks(indexed,track_key);

CREATE TABLE IF NOT EXISTS artists (
    artist_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    artwork_binding BLOB, artwork_album_key INTEGER, artwork_track_key INTEGER,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    musicbrainz_artist_id TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.musicbrainz_artist_id')) VIRTUAL,
    UNIQUE(object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS artists_native_artwork_idx ON artists(native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS artists_artwork_album_idx ON artists(artwork_album_key) WHERE artwork_album_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS artists_artwork_track_idx ON artists(artwork_track_key) WHERE artwork_track_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS artists_order_idx ON artists(sort_text,artist_key);
CREATE INDEX IF NOT EXISTS artists_membership_idx ON artists(indexed,artist_key);

CREATE TABLE IF NOT EXISTS genres (
    genre_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    UNIQUE(object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS genres_native_artwork_idx ON genres(native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS genres_order_idx ON genres(sort_text,genre_key);
CREATE INDEX IF NOT EXISTS genres_membership_idx ON genres(indexed,genre_key);

CREATE TABLE IF NOT EXISTS moods (
    mood_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    UNIQUE(object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS moods_order_idx ON moods(sort_text,mood_key);
CREATE INDEX IF NOT EXISTS moods_membership_idx ON moods(indexed,mood_key);

CREATE TABLE IF NOT EXISTS folders (
    folder_key INTEGER PRIMARY KEY,
    object_id TEXT NOT NULL,
    media_uri TEXT NOT NULL UNIQUE,
    indexed INTEGER NOT NULL DEFAULT 0,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
    native_metadata INTEGER NOT NULL DEFAULT 0,
    native_artwork BLOB,
    name TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.name'),object_id)) VIRTUAL,
    normalized_name TEXT NOT NULL DEFAULT '',
    sort_text TEXT GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.sort_text'),normalized_name)) VIRTUAL,
    UNIQUE(object_id)
) STRICT;
CREATE INDEX IF NOT EXISTS folders_native_artwork_idx ON folders(native_artwork) WHERE native_artwork IS NOT NULL;
CREATE INDEX IF NOT EXISTS folders_order_idx ON folders(sort_text,folder_key);
CREATE INDEX IF NOT EXISTS folders_membership_idx ON folders(indexed,folder_key);

CREATE INDEX IF NOT EXISTS albums_title_idx ON albums(title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_artist_idx ON albums((album_artist_sort IS NULL),album_artist_sort,(year_sort IS NULL),year_sort,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_year_idx ON albums(year_sort,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_release_idx ON albums(release_sort,title_sort,album_key);
CREATE INDEX IF NOT EXISTS albums_added_idx ON albums(added_sort,title_sort,album_key);

CREATE TABLE IF NOT EXISTS album_artists (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, artist_key)
) STRICT;

CREATE INDEX IF NOT EXISTS album_artists_artist_idx ON album_artists(artist_key, album_key);

CREATE TABLE IF NOT EXISTS track_album_artists (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL,
    PRIMARY KEY(track_key,artist_key),
    UNIQUE(track_key,position)
) STRICT;
CREATE INDEX IF NOT EXISTS track_album_artists_artist_idx ON track_album_artists(artist_key,track_key);

CREATE TABLE IF NOT EXISTS track_artists (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    artist_key INTEGER NOT NULL REFERENCES artists ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, artist_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_artists_artist_idx ON track_artists(artist_key, track_key);

CREATE TABLE IF NOT EXISTS album_genres (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position),
    UNIQUE (album_key, genre_key)
) STRICT;

CREATE INDEX IF NOT EXISTS album_genres_genre_idx ON album_genres(genre_key, album_key);

CREATE TABLE IF NOT EXISTS track_genres (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    genre_key INTEGER NOT NULL REFERENCES genres ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, genre_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_genres_genre_idx ON track_genres(genre_key, track_key);

CREATE TABLE IF NOT EXISTS track_moods (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    mood_key INTEGER NOT NULL REFERENCES moods ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, mood_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_moods_mood_idx ON track_moods(mood_key, track_key);

CREATE TABLE IF NOT EXISTS track_folders (
    track_key INTEGER NOT NULL REFERENCES tracks ON DELETE CASCADE,
    folder_key INTEGER NOT NULL REFERENCES folders ON DELETE CASCADE,
    observed_stamp INTEGER NOT NULL DEFAULT 0,
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (track_key, position),
    UNIQUE (track_key, folder_key)
) STRICT;

CREATE INDEX IF NOT EXISTS track_folders_folder_idx ON track_folders(folder_key, track_key);

CREATE TABLE IF NOT EXISTS album_release_types (
    album_key INTEGER NOT NULL REFERENCES albums ON DELETE CASCADE,
    release_type TEXT NOT NULL CHECK (release_type <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_key, position)
) STRICT;

CREATE TABLE IF NOT EXISTS home_entries (
    section_id TEXT NOT NULL CHECK (section_id <> ''),
    position INTEGER NOT NULL CHECK (position >= 0),
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('track', 'album', 'artist', 'playlist')),
    entity_key INTEGER NOT NULL,
    title TEXT NOT NULL,
    subtitle TEXT NOT NULL,
    section_title TEXT,
    PRIMARY KEY (section_id, position)
) STRICT;

CREATE TABLE IF NOT EXISTS artwork_previews(family TEXT PRIMARY KEY,hash TEXT NOT NULL) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS native_playlist_pending (
    playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
    object_id TEXT NOT NULL, track_key INTEGER NOT NULL REFERENCES tracks,
    position INTEGER NOT NULL,
    PRIMARY KEY(playlist_key,object_id)
) STRICT;

-- Server rows need the server; a write racing a server reset aborts instead of reviving the catalog.
CREATE TRIGGER IF NOT EXISTS server_required_albums BEFORE INSERT ON albums WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_artists BEFORE INSERT ON artists WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_genres BEFORE INSERT ON genres WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_moods BEFORE INSERT ON moods WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_folders BEFORE INSERT ON folders WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_playlists BEFORE INSERT ON playlists WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_home_entries BEFORE INSERT ON home_entries WHEN NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_tracks BEFORE INSERT ON tracks WHEN NEW.object_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
CREATE TRIGGER IF NOT EXISTS server_required_track_claims BEFORE UPDATE OF object_id ON tracks WHEN NEW.object_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM sources) BEGIN SELECT RAISE(ABORT,'no server is configured'); END;
"#;

const BASE_VERSION: i64 = 64;

/// The encoded server segment of a former `slopsonic:source/<kind>/<server>/<object>` URI column.
macro_rules! server_segment {
    ($column:literal) => {
        concat!(
            "substr(substr(substr(",
            $column,
            ",18),instr(substr(",
            $column,
            ",18),'/')+1),1,",
            "instr(substr(substr(",
            $column,
            ",18),instr(substr(",
            $column,
            ",18),'/')+1),'/')-1)"
        )
    };
}

/// Rewrites a former `slopsonic:source/<kind>/<server>/<object>` URI column to `slopsonic:<kind>/<object>`.
macro_rules! single_server_uri {
    ($column:literal) => {
        concat!(
            "CASE WHEN ",
            $column,
            " LIKE 'slopsonic:source/%' THEN 'slopsonic:'",
            "||substr(substr(",
            $column,
            ",18),1,instr(substr(",
            $column,
            ",18),'/')-1)||'/'",
            "||substr(substr(substr(",
            $column,
            ",18),instr(substr(",
            $column,
            ",18),'/')+1),",
            "instr(substr(substr(",
            $column,
            ",18),instr(substr(",
            $column,
            ",18),'/')+1),'/')+1)",
            " ELSE ",
            $column,
            " END"
        )
    };
}

const MIGRATIONS: &[&str] = &[
    // 65: drop storage for playlist folders, local-file albums and non-download locators.
    "ALTER TABLE playlists DROP COLUMN playlist_folder_key;
     DROP TABLE playlist_folders;
     DROP TABLE album_release_observations;
     ALTER TABLE albums DROP COLUMN cached_local;
     DROP VIEW download_access_files;
     DELETE FROM download_locators WHERE origin<>'download';
     CREATE TABLE download_locators_new (
         download_access_file_key INTEGER PRIMARY KEY,
         source_key INTEGER REFERENCES sources ON DELETE CASCADE,
         media_uri TEXT NOT NULL CHECK(media_uri<>''),
         origin TEXT NOT NULL CHECK(origin='download'),
         path TEXT NOT NULL CHECK(path<>''), root TEXT NOT NULL, relative_path TEXT NOT NULL,
         access_uri TEXT NOT NULL CHECK(access_uri<>''),
         UNIQUE(media_uri,origin)
     ) STRICT;
     INSERT INTO download_locators_new SELECT * FROM download_locators;
     DROP TABLE download_locators;
     ALTER TABLE download_locators_new RENAME TO download_locators;",
    // 66: drop the never-written album audio acceptance.
    "ALTER TABLE albums DROP COLUMN accepted_audio;",
    // 67: drop app-owned playlists; every playlist belongs to the server.
    "DELETE FROM playlists WHERE owner<>'source';
     DROP INDEX playlists_global_object_idx;
     ALTER TABLE playlists DROP COLUMN owner;
     ALTER TABLE playlists DROP COLUMN artwork_bytes;
     ALTER TABLE playlists DROP COLUMN artwork_mime;
     ALTER TABLE playlists DROP COLUMN artwork_revision;",
    // 68: drop local listening history; keep only undelivered scrobbles, with their payload.
    "CREATE TABLE scrobble_outbox (
         outbox_key INTEGER PRIMARY KEY,
         play_id TEXT NOT NULL CHECK (play_id <> ''),
         service TEXT NOT NULL CHECK (service <> ''),
         account_id TEXT NOT NULL CHECK (account_id <> ''),
         track_title TEXT NOT NULL,
         artist_name TEXT NOT NULL,
         album_title TEXT NOT NULL,
         started_at INTEGER NOT NULL CHECK (started_at >= 0),
         duration_millis INTEGER NOT NULL CHECK (duration_millis >= 0),
         attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
         next_attempt_at INTEGER CHECK (next_attempt_at IS NULL OR next_attempt_at >= 0),
         last_error TEXT,
         UNIQUE (service, account_id, play_id)
     ) STRICT;
     INSERT INTO scrobble_outbox(play_id,service,account_id,track_title,artist_name,album_title,
             started_at,duration_millis,attempts,next_attempt_at,last_error)
         SELECT coalesce(listen.external_id,'listen-'||listen.listen_key),outbox.service,outbox.account_id,
             listen.track_title,listen.artist_name,listen.album_title,listen.started_at,listen.duration_millis,
             outbox.attempts,outbox.next_attempt_at,outbox.last_error
         FROM listen_outbox outbox JOIN listens listen USING(listen_key);
     DROP TABLE listen_outbox;
     DROP TABLE listens;
     DROP INDEX tracks_local_play_order_idx;
     DROP INDEX tracks_global_play_order_idx;
     DROP INDEX tracks_local_shuffle_idx;
     DROP INDEX tracks_global_local_shuffle_idx;
     ALTER TABLE tracks DROP COLUMN local_play_count;",
    // 69: drop skip counts; no server reports them.
    "ALTER TABLE native_media_state DROP COLUMN skip_count;
     ALTER TABLE tracks DROP COLUMN displayed_skip_count;",
    // 70: overrides only cover undelivered edits; queue standing ratings for delivery.
    "CREATE TABLE rating_outbox (
         outbox_key INTEGER PRIMARY KEY,
         media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
         rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 10),
         attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
         next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0)
     ) STRICT;
     CREATE INDEX rating_outbox_due_idx ON rating_outbox(next_attempt_at, outbox_key);
     INSERT INTO rating_outbox(media_uri,rating,next_attempt_at)
         SELECT media_uri,rating/10,0 FROM user_media_state
         WHERE rating IS NOT NULL AND media_uri LIKE 'slopsonic:source/%';
     UPDATE user_media_state SET rating=NULL WHERE media_uri NOT IN (SELECT media_uri FROM rating_outbox);
     UPDATE user_media_state SET favorite=NULL WHERE media_uri NOT IN (SELECT media_uri FROM favorite_outbox);
     DELETE FROM user_media_state WHERE favorite IS NULL AND rating IS NULL;
     ALTER TABLE user_media_state DROP COLUMN extra;
     ALTER TABLE favorite_outbox DROP COLUMN previous_favorite;",
    // 71: a download's revision is the audio key it was fetched from; drop ReplayGain gating keys.
    "DROP VIEW download_access_files;
     UPDATE download_files SET revision=coalesce(
         (SELECT hex(metadata.loudness_analysis_key) FROM download_access_metadata metadata
             WHERE metadata.access_uri=download_files.path AND metadata.loudness_analysis_key IS NOT NULL),
         (SELECT hex(track.source_loudness_analysis_key) FROM tracks track WHERE track.media_uri=download_files.media_uri),
         '');
     ALTER TABLE download_access_metadata DROP COLUMN loudness_analysis_key;
     DROP INDEX tracks_album_local_audio_idx;
     ALTER TABLE tracks DROP COLUMN loudness_analysis_key;
     ALTER TABLE tracks RENAME COLUMN source_loudness_analysis_key TO audio_key;
     ALTER TABLE albums DROP COLUMN loudness_analysis_key;
     ALTER TABLE albums DROP COLUMN source_loudness_analysis_key;",
    // 72: one server. Media URIs lose the server id and the catalog cache is rebuilt by the
    // next sync; the queue keeps its tracks, and the kept server's outboxes, user state and
    // downloads keep their rows. Rows of any older server are dropped instead of re-pointed.
    // Upgrades run with foreign keys off, so the former cascades from `sources` are explicit.
    concat!(
        "DROP VIEW download_access_files;
     CREATE TEMP TABLE kept_server(segment TEXT PRIMARY KEY);
     INSERT OR IGNORE INTO kept_server SELECT ",
        server_segment!("media_uri"),
        " FROM (SELECT media_uri FROM tracks WHERE source_key=(SELECT max(source_key) FROM sources)
         UNION ALL SELECT media_uri FROM albums WHERE source_key=(SELECT max(source_key) FROM sources)
         UNION ALL SELECT media_uri FROM artists WHERE source_key=(SELECT max(source_key) FROM sources))
         WHERE media_uri LIKE 'slopsonic:source/%';
     DELETE FROM user_media_state WHERE media_uri LIKE 'slopsonic:source/%' AND EXISTS(SELECT 1 FROM kept_server) AND ",
        server_segment!("media_uri"),
        " NOT IN (SELECT segment FROM kept_server);
     DELETE FROM favorite_outbox WHERE media_uri LIKE 'slopsonic:source/%' AND EXISTS(SELECT 1 FROM kept_server) AND ",
        server_segment!("media_uri"),
        " NOT IN (SELECT segment FROM kept_server);
     DELETE FROM rating_outbox WHERE media_uri LIKE 'slopsonic:source/%' AND EXISTS(SELECT 1 FROM kept_server) AND ",
        server_segment!("media_uri"),
        " NOT IN (SELECT segment FROM kept_server);
     DELETE FROM download_files WHERE source_id IS NOT NULL
         AND source_id IS NOT (SELECT object_id FROM sources WHERE source_key=(SELECT max(source_key) FROM sources));
     DROP TABLE kept_server;
     DELETE FROM collection_album_index;
     DELETE FROM collection_totals;
     DELETE FROM folder_children;
     DELETE FROM playlist_genres;
     DELETE FROM home_entries;
     DELETE FROM playlists;
     DELETE FROM album_artists;
     DELETE FROM album_genres;
     DELETE FROM album_release_types;
     DELETE FROM track_artists;
     DELETE FROM track_album_artists;
     DELETE FROM track_genres;
     DELETE FROM track_moods;
     DELETE FROM track_folders;
     UPDATE tracks SET album_key=NULL,artwork_artist_key=NULL;
     DELETE FROM albums;
     DELETE FROM artists;
     DELETE FROM genres;
     DELETE FROM moods;
     DELETE FROM folders;
     DELETE FROM tracks WHERE track_key NOT IN (SELECT track_key FROM queue_entries);
     DELETE FROM download_locators WHERE source_key<>(SELECT max(source_key) FROM sources);
     UPDATE tracks SET source_key=NULL WHERE source_key<>(SELECT max(source_key) FROM sources);
     DELETE FROM sources WHERE source_key<>(SELECT max(source_key) FROM sources);
     UPDATE sources SET object_id='server',freshness=NULL,catalog_complete=0,album_count=0,
         track_count=0,observation_stamp=observation_stamp+1;
     DELETE FROM native_media_state;
     DELETE FROM artwork_previews;
     UPDATE tracks SET indexed=0,observed_stamp=0,native_artwork=NULL,artwork_kind=0;
     UPDATE OR IGNORE tracks SET media_uri=",
        single_server_uri!("media_uri"),
        " WHERE source_key IS NOT NULL;
     UPDATE OR REPLACE user_media_state SET media_uri=",
        single_server_uri!("media_uri"),
        ";
     UPDATE OR REPLACE favorite_outbox SET media_uri=",
        single_server_uri!("media_uri"),
        ";
     UPDATE OR REPLACE rating_outbox SET media_uri=",
        single_server_uri!("media_uri"),
        ";
     UPDATE OR REPLACE download_files SET media_uri=",
        single_server_uri!("media_uri"),
        ";
     UPDATE OR REPLACE download_locators SET media_uri=",
        single_server_uri!("media_uri"),
        ";
     DROP INDEX download_source_idx;
     ALTER TABLE download_files DROP COLUMN source_id;"
    ),
    // 73: drop the per-server keys. Runs with foreign keys off; the catalog cache tables are
    // dropped and recreated empty for the next sync, queued tracks and download locators are copied.
    "DROP VIEW IF EXISTS download_access_files;
     DROP TABLE collection_album_index;
     DROP TABLE collection_totals;
     DROP TABLE folder_children;
     DROP TABLE playlist_genres;
     DROP TABLE home_entries;
     DROP TABLE native_playlist_pending;
     DROP TABLE playlist_entries;
     DROP TABLE playlists;
     DROP TABLE album_artists;
     DROP TABLE album_genres;
     DROP TABLE album_release_types;
     DROP TABLE track_artists;
     DROP TABLE track_album_artists;
     DROP TABLE track_genres;
     DROP TABLE track_moods;
     DROP TABLE track_folders;
     DROP TABLE albums;
     DROP TABLE artists;
     DROP TABLE genres;
     DROP TABLE moods;
     DROP TABLE folders;
     DELETE FROM native_media_state;
     DELETE FROM artwork_previews;
     CREATE TABLE tracks_new (
         track_key INTEGER PRIMARY KEY,
         object_id TEXT,
         media_uri TEXT NOT NULL UNIQUE,
         album_key INTEGER REFERENCES albums ON DELETE SET NULL,
         indexed INTEGER NOT NULL DEFAULT 0,
         observed_stamp INTEGER NOT NULL DEFAULT 0,
         metadata BLOB NOT NULL DEFAULT (jsonb('{}')),
         native_metadata INTEGER NOT NULL DEFAULT 0,
         source_path TEXT,
         first_seen_at INTEGER,
         native_artwork BLOB,
         artwork_kind INTEGER NOT NULL DEFAULT 0, artwork_artist_key INTEGER,
         audio_key BLOB NOT NULL DEFAULT (zeroblob(32)),
         album_title_sort TEXT, album_artist_sort TEXT, album_year_sort INTEGER,
         displayed_play_count INTEGER NOT NULL DEFAULT 0, displayed_last_played INTEGER,
         is_downloaded INTEGER NOT NULL DEFAULT 0,
         title TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.title')) VIRTUAL,
         display_artist TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.artist')) VIRTUAL,
         display_album TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.album')) VIRTUAL,
         sort_text TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.sort_text')) VIRTUAL,
         normalized_search TEXT NOT NULL DEFAULT '',
         duration_millis INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.duration_millis'),0)) STORED,
         disc_number INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.disc_number'),0)) VIRTUAL,
         track_number INTEGER GENERATED ALWAYS AS (coalesce(json_extract(metadata,'$.track_number'),0)) VIRTUAL,
         year INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.year')) VIRTUAL,
         release_date TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.release_date')) VIRTUAL,
         date_added TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.date_added')) VIRTUAL,
         source_format TEXT GENERATED ALWAYS AS (json_extract(metadata,'$.source_format')) VIRTUAL,
         bpm INTEGER GENERATED ALWAYS AS (json_extract(metadata,'$.bpm')) VIRTUAL,
         UNIQUE(object_id)
     ) STRICT;
     INSERT INTO tracks_new(track_key,object_id,media_uri,metadata,native_metadata,source_path,
             first_seen_at,native_artwork,artwork_kind,audio_key,displayed_play_count,
             displayed_last_played,is_downloaded,normalized_search)
         SELECT track_key,CASE WHEN source_key IN (SELECT source_key FROM sources) THEN object_id END,
             media_uri,metadata,native_metadata,source_path,first_seen_at,native_artwork,
             artwork_kind,audio_key,displayed_play_count,displayed_last_played,is_downloaded,
             normalized_search
         FROM tracks WHERE track_key IN (SELECT track_key FROM queue_entries);
     DROP TABLE tracks;
     ALTER TABLE tracks_new RENAME TO tracks;
     CREATE TABLE download_locators_new (
         download_access_file_key INTEGER PRIMARY KEY,
         media_uri TEXT NOT NULL CHECK(media_uri<>''),
         origin TEXT NOT NULL CHECK(origin='download'),
         path TEXT NOT NULL CHECK(path<>''), root TEXT NOT NULL, relative_path TEXT NOT NULL,
         access_uri TEXT NOT NULL CHECK(access_uri<>''),
         UNIQUE(media_uri,origin)
     ) STRICT;
     INSERT INTO download_locators_new
         SELECT download_access_file_key,media_uri,origin,path,root,relative_path,access_uri
         FROM download_locators;
     DROP TABLE download_locators;
     ALTER TABLE download_locators_new RENAME TO download_locators;
     UPDATE sources SET freshness=NULL,catalog_complete=0,album_count=0,track_count=0,
         observation_stamp=observation_stamp+1;",
    // 74: drop the locator index on `origin`, which always holds 'download'.
    "DROP INDEX IF EXISTS download_locators_source_idx;",
    // 75: playback reads download_files directly; the locators and write-only file metadata go.
    // Ratings count whole stars, 0 to 5.
    "DROP VIEW IF EXISTS download_access_files;
     DROP TABLE download_access_metadata;
     DROP TABLE download_locators;
     UPDATE native_media_state SET rating=(rating+10)/20 WHERE rating IS NOT NULL;
     CREATE TABLE user_media_state_new (
         media_uri TEXT PRIMARY KEY CHECK (media_uri <> ''),
         favorite INTEGER CHECK (favorite IS NULL OR favorite IN (0,1)),
         rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 5)
     ) STRICT;
     INSERT INTO user_media_state_new SELECT media_uri,favorite,(rating+10)/20 FROM user_media_state;
     DROP TABLE user_media_state;
     ALTER TABLE user_media_state_new RENAME TO user_media_state;
     CREATE TABLE rating_outbox_new (
         outbox_key INTEGER PRIMARY KEY,
         media_uri TEXT NOT NULL UNIQUE CHECK (media_uri <> ''),
         rating INTEGER CHECK (rating IS NULL OR rating BETWEEN 0 AND 5),
         attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
         next_attempt_at INTEGER NOT NULL CHECK (next_attempt_at >= 0)
     ) STRICT;
     INSERT INTO rating_outbox_new SELECT outbox_key,media_uri,(rating+1)/2,attempts,next_attempt_at FROM rating_outbox;
     DROP TABLE rating_outbox;
     ALTER TABLE rating_outbox_new RENAME TO rating_outbox;",
    // 76: drop the server columns nothing reads.
    "ALTER TABLE sources DROP COLUMN kind;
     ALTER TABLE sources DROP COLUMN normalized_name;
     ALTER TABLE sources DROP COLUMN artwork_digest;",
];

pub(crate) const SCHEMA_VERSION: i64 = BASE_VERSION + MIGRATIONS.len() as i64;

async fn migrate(
    connection: &mut SqliteConnection,
    version: i64,
    migrations: &[&str],
) -> LibraryResult<()> {
    let start = version - BASE_VERSION;
    if start < 0 || start > migrations.len() as i64 {
        return Err(crate::LibraryError::Migration(format!(
            "unsupported library schema {version}"
        )));
    }
    for sql in &migrations[start as usize..] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(*sql))
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

pub(crate) async fn initialize_durable(connection: &mut SqliteConnection) -> LibraryResult<()> {
    let version = pragma(connection, "user_version").await?;
    let upgrading = version != 0 && version < SCHEMA_VERSION;
    if upgrading {
        // Migrations rebuild tables; dropping one must not cascade into the tables that reference it.
        sqlx::raw_sql("PRAGMA foreign_keys=OFF")
            .execute(&mut *connection)
            .await?;
    }
    let result = initialize_in_transaction(connection, version, upgrading).await;
    if upgrading {
        sqlx::raw_sql("PRAGMA foreign_keys=ON")
            .execute(&mut *connection)
            .await?;
    }
    result
}

async fn initialize_in_transaction(
    connection: &mut SqliteConnection,
    version: i64,
    upgrading: bool,
) -> LibraryResult<()> {
    let mut transaction = connection.begin().await?;
    if upgrading {
        let triggers: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'catalog\\_index\\_%' ESCAPE '\\'",
        )
        .fetch_all(&mut *transaction)
        .await?;
        for name in triggers {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER \"{name}\"")))
                .execute(&mut *transaction)
                .await?;
        }
    }
    if version != 0 {
        migrate(&mut transaction, version, MIGRATIONS).await?;
    }
    sqlx::raw_sql(STORE_SCHEMA)
        .execute(&mut *transaction)
        .await?;
    if version == 0 {
        crate::catalog::index::initialize(&mut transaction).await?;
    } else if upgrading {
        crate::catalog::index::recreate_triggers(&mut transaction).await?;
    }
    if upgrading {
        let violation: Option<String> =
            sqlx::query_scalar("SELECT \"table\" FROM pragma_foreign_key_check")
                .fetch_optional(&mut *transaction)
                .await?;
        if let Some(table) = violation {
            return Err(crate::LibraryError::Migration(format!(
                "foreign key violation in {table} after migration"
            )));
        }
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "PRAGMA user_version = {SCHEMA_VERSION}"
    )))
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use sqlx::Connection;

    #[tokio::test]
    async fn applies_pending_migrations_only() {
        let mut connection = SqliteConnection::connect(":memory:").await.unwrap();
        let migrations = ["CREATE TABLE a(x)", "CREATE TABLE b(x)"];
        migrate(&mut connection, BASE_VERSION + 1, &migrations)
            .await
            .unwrap();
        assert!(
            sqlx::raw_sql("SELECT * FROM b")
                .execute(&mut connection)
                .await
                .is_ok()
        );
        assert!(
            sqlx::raw_sql("SELECT * FROM a")
                .execute(&mut connection)
                .await
                .is_err()
        );
        assert!(
            migrate(&mut connection, BASE_VERSION + 3, &migrations)
                .await
                .is_err()
        );
        assert!(
            migrate(&mut connection, BASE_VERSION - 1, &migrations)
                .await
                .is_err()
        );
    }
}
