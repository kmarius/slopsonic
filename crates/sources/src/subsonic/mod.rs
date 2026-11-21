use crate::config::{decode_provider_payload, require_payload_version};
use crate::policy::{raw_item_id, u16_from_option};
use crate::{
    ConnectedSource, CredentialInput, ImageBytes, SourceConfiguration, SourceEditResult,
    SourceError, SourceResult,
};
use audio_processing::ResolvedStream;
use playback::{SourceReportFact, SourceReportPhase, StreamQuality};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::instrument;

mod client;
mod item;
mod navidrome;
mod refresh;

use crate::remote_json as json;
use client::*;
use item::*;

use crate::NativeImageRef as ImageRef;

#[derive(Clone, Debug, PartialEq)]
struct Playlist {
    header: library::PlaylistObservation,
    revision: Option<String>,
    valid_until: Option<i64>,
}
#[derive(Clone, Debug, PartialEq)]
struct PlaylistSnapshot {
    playlist: Playlist,
    entries: Vec<library::PlaylistEntryObservation>,
}

fn normalize_release_types(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        let value = value.trim().to_lowercase();
        if !value.is_empty() && !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

const SOURCE_CONFIG_VERSION: u32 = 1;
const NAVIDROME_LIBRARY_VERSION: u32 = 1;

#[derive(Deserialize)]
struct SubsonicSourcePayload {
    version: u32,
    base_url: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    username: String,
    trust_invalid_cert: bool,
    #[serde(default)]
    navidrome_library_version: u32,
    #[serde(default)]
    authentication: SubsonicAuthentication,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsonicAuthentication {
    #[default]
    Password,
    LegacyPassword,
    ApiKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubsonicSourceConfig {
    pub(crate) base_url: String,
    pub(crate) username: String,
    pub(crate) trust_invalid_cert: bool,
    pub(crate) navidrome_library_version: u32,
    pub(crate) authentication: SubsonicAuthentication,
}

impl SubsonicSourceConfig {
    pub fn from_configuration(stored: &crate::SourceConfiguration) -> SourceResult<Self> {
        let payload: SubsonicSourcePayload = decode_provider_payload(stored)?;
        require_payload_version(payload.version, SOURCE_CONFIG_VERSION)?;
        let username = if payload.username.trim().is_empty() {
            payload.user_id
        } else {
            payload.username
        };
        if username.trim().is_empty() {
            return Err(SourceError::InvalidConfig(
                "the saved OpenSubsonic username is missing".to_string(),
            ));
        }
        Ok(Self {
            base_url: payload.base_url,
            username,
            trust_invalid_cert: payload.trust_invalid_cert,
            navidrome_library_version: payload.navidrome_library_version,
            authentication: payload.authentication,
        })
    }

    pub(crate) fn into_payload(self) -> serde_json::Value {
        serde_json::json!({
            "version": SOURCE_CONFIG_VERSION,
            "base_url": self.base_url,
            "username": self.username,
            "trust_invalid_cert": self.trust_invalid_cert,
            "navidrome_library_version": self.navidrome_library_version,
            "authentication": self.authentication,
        })
    }
}

struct AuthenticatedSubsonic {
    configuration: SourceConfiguration,
    source: SubsonicSource,
    credential: String,
}

impl AuthenticatedSubsonic {
    fn connected(self) -> ConnectedSource {
        ConnectedSource::subsonic(self.configuration, self.source, Some(self.credential))
    }
}

pub(crate) async fn connect(
    authentication: SubsonicAuthentication,
    credentials: CredentialInput,
) -> SourceResult<ConnectedSource> {
    SubsonicSource::authenticate(authentication, credentials)
        .await
        .map(AuthenticatedSubsonic::connected)
}

pub(crate) fn open(
    configuration: &SourceConfiguration,
    credential: Option<String>,
) -> SourceResult<SubsonicSource> {
    let config = SubsonicSourceConfig::from_configuration(configuration)?;
    let credential = credential.ok_or_else(|| {
        SourceError::InvalidConfig("saved OpenSubsonic credentials are missing".to_string())
    })?;
    SubsonicSource::open(configuration.kind, config, credential)
}

pub(crate) async fn edit(
    current: SourceConfiguration,
    current_credential: Option<String>,
    authentication: SubsonicAuthentication,
    mut credentials: CredentialInput,
) -> SourceResult<SourceEditResult> {
    let flavor = current.kind;
    let saved = SubsonicSourceConfig::from_configuration(&current)?;
    let name = crate::source::edited_source_name(
        credentials.source_name.as_deref().unwrap_or_default(),
        &current.name,
    );
    let address_changed = crate::source::comparable_address(&credentials.server_url)
        != crate::source::comparable_address(&saved.base_url);
    let username_changed = credentials.username.trim() != saved.username;
    let has_password = !credentials.secret.is_empty();
    let authentication_changed = authentication != saved.authentication;

    // A saved credential is only ever sent back to the host it was issued for.
    if (address_changed || username_changed || authentication_changed) && !has_password {
        return Err(SourceError::Other(
            "Enter the server password or API key to save address or authentication changes."
                .to_string(),
        ));
    }

    if has_password {
        credentials.source_name = Some(name);
        let authenticated = SubsonicSource::authenticate(authentication, credentials).await?;
        return Ok(SourceEditResult::Connected(Box::new(
            authenticated.connected(),
        )));
    }

    let reopen = address_changed || credentials.trust_invalid_cert != saved.trust_invalid_cert;
    let configuration = crate::config::encode_provider_payload(
        flavor,
        name,
        SubsonicSourceConfig {
            base_url: normalize_base_url(&credentials.server_url)?
                .as_str()
                .trim_end_matches('/')
                .to_owned(),
            username: saved.username,
            trust_invalid_cert: credentials.trust_invalid_cert,
            navidrome_library_version: saved.navidrome_library_version,
            authentication: saved.authentication,
        }
        .into_payload(),
    );
    if configuration == current {
        return Ok(SourceEditResult::Unchanged);
    }
    if !reopen {
        return Ok(SourceEditResult::ConfigurationOnly(configuration));
    }
    let source = open(&configuration, current_credential)?;
    Ok(SourceEditResult::Connected(Box::new(
        ConnectedSource::subsonic(configuration, source, None),
    )))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsonicFlavor {
    Navidrome,
    Subsonic,
}
impl SubsonicFlavor {
    /// The stable name stored as the cached server's kind.
    pub fn kind_name(self) -> &'static str {
        match self {
            Self::Navidrome => "navidrome",
            Self::Subsonic => "subsonic",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Navidrome => "Navidrome",
            Self::Subsonic => "OpenSubsonic",
        }
    }
}
#[derive(Debug)]
pub struct SubsonicSource {
    client: Client,
    navidrome_client: Client,
    base_url: Url,
    username: String,
    credential: Arc<SubsonicCredential>,
    flavor: SubsonicFlavor,
    navidrome_library: bool,
    navidrome_session: navidrome::NavidromeSession,
    trust_invalid_cert: bool,
    extensions: tokio::sync::OnceCell<serde_json::Value>,
}
impl SubsonicSource {
    fn open(
        flavor: SubsonicFlavor,
        config: SubsonicSourceConfig,
        credential: String,
    ) -> SourceResult<Self> {
        let base_url = normalize_base_url(&config.base_url)?;
        let client = build_client(config.trust_invalid_cert)?;
        let navidrome_client = navidrome::build_client(config.trust_invalid_cert)?;
        let credential = SubsonicCredential::parse(&credential)?;
        let navidrome_library = config.navidrome_library_version > 0;
        if config.navidrome_library_version > NAVIDROME_LIBRARY_VERSION {
            return Err(SourceError::InvalidConfig(format!(
                "Navidrome library version {} is not supported.",
                config.navidrome_library_version
            )));
        }
        if config.navidrome_library_version > 0
            && (flavor != SubsonicFlavor::Navidrome || credential.navidrome_password().is_none())
        {
            return Err(SourceError::InvalidConfig(
                "Saved Navidrome access needs the server password.".to_string(),
            ));
        }
        if credential.authentication() != config.authentication {
            return Err(SourceError::InvalidConfig(
                "Saved OpenSubsonic authentication does not match its credential.".to_string(),
            ));
        }
        Ok(Self {
            client,
            navidrome_client,
            base_url,
            username: config.username,
            credential: Arc::new(credential),
            flavor,
            navidrome_library,
            navidrome_session: navidrome::NavidromeSession::default(),
            trust_invalid_cert: config.trust_invalid_cert,
            extensions: tokio::sync::OnceCell::new(),
        })
    }

    #[instrument(skip(credentials), fields(base_url = %credentials.server_url, username = %credentials.username, trust_invalid_cert = credentials.trust_invalid_cert))]
    async fn authenticate(
        authentication: SubsonicAuthentication,
        credentials: CredentialInput,
    ) -> SourceResult<AuthenticatedSubsonic> {
        let CredentialInput {
            source_name: submitted_name,
            server_url,
            username,
            secret: password,
            trust_invalid_cert,
        } = credentials;
        let base_url = normalize_base_url(&server_url)?;
        let client = build_client(trust_invalid_cert)?;
        let mut credential = match authentication {
            SubsonicAuthentication::Password => {
                SubsonicCredential::from_navidrome_password(&password)
            }
            SubsonicAuthentication::LegacyPassword => SubsonicCredential::LegacyPassword(password),
            SubsonicAuthentication::ApiKey => SubsonicCredential::from_api_key(&password)?,
        };
        let (canonical_username, provider_name) = match authentication {
            SubsonicAuthentication::Password | SubsonicAuthentication::LegacyPassword => {
                let mut ping_url = endpoint(&base_url, "ping")?;
                ping_url
                    .query_pairs_mut()
                    .extend_pairs(credential.common_query(&username, &[]));
                let response = subsonic_json(client.get(ping_url)).await?;
                let observed = async {
                    let mut user_url = endpoint(&base_url, "getUser")?;
                    user_url.query_pairs_mut().extend_pairs(
                        credential.common_query(&username, &[("username", username.as_str())]),
                    );
                    subsonic_json(client.get(user_url)).await
                }
                .await
                .ok();
                let canonical = observed
                    .as_ref()
                    .and_then(|value| value.body["user"]["username"].as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(username.trim())
                    .to_string();
                let name = observed
                    .and_then(|value| value.server_type)
                    .or(response.server_type)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_default();
                (canonical, name)
            }
            SubsonicAuthentication::ApiKey => {
                let mut token_url = endpoint(&base_url, "tokenInfo")?;
                token_url
                    .query_pairs_mut()
                    .extend_pairs(credential.common_query("", &[]));
                let response = subsonic_json(client.get(token_url)).await?;
                let canonical = json::field::<String>(&response.body["tokenInfo"], "username")
                    .unwrap_or_default();
                let name = response
                    .server_type
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_default();
                (canonical, name)
            }
        };
        // Navidrome's native API needs the plain password, so only password logins use it.
        let flavor = if provider_name.trim().eq_ignore_ascii_case("navidrome")
            && authentication == SubsonicAuthentication::Password
        {
            SubsonicFlavor::Navidrome
        } else {
            if let SubsonicCredential::Token {
                navidrome_password, ..
            } = &mut credential
            {
                *navidrome_password = None;
            }
            SubsonicFlavor::Subsonic
        };
        let provider_name = if provider_name.is_empty() {
            flavor.display_name().to_string()
        } else {
            provider_name
        };
        if canonical_username.trim().is_empty() {
            return Err(SourceError::Auth(
                "OpenSubsonic returned an empty canonical username".to_string(),
            ));
        }
        let serialized_credential = credential.serialize();
        let configuration = crate::config::encode_provider_payload(
            flavor,
            crate::source::configured_source_name(submitted_name, provider_name),
            SubsonicSourceConfig {
                base_url: base_url.as_str().trim_end_matches('/').to_string(),
                username: canonical_username.clone(),
                trust_invalid_cert,
                navidrome_library_version: if flavor == SubsonicFlavor::Navidrome {
                    NAVIDROME_LIBRARY_VERSION
                } else {
                    0
                },
                authentication,
            }
            .into_payload(),
        );
        let source = Self {
            navidrome_client: navidrome::build_client(trust_invalid_cert)?,
            client,
            base_url,
            username: canonical_username,
            credential: Arc::new(credential),
            flavor,
            navidrome_library: flavor == SubsonicFlavor::Navidrome,
            navidrome_session: navidrome::NavidromeSession::default(),
            trust_invalid_cert,
            extensions: tokio::sync::OnceCell::new(),
        };
        Ok(AuthenticatedSubsonic {
            configuration,
            source,
            credential: serialized_credential,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{SubsonicAuthentication, SubsonicCredential, SubsonicFlavor, SubsonicSourceConfig};
    use crate::{CredentialInput, SourceEditResult};

    fn saved() -> crate::SourceConfiguration {
        crate::config::encode_provider_payload(
            SubsonicFlavor::Subsonic,
            "Music".to_string(),
            SubsonicSourceConfig {
                base_url: "https://music.example.com".to_string(),
                username: "listener".to_string(),
                trust_invalid_cert: false,
                navidrome_library_version: 0,
                authentication: SubsonicAuthentication::Password,
            }
            .into_payload(),
        )
    }

    async fn edit_without_secret(
        server_url: &str,
        trust_invalid_cert: bool,
    ) -> crate::SourceResult<SourceEditResult> {
        super::edit(
            saved(),
            Some(SubsonicCredential::from_password("password").serialize()),
            SubsonicAuthentication::Password,
            CredentialInput {
                source_name: None,
                server_url: server_url.to_string(),
                username: "listener".to_string(),
                secret: String::new(),
                trust_invalid_cert,
            },
        )
        .await
    }

    #[tokio::test]
    async fn address_change_without_secret_does_not_reuse_saved_credential() {
        assert!(
            edit_without_secret("https://other.example.com", false)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn certificate_trust_change_reopens_with_saved_credential() {
        assert!(matches!(
            edit_without_secret("https://music.example.com", true).await,
            Ok(SourceEditResult::Connected(_))
        ));
    }
}
