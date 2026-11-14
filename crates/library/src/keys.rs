//! Shared integer identities used at Library and consumer boundaries.

use std::fmt;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

/// The media URI of a server item: `slopsonic:<kind>/<percent-encoded object id>`.
pub fn entity_uri(entity_kind: &str, object_id: &str) -> String {
    format!("{}{}", entity_prefix(entity_kind), encode(object_id))
}

pub(crate) fn entity_prefix(entity_kind: &str) -> String {
    debug_assert!(matches!(
        entity_kind,
        "track" | "album" | "artist" | "genre" | "mood" | "folder"
    ));
    format!("slopsonic:{entity_kind}/")
}

/// The kind and object id of a track, album or artist URI.
pub fn entity_parts(uri: &str) -> Option<(String, String)> {
    let (kind, encoded) = uri.strip_prefix("slopsonic:")?.split_once('/')?;
    if !matches!(kind, "track" | "album" | "artist") || encoded.contains('/') {
        return None;
    }
    let object = decode(encoded).filter(|object| !object.is_empty())?;
    Some((kind.to_string(), object))
}

/// Rewrites a former `slopsonic:source/<kind>/<server>/<object>` URI; others pass through.
pub fn single_server_uri(uri: &str) -> String {
    let parts = uri
        .strip_prefix("slopsonic:source/")
        .and_then(|rest| rest.split_once('/'))
        .and_then(|(kind, rest)| Some((kind, rest.split_once('/')?.1)));
    match parts {
        Some((kind, object)) => format!("slopsonic:{kind}/{object}"),
        None => uri.to_owned(),
    }
}

pub fn file_media_path(uri: &str) -> Option<std::path::PathBuf> {
    let parsed = url::Url::parse(uri).ok()?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    parsed.to_file_path().ok()
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn decode(value: &str) -> Option<String> {
    percent_decode_str(value)
        .decode_utf8()
        .ok()
        .map(|value| value.into_owned())
}

macro_rules! integer_key {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Eq,
            Hash,
            Ord,
            PartialEq,
            PartialOrd,
            serde::Deserialize,
            serde::Serialize,
            sqlx::Type,
        )]
        #[repr(transparent)]
        #[sqlx(transparent)]
        pub struct $name(i64);

        impl $name {
            pub const fn from_raw(value: i64) -> Self {
                Self(value)
            }

            pub const fn raw(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

integer_key!(SourceKey);
integer_key!(TrackKey);
integer_key!(AlbumKey);
integer_key!(ArtistKey);
integer_key!(GenreKey);
integer_key!(MoodKey);
integer_key!(FolderKey);
integer_key!(PlaylistKey);
integer_key!(PlaylistEntryKey);
integer_key!(ListenOutboxKey);
integer_key!(DownloadAccessFileKey);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_uri_round_trips_percent_encoded_object_ids() {
        let uri = entity_uri("track", "opaque:/track%id");
        assert!(uri.starts_with("slopsonic:track/"));
        assert!(uri.contains("%2F"));
        assert_eq!(
            entity_parts(&uri),
            Some(("track".to_string(), "opaque:/track%id".to_string()))
        );
        assert_eq!(entity_parts("slopsonic:track/a/b"), None);
        assert_eq!(entity_parts("slopsonic:genre/rock"), None);
        assert_eq!(entity_parts("slopsonic:track/"), None);
    }

    #[test]
    fn former_server_uris_drop_the_server_segment() {
        assert_eq!(
            single_server_uri("slopsonic:source/album/slopsonic%2Dsource%2Dabc/a%2F1"),
            "slopsonic:album/a%2F1"
        );
        assert_eq!(single_server_uri("slopsonic:track/t"), "slopsonic:track/t");
        assert_eq!(single_server_uri("file:///a.flac"), "file:///a.flac");
    }

    #[test]
    fn file_operation_paths_decode_spaces_unicode_and_literal_percent_signs() {
        let path = std::env::temp_dir()
            .join("Slopsonic music")
            .join("Björk 100% #1.flac");
        let uri = url::Url::from_file_path(&path).unwrap();
        assert_eq!(file_media_path(uri.as_str()), Some(path));
        assert!(file_media_path("https://example.test/song.flac").is_none());
        assert!(file_media_path("file://user:secret@localhost/song.flac").is_none());
    }
}
