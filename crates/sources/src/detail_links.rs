//! Destinations in a source's web client.

use url::Url;

use crate::{SourceConfiguration, SourceError, SourceResult, SubsonicFlavor};

impl SourceConfiguration {
    /// Only Navidrome ships a web client with stable per-item routes.
    pub fn detail_web_url(&self, kind: &str, object_id: &str) -> SourceResult<String> {
        if self.kind != SubsonicFlavor::Navidrome || kind == "track" {
            return Err(SourceError::NotFound);
        }
        let id = entity_id(kind, object_id)?;
        let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
        let mut url = web_base(&config.base_url)?
            .join("app/")
            .map_err(url_error)?;
        url.set_fragment(Some(&format!("/{kind}/{}/show", encode(id))));
        Ok(url.into())
    }
}

fn entity_id<'a>(kind: &str, id: &'a str) -> SourceResult<&'a str> {
    id.strip_prefix("navidrome:")
        .and_then(|rest| rest.strip_prefix(kind))
        .and_then(|rest| rest.strip_prefix(':'))
        .filter(|id| !id.is_empty() && matches!(kind, "artist" | "album" | "playlist"))
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
    fn only_navidrome_has_web_routes() {
        let navidrome = configuration(
            crate::SubsonicFlavor::Navidrome,
            json!({"version":1,"base_url":"https://music.test/nav/","username":"listener","trust_invalid_cert":false}),
        );
        assert_eq!(
            navidrome
                .detail_web_url("album", "navidrome:album:42")
                .unwrap(),
            "https://music.test/nav/app/#/album/42/show"
        );
        assert!(
            navidrome
                .detail_web_url("track", "navidrome:track:42")
                .is_err()
        );
        assert!(
            navidrome
                .detail_web_url("album", "subsonic:album:42")
                .is_err()
        );
        let subsonic = configuration(
            crate::SubsonicFlavor::Subsonic,
            json!({"version":1,"base_url":"https://music.test/airsonic/","username":"listener","trust_invalid_cert":false}),
        );
        assert!(
            subsonic
                .detail_web_url("album", "subsonic:album:42")
                .is_err()
        );
    }
}
