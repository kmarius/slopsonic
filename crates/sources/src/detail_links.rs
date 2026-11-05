//! Destinations in a source's web client.

use url::Url;

use crate::{SourceConfiguration, SourceError, SourceResult};

impl SourceConfiguration {
    pub fn detail_web_url(&self, kind: &str, object_id: &str) -> SourceResult<String> {
        let flavor = self.kind.source_id();
        let id = entity_id(flavor, kind, object_id)?;
        let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
        let url = subsonic_url(&web_base(&config.base_url)?, flavor, kind, id)?
            .ok_or(SourceError::NotFound)?;
        Ok(url.into())
    }
}

fn entity_id<'a>(source: &str, kind: &str, id: &'a str) -> SourceResult<&'a str> {
    id.strip_prefix(&format!("{source}:{kind}:"))
        .filter(|id| !id.is_empty() && matches!(kind, "track" | "artist" | "album" | "playlist"))
        .ok_or(SourceError::NotFound)
}

fn web_base(base: &str) -> SourceResult<Url> {
    let mut url = Url::parse(base).map_err(url_error)?;
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

fn subsonic_url(base: &Url, server: &str, kind: &str, id: &str) -> SourceResult<Option<Url>> {
    if kind == "track" {
        return Ok(None);
    }
    let mut url = web_base(base.as_str())?;
    let path = url.path().trim_end_matches('/').to_owned();
    // These API mount points identify the web app even on older saved connections.
    if let Some(app) = path.strip_suffix("/apps/music/subsonic") {
        url.set_path(&format!("{app}/apps/music/"));
        let id = id.strip_prefix(&format!("{kind}-")).unwrap_or(id);
        url.set_fragment(Some(&format!("/{kind}/{}", encode(id))));
    } else if let Some(root) = path.strip_suffix("/api/subsonic") {
        url.set_path(&format!("{root}/library/{kind}s/{}", encode(id)));
    } else {
        match server.to_ascii_lowercase().as_str() {
            "navidrome" => {
                url = url.join("app/").map_err(url_error)?;
                url.set_fragment(Some(&format!("/{kind}/{}/show", encode(id))));
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(url))
}

fn encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn url_error(error: url::ParseError) -> SourceError {
    SourceError::Other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn configuration(
        kind: crate::SubsonicFlavor,
        payload: serde_json::Value,
    ) -> SourceConfiguration {
        SourceConfiguration {
            source_id: crate::SourceId::new("server"),
            kind,
            name: "My custom name".into(),
            provider_payload: payload.to_string(),
        }
    }

    #[test]
    fn server_detail_links_keep_mount_paths_and_encode_item_and_server_ids() {
        let source = configuration(
            crate::SubsonicFlavor::Navidrome,
            json!({"version":1,"base_url":"https://music.test/navidrome/","username":"listener","trust_invalid_cert":false}),
        );
        assert_eq!(
            source
                .detail_web_url("artist", "navidrome:artist:a/b")
                .unwrap(),
            "https://music.test/navidrome/app/#/artist/a%2Fb/show"
        );
    }

    #[test]
    fn subsonic_web_clients_use_their_real_routes_and_id_namespaces() {
        for (base, server, kind, id, expected) in [
            (
                "https://cloud.test/cloud/index.php/apps/music/subsonic/",
                "Nextcloud Music",
                "album",
                "album-42",
                "https://cloud.test/cloud/index.php/apps/music/#/album/42",
            ),
            (
                "https://cloud.test/apps/music/subsonic/",
                "ownCloud Music",
                "artist",
                "artist-17",
                "https://cloud.test/apps/music/#/artist/17",
            ),
            (
                "https://music.test/proxy/api/subsonic/",
                "funkwhale",
                "artist",
                "12",
                "https://music.test/proxy/library/artists/12",
            ),
            (
                "https://music.test/api/subsonic/",
                "funkwhale",
                "album",
                "15",
                "https://music.test/library/albums/15",
            ),
            (
                "https://music.test/nav/",
                "Navidrome",
                "album",
                "42",
                "https://music.test/nav/app/#/album/42/show",
            ),
        ] {
            assert_eq!(
                subsonic_url(&Url::parse(base).unwrap(), server, kind, id)
                    .unwrap()
                    .unwrap()
                    .as_str(),
                expected
            );
        }
        for server in ["airsonic", "gonic", "unknown"] {
            let source = configuration(
                crate::SubsonicFlavor::Subsonic,
                json!({"version":1,"base_url":format!("https://music.test/{server}/"),"username":"listener","trust_invalid_cert":false}),
            );
            assert!(source.detail_web_url("album", "subsonic:album:42").is_err());
        }
        for (base, entity, id, expected) in [
            (
                "https://cloud.test/apps/music/subsonic",
                "artist",
                "artist-42",
                "https://cloud.test/apps/music/#/artist/42",
            ),
            (
                "https://music.test/api/subsonic",
                "album",
                "42",
                "https://music.test/library/albums/42",
            ),
        ] {
            let source = configuration(
                crate::SubsonicFlavor::Subsonic,
                json!({"version":1,"base_url":base,"username":"listener","trust_invalid_cert":false}),
            );
            assert_eq!(
                source
                    .detail_web_url(entity, &format!("subsonic:{entity}:{id}"))
                    .unwrap(),
                expected
            );
        }
    }
}
