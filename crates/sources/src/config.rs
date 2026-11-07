use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{SourceError, SourceResult, subsonic::SubsonicFlavor};
use library::SourceId;

/// Credential-free source configuration persisted by Slopsonic Settings.
///
/// The provider payload is opaque outside Sources. Cache health never affects
/// this value or its separately stored credential reference.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceConfiguration {
    pub source_id: SourceId,
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
    pub kind: SubsonicFlavor,
    pub authentication: crate::SubsonicAuthentication,
    pub credentials: CredentialInput,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct CredentialPreset {
    pub source_name: String,
    pub server_url: String,
    pub username: String,
    pub trust_invalid_cert: bool,
    pub open_subsonic_authentication: Option<crate::SubsonicAuthentication>,
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
            open_subsonic_authentication: Some(config.authentication),
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

pub(crate) fn encode_provider_payload(
    source_id: SourceId,
    kind: SubsonicFlavor,
    name: impl Into<String>,
    provider_payload: serde_json::Value,
) -> SourceConfiguration {
    SourceConfiguration {
        source_id,
        kind,
        name: name.into(),
        provider_payload: provider_payload.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::SourceId;

    use super::*;
    use crate::subsonic::SubsonicSourceConfig;

    fn migrated_source(kind: SubsonicFlavor, payload: serde_json::Value) -> SourceConfiguration {
        SourceConfiguration {
            source_id: SourceId::new(format!("{}:server:test", kind.source_id())),
            kind,
            name: "Test Source".to_string(),
            provider_payload: payload.to_string(),
        }
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
        let subsonic_payload = subsonic.clone().into_payload();
        assert!(subsonic_payload.get("user_id").is_none());
        assert_eq!(
            decode_provider_payload::<serde_json::Value>(&encode_provider_payload(
                subsonic_stored.source_id,
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
