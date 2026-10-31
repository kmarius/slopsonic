use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{SourceError, SourceInputIdentity, SourceResult, subsonic::SubsonicFlavor};
use library::SourceId;

/// Credential-free source configuration persisted by Rufin Settings.
///
/// The provider payload is opaque outside Sources. Cache health never affects
/// this value or its separately stored credential reference.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceConfiguration {
    pub source_id: SourceId,
    pub kind: String,
    pub name: String,
    pub provider_payload: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CredentialInput {
    pub source_name: Option<String>,
    pub server_url: String,
    pub username: String,
    pub secret: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SourceSetup {
    OpenSubsonic {
        kind: SubsonicFlavor,
        authentication: crate::SubsonicAuthentication,
        credentials: CredentialInput,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CredentialPreset {
    pub source_name: String,
    pub server_url: String,
    pub username: String,
    pub trust_invalid_cert: bool,
    pub open_subsonic_authentication: Option<crate::SubsonicAuthentication>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SourceSettingsChange {
    OpenSubsonic {
        source_id: SourceId,
        kind: SubsonicFlavor,
        authentication: crate::SubsonicAuthentication,
        credentials: CredentialInput,
    },
}

impl SourceSettingsChange {
    pub fn source_id(&self) -> &SourceId {
        match self {
            Self::OpenSubsonic { source_id, .. } => source_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditableSource {
    Credentials {
        source_id: SourceId,
        kind: String,
        credentials: CredentialPreset,
    },
}

impl SourceConfiguration {
    pub fn server_address(&self) -> Option<String> {
        let address = match self.kind.as_str() {
            "navidrome" | "subsonic" => {
                crate::subsonic::SubsonicSourceConfig::from_configuration(self)
                    .ok()?
                    .base_url
            }
            _ => return None,
        };
        let mut url = url::Url::parse(&address).ok()?;
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        Some(url.into())
    }

    pub fn supports_playlist_public(&self) -> bool {
        matches!(self.kind.as_str(), "navidrome" | "subsonic")
    }

    /// Identify the source inputs that determine rebuildable canonical facts.
    ///
    /// Credentials and presentation settings do not participate, so Rufin can
    /// validate an existing cache even when live source access is unavailable.
    pub fn input_identity(&self) -> SourceResult<SourceInputIdentity> {
        self.input_identity_with_reader_versions(None)
    }

    fn input_identity_with_reader_versions(
        &self,
        navidrome_library_version: Option<u32>,
    ) -> SourceResult<SourceInputIdentity> {
        let mut digest = blake3::Hasher::new();
        digest.update(b"rufin-source-input");
        digest.update(&1_u32.to_le_bytes());
        digest_part(&mut digest, self.source_id.as_str().as_bytes());
        digest_part(&mut digest, self.kind.as_bytes());
        match self.kind.as_str() {
            "navidrome" | "subsonic" => {
                let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
                digest_part(&mut digest, config.username.as_bytes());
                let library_version =
                    navidrome_library_version.unwrap_or(config.navidrome_library_version);
                if library_version > 0 {
                    digest_part(&mut digest, b"navidrome-library");
                    digest.update(&library_version.to_le_bytes());
                }
            }
            kind => {
                return Err(SourceError::InvalidConfig(format!(
                    "unknown source kind {kind}"
                )));
            }
        }
        Ok(SourceInputIdentity {
            source_id: self.source_id.clone(),
            digest: *digest.finalize().as_bytes(),
        })
    }

    /// Return the fields Rufin may present when editing this source.
    pub fn editable(&self) -> SourceResult<EditableSource> {
        match self.kind.as_str() {
            "navidrome" | "subsonic" => {
                let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
                Ok(EditableSource::Credentials {
                    source_id: self.source_id.clone(),
                    kind: self.kind.clone(),
                    credentials: CredentialPreset {
                        source_name: self.name.clone(),
                        server_url: config.base_url,
                        username: config.username,
                        trust_invalid_cert: config.trust_invalid_cert,
                        open_subsonic_authentication: Some(config.authentication),
                    },
                })
            }
            kind => Err(SourceError::InvalidConfig(format!(
                "unknown source kind {kind}"
            ))),
        }
    }
}

fn digest_part(digest: &mut blake3::Hasher, value: &[u8]) {
    digest.update(&(value.len() as u64).to_le_bytes());
    digest.update(value);
}

pub(crate) fn decode_provider_payload<T: DeserializeOwned>(
    stored: &SourceConfiguration,
) -> SourceResult<T> {
    serde_json::from_str(&stored.provider_payload)
        .map_err(|error| SourceError::InvalidConfig(error.to_string()))
}

pub(crate) fn require_payload_version(actual: u32, expected: u32) -> SourceResult<()> {
    if actual != expected {
        return Err(SourceError::InvalidConfig(format!(
            "unsupported payload version {actual}"
        )));
    }
    Ok(())
}

pub(crate) fn encode_provider_payload(
    source_id: SourceId,
    kind: impl Into<String>,
    name: impl Into<String>,
    provider_payload: serde_json::Value,
) -> SourceConfiguration {
    SourceConfiguration {
        source_id,
        kind: kind.into(),
        name: name.into(),
        provider_payload: provider_payload.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::SourceId;

    use super::*;
    use crate::subsonic::SubsonicSourceConfig;

    fn migrated_source(kind: &str, payload: serde_json::Value) -> SourceConfiguration {
        SourceConfiguration {
            source_id: SourceId::new(format!("{kind}:server:test")),
            kind: kind.to_string(),
            name: "Test Source".to_string(),
            provider_payload: payload.to_string(),
        }
    }

    #[test]
    fn provider_payloads_decode_the_credential_free_saved_shape() {
        let subsonic_stored = migrated_source(
            "subsonic",
            serde_json::json!({
                "version": 1,
                "base_url": "https://subsonic.example",
                "user_id": "legacy-listener",
                "trust_invalid_cert": true,
            }),
        );
        let subsonic = SubsonicSourceConfig::from_configuration(&subsonic_stored)
            .expect("OpenSubsonic payload");
        assert_eq!(subsonic.base_url, "https://subsonic.example");
        assert_eq!(subsonic.username, "legacy-listener");
        assert!(subsonic.trust_invalid_cert);
        assert_eq!(
            subsonic.authentication,
            crate::subsonic::SubsonicAuthentication::Password
        );
        let subsonic_payload = subsonic.clone().into_payload();
        assert!(subsonic_payload.get("user_id").is_none());
        assert_eq!(
            decode_provider_payload::<serde_json::Value>(&encode_provider_payload(
                subsonic_stored.source_id,
                "subsonic",
                "Test Source",
                subsonic_payload.clone(),
            ))
            .expect("round-trip OpenSubsonic payload"),
            subsonic_payload,
        );
    }

    #[test]
    fn unsupported_provider_payload_version_is_rejected() {
        let stored = migrated_source(
            "subsonic",
            serde_json::json!({
                "version": 2,
                "base_url": "https://music.example",
                "username": "listener",
                "trust_invalid_cert": false,
            }),
        );

        let error = SubsonicSourceConfig::from_configuration(&stored)
            .expect_err("unsupported payload version");
        assert!(matches!(error, SourceError::InvalidConfig(_)));
    }
}

impl SourceConfiguration {
    pub fn recognize_media_locator(&self, locator: &str) -> Option<String> {
        let uri = url::Url::parse(locator).ok()?;
        let payload: serde_json::Value = serde_json::from_str(&self.provider_payload).ok()?;
        let base = url::Url::parse(payload.get("base_url")?.as_str()?).ok()?;
        if uri.origin() != base.origin()
            || !uri.path().starts_with(base.path().trim_end_matches('/'))
        {
            return None;
        }
        let path = uri.path().strip_prefix(base.path().trim_end_matches('/'))?;
        let object = if matches!(self.kind.as_str(), "subsonic" | "navidrome") {
            if !matches!(
                path,
                "/rest/stream" | "/rest/stream.view" | "/rest/download" | "/rest/download.view"
            ) {
                return None;
            }
            let id = uri.query_pairs().find(|(key, _)| key == "id")?.1;
            format!("{}:track:{id}", self.kind)
        } else {
            return None;
        };
        Some(library::source_entity_uri(
            &self.source_id,
            "track",
            &object,
        ))
    }
}

#[cfg(test)]
mod locator_tests {
    #[test]
    fn only_captured_adapter_recognizes_its_provider_locator() {
        let config = super::SourceConfiguration {
            source_id: library::SourceId::new("one"),
            kind: "navidrome".into(),
            name: "Music".into(),
            provider_payload: r#"{"version":1,"base_url":"https://music.test/server"}"#.into(),
        };
        assert_eq!(
            config.recognize_media_locator(
                "https://music.test/server/rest/stream.view?id=song&p=secret"
            ),
            Some(library::source_entity_uri(
                &config.source_id,
                "track",
                "navidrome:track:song"
            ))
        );
        assert!(
            config
                .recognize_media_locator("https://other.test/server/rest/stream.view?id=song")
                .is_none()
        );
        assert!(
            config
                .recognize_media_locator("https://music.test/unrelated/song")
                .is_none()
        );
    }
}
