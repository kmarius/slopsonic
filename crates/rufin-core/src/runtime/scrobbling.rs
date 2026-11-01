use std::sync::Arc;

use async_channel::Sender;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LastFmPreferences {
    pub enabled: bool,
    pub api_key: String,
    pub api_secret: String,
    pub username: String,
    pub connected: bool,
    pub now_playing_enabled: bool,
}

impl Default for LastFmPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            api_key: String::new(),
            api_secret: String::new(),
            username: String::new(),
            connected: false,
            now_playing_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibreFmPreferences {
    pub enabled: bool,
    pub username: String,
    pub connected: bool,
    pub now_playing_enabled: bool,
}

impl Default for LibreFmPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            username: String::new(),
            connected: false,
            now_playing_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenBrainzPreferences {
    pub enabled: bool,
    pub username: String,
    pub user_token: String,
    pub now_playing_enabled: bool,
}

impl Default for ListenBrainzPreferences {
    fn default() -> Self {
        Self {
            enabled: false,
            username: String::new(),
            user_token: String::new(),
            now_playing_enabled: true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScrobblingPreferences {
    pub lastfm: LastFmPreferences,
    pub librefm: LibreFmPreferences,
    pub listenbrainz: ListenBrainzPreferences,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScrobblingConnection {
    LastFm { api_key: String, api_secret: String },
    LibreFm,
}

pub enum ScrobblingConnectionEvent {
    OpenUrl {
        url: String,
        opened: Arc<BrowserOpening>,
    },
    Connected {
        username: String,
    },
    TimedOut,
    Failed(String),
}

pub type ScrobblingHandle = Arc<crate::scrobbling::ScrobblingOwner>;

pub struct BrowserOpening(pub(crate) Sender<Result<(), String>>);
impl BrowserOpening {
    pub async fn complete(&self, failure: Option<String>) {
        let _ = self.0.send(failure.map_or(Ok(()), Err)).await;
    }
}
pub struct ScrobblingSession(pub(crate) async_channel::Receiver<ScrobblingConnectionEvent>);
impl ScrobblingSession {
    pub async fn next(&self) -> Result<ScrobblingConnectionEvent, crate::settings::SettingsError> {
        self.0
            .recv()
            .await
            .map_err(|error| error.to_string().into())
    }
}
