use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{SourceError, SourceResult, subsonic::SubsonicFlavor};

/// Credential-free source configuration persisted by Slopsonic Settings.
///
/// The provider payload is opaque outside Sources. Cache health never affects
/// this value or the separately stored credential.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceConfiguration {
    pub kind: SubsonicFlavor,
    pub name: String,
    pub provider_payload: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub struct CredentialInput {
    pub source_name: Option<String>,
    pub server_url: String,
    pub username: String,
    pub secret: String,
    pub trust_invalid_cert: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSetup {
    pub authentication: crate::SubsonicAuthentication,
    pub credentials: CredentialInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialPreset {
    pub source_name: String,
    pub server_url: String,
    pub username: String,
    pub trust_invalid_cert: bool,
    pub authentication: crate::SubsonicAuthentication,
}

impl SourceConfiguration {
    pub fn server_address(&self) -> Option<String> {
        let address = crate::subsonic::SubsonicSourceConfig::from_configuration(self)
            .ok()?
            .base_url;
        let mut url = url::Url::parse(&address).ok()?;
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        Some(url.into())
    }

    /// Return the fields Slopsonic may present when editing this source.
    pub fn editable(&self) -> SourceResult<CredentialPreset> {
        let config = crate::subsonic::SubsonicSourceConfig::from_configuration(self)?;
        Ok(CredentialPreset {
            source_name: self.name.clone(),
            server_url: config.base_url,
            username: config.username,
            trust_invalid_cert: config.trust_invalid_cert,
            authentication: config.authentication,
        })
    }
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

impl CredentialPreset {
    /// Whether saving `input` may count as a server switch, which resets local server state.
    /// The server canonicalizes the username on connect, so this can warn where `same_server` then
    /// matches. A new API key names its account itself (`tokenInfo`), so entering one always may.
    pub fn switches_server(
        &self,
        input: &CredentialInput,
        authentication: crate::SubsonicAuthentication,
    ) -> bool {
        crate::source::comparable_address(&self.server_url)
            != crate::source::comparable_address(&input.server_url)
            || self.username.trim() != input.username.trim()
            || authentication == crate::SubsonicAuthentication::ApiKey && !input.secret.is_empty()
    }
}

impl SourceConfiguration {
    /// Whether both reach the same account on the same server; anything else is a server switch.
    pub fn same_server(&self, other: &Self) -> bool {
        use crate::subsonic::SubsonicSourceConfig;
        match (
            SubsonicSourceConfig::from_configuration(self),
            SubsonicSourceConfig::from_configuration(other),
        ) {
            (Ok(left), Ok(right)) => {
                crate::source::comparable_address(&left.base_url)
                    == crate::source::comparable_address(&right.base_url)
                    && left.username == right.username
            }
            _ => false,
        }
    }
}

pub(crate) fn encode_provider_payload(
    kind: SubsonicFlavor,
    name: impl Into<String>,
    provider_payload: serde_json::Value,
) -> SourceConfiguration {
    SourceConfiguration {
        kind,
        name: name.into(),
        provider_payload: provider_payload.to_string(),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::subsonic::SubsonicSourceConfig;

    fn migrated_source(kind: SubsonicFlavor, payload: serde_json::Value) -> SourceConfiguration {
        SourceConfiguration {
            kind,
            name: "Test Source".to_string(),
            provider_payload: payload.to_string(),
        }
    }

    #[test]
    fn same_server_compares_address_and_canonical_username() {
        let source = |url: &str, username: &str, trust: bool| {
            migrated_source(
                SubsonicFlavor::Navidrome,
                serde_json::json!({
                    "version": 1,
                    "base_url": url,
                    "username": username,
                    "trust_invalid_cert": trust,
                }),
            )
        };
        let saved = source("https://music.example", "listener", false);
        assert!(saved.same_server(&source("https://music.example/", "listener", true)));
        assert!(!saved.same_server(&source("https://other.example", "listener", false)));
        assert!(!saved.same_server(&source("https://music.example", "someone", false)));
        // Case-sensitive on purpose: the edit dialog warns first, so a case change never wipes silently.
        assert!(!saved.same_server(&source("https://music.example", "Listener", false)));
    }

    #[test]
    fn editing_the_address_or_username_switches_the_server() {
        let saved = CredentialPreset {
            source_name: "Music".into(),
            server_url: "https://music.example".into(),
            username: "listener".into(),
            trust_invalid_cert: false,
            authentication: crate::SubsonicAuthentication::Password,
        };
        let input = |url: &str, username: &str| CredentialInput {
            source_name: Some("Renamed".into()),
            server_url: url.into(),
            username: username.into(),
            secret: "new secret".into(),
            trust_invalid_cert: true,
        };
        let password = crate::SubsonicAuthentication::Password;
        let api_key = crate::SubsonicAuthentication::ApiKey;
        assert!(!saved.switches_server(&input(" https://music.example/ ", "listener "), password));
        assert!(saved.switches_server(&input("https://proxy.example/music", "listener"), password));
        assert!(saved.switches_server(&input("https://music.example", "someone"), password));
        // The key decides the account, whatever the hidden username field still holds.
        assert!(saved.switches_server(&input("https://music.example", "listener"), api_key));
        let mut keep_key = input("https://music.example", "listener");
        keep_key.secret.clear();
        assert!(!saved.switches_server(&keep_key, api_key));
    }

    #[test]
    fn provider_payloads_decode_the_credential_free_saved_shape() {
        let subsonic_stored = migrated_source(
            SubsonicFlavor::Subsonic,
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
        let subsonic_payload = subsonic.into_payload();
        assert!(subsonic_payload.get("user_id").is_none());
        assert_eq!(
            decode_provider_payload::<serde_json::Value>(&encode_provider_payload(
                SubsonicFlavor::Subsonic,
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
            SubsonicFlavor::Subsonic,
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
