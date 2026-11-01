use gio::prelude::ApplicationExt;
use glib::prelude::*;
use playback::{PlaybackView, TransportStatus};
use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::rc::Rc;
use tracing::warn;

use crate::APP_ID;
use app_identity::DISPLAY_NAME;
const APP_NAME: &str = DISPLAY_NAME;
const DBUS_TIMEOUT_MSEC: i32 = 1_000;
const NOTIFICATIONS_BUS_NAME: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_INTERFACE: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const NOW_PLAYING_NOTIFICATION_ID: &str = "now-playing";
const NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC: i32 = -1;
const NOTIFICATION_ARTWORK_SIZE: u32 = 96;

fn notification_icon_path(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    notification_icon_bytes(&bytes)
}

fn notification_icon_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    artwork::square_thumbnail_png(bytes, NOTIFICATION_ARTWORK_SIZE).ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NativeNowPlayingNotification {
    title: String,
    body: String,
    artwork_uri: Option<String>,
}

pub struct Notifications {
    application: gio::Application,
    notification_id: Cell<u32>,
    notification_run: Cell<Option<playback::RunId>>,
    sendable: Cell<bool>,
}

impl Notifications {
    pub fn new(application: gio::Application) -> Rc<Self> {
        let notifications = Rc::new(Self {
            application,
            notification_id: Cell::new(0),
            notification_run: Cell::new(None),
            sendable: Cell::new(false),
        });
        notifications
    }

    pub fn observe(
        self: &Rc<Self>,
        player: Option<&PlaybackView>,
        enabled: bool,
        artwork_path: Option<PathBuf>,
        refresh_current: bool,
    ) {
        self.sendable
            .set(now_playing_notification_can_send(enabled, player));
        if now_playing_notification_should_withdraw(enabled, player) {
            self.withdraw();
            return;
        }
        if !self.sendable.get() {
            return;
        }
        let Some(player) = player else {
            return;
        };
        let Some(run) = player
            .transport
            .current
            .as_ref()
            .and_then(|media| media.id.run)
        else {
            return;
        };
        if !refresh_current && self.notification_run.get() == Some(run) {
            return;
        }
        self.notification_run.set(Some(run));
        let Some(entry) = player.transport.current.as_ref() else {
            return;
        };
        let title = entry.row.track.metadata.title.clone();
        let body = format!(
            "{} - {}",
            entry.row.track.metadata.artist, entry.row.track.metadata.album
        );
        let notifications = Rc::clone(self);
        glib::spawn_future_local(async move {
            if !notifications.matches(run) {
                return;
            }

            {
                let artwork_uri = artwork_path
                    .as_deref()
                    .and_then(now_playing_notification_artwork_uri);
                let native_notification = NativeNowPlayingNotification {
                    title: title.clone(),
                    body: body.clone(),
                    artwork_uri,
                };
                let replaces_id = notifications.notification_id.get();
                match send_freedesktop_now_playing_notification(&native_notification, replaces_id)
                    .await
                {
                    Ok(notification_id) => {
                        if notifications.matches(run) {
                            notifications
                                .application
                                .withdraw_notification(NOW_PLAYING_NOTIFICATION_ID);
                            notifications.notification_id.set(notification_id);
                        } else {
                            close_freedesktop_now_playing_notification(notification_id).await;
                        }
                    }
                    Err(error) => {
                        warn!(%error, "failed to send Freedesktop now-playing notification");
                        if replaces_id != 0 {
                            notifications.notification_id.set(0);
                            close_freedesktop_now_playing_notification(replaces_id).await;
                        }
                        if notifications.matches(run) {
                            send_gio_now_playing_notification(
                                &notifications.application,
                                title,
                                body,
                                artwork_path,
                            )
                            .await;
                        }
                    }
                }
            }
        });
    }

    pub fn withdraw(&self) {
        self.application
            .withdraw_notification(NOW_PLAYING_NOTIFICATION_ID);
        self.notification_run.set(None);
        self.sendable.set(false);
        {
            let notification_id = self.notification_id.replace(0);
            if notification_id != 0 {
                glib::spawn_future_local(async move {
                    close_freedesktop_now_playing_notification(notification_id).await;
                });
            }
        }
    }

    fn matches(&self, run: playback::RunId) -> bool {
        self.sendable.get() && self.notification_run.get() == Some(run)
    }
}

fn now_playing_notification_artwork_uri(path: &Path) -> Option<String> {
    glib::filename_to_uri(path, None)
        .ok()
        .map(|uri| uri.to_string())
}

fn now_playing_notification_hints(artwork_uri: Option<&str>) -> glib::VariantDict {
    let hints = glib::VariantDict::new(None);
    hints.insert("desktop-entry", APP_ID);
    hints.insert("transient", true);
    if let Some(uri) = artwork_uri {
        hints.insert("image-path", uri);
        hints.insert("image_path", uri);
    }
    hints
}

fn now_playing_notification_parameters(
    notification: &NativeNowPlayingNotification,
    replaces_id: u32,
) -> glib::Variant {
    (
        APP_NAME.to_string(),
        replaces_id,
        APP_ID.to_string(),
        notification.title.clone(),
        notification.body.clone(),
        Vec::<String>::new(),
        now_playing_notification_hints(notification.artwork_uri.as_deref()),
        NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC,
    )
        .to_variant()
}

async fn send_freedesktop_now_playing_notification(
    notification: &NativeNowPlayingNotification,
    replaces_id: u32,
) -> Result<u32, glib::Error> {
    let connection = gio::bus_get_future(gio::BusType::Session).await?;
    let parameters = now_playing_notification_parameters(notification, replaces_id);
    let reply_type = glib::VariantTy::new("(u)").ok();
    let reply = connection
        .call_future(
            Some(NOTIFICATIONS_BUS_NAME),
            NOTIFICATIONS_OBJECT_PATH,
            NOTIFICATIONS_INTERFACE,
            "Notify",
            Some(&parameters),
            reply_type,
            gio::DBusCallFlags::NONE,
            DBUS_TIMEOUT_MSEC,
        )
        .await?;
    Ok(reply.try_child_get::<u32>(0).ok().flatten().unwrap_or(0))
}

async fn close_freedesktop_now_playing_notification(notification_id: u32) {
    if notification_id == 0 {
        return;
    }
    let Ok(connection) = gio::bus_get_future(gio::BusType::Session).await else {
        return;
    };
    let parameters = (notification_id,).to_variant();
    let _closed = connection
        .call_future(
            Some(NOTIFICATIONS_BUS_NAME),
            NOTIFICATIONS_OBJECT_PATH,
            NOTIFICATIONS_INTERFACE,
            "CloseNotification",
            Some(&parameters),
            None,
            gio::DBusCallFlags::NONE,
            DBUS_TIMEOUT_MSEC,
        )
        .await;
}

async fn send_gio_now_playing_notification(
    application: &gio::Application,
    title: String,
    body: String,
    artwork_path: Option<PathBuf>,
) {
    let icon_bytes = match artwork_path {
        Some(path) => gio::spawn_blocking(move || notification_icon_path(&path))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let notification = gio::Notification::new(&title);
    notification.set_body(Some(&body));
    if let Some(bytes) = icon_bytes {
        let bytes = glib::Bytes::from_owned(bytes);
        notification.set_icon(&gio::BytesIcon::new(&bytes));
    }
    application.send_notification(Some(NOW_PLAYING_NOTIFICATION_ID), &notification);
}

pub fn now_playing_notification_can_send(enabled: bool, player: Option<&PlaybackView>) -> bool {
    enabled
        && player.is_some_and(|player| {
            matches!(
                player.transport.state,
                TransportStatus::Playing | TransportStatus::Buffering
            ) && player.transport.current.is_some()
        })
}

pub fn now_playing_notification_should_withdraw(
    enabled: bool,
    player: Option<&PlaybackView>,
) -> bool {
    !enabled
        || player.is_none_or(|player| {
            player.transport.current.is_none() || player.transport.state == TransportStatus::Stopped
        })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        APP_ID, APP_NAME, NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC, NativeNowPlayingNotification,
        now_playing_notification_artwork_uri, now_playing_notification_hints,
        now_playing_notification_parameters,
    };
    use super::{NOTIFICATION_ARTWORK_SIZE, notification_icon_bytes};

    #[test]
    fn notification_artwork_is_square_and_thumbnail_sized() {
        let cover = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../resources/icons/hicolor/512x512/apps/io.github.kmarius.slopsonic.png"
        ));
        let bytes = notification_icon_bytes(cover).expect("notification bytes");
        let icon = artwork::decode_rgba(&bytes, u32::MAX).expect("notification image");

        assert_eq!(icon.width(), NOTIFICATION_ARTWORK_SIZE);
        assert_eq!(icon.height(), NOTIFICATION_ARTWORK_SIZE);
    }

    #[test]
    fn freedesktop_notification_hints_include_identity_and_artwork() {
        let hints = now_playing_notification_hints(Some("file:///music/cover.png"));

        assert_eq!(
            hints.lookup::<String>("desktop-entry").unwrap().as_deref(),
            Some(APP_ID)
        );
        assert_eq!(hints.lookup::<bool>("transient").unwrap(), Some(true));
        assert_eq!(
            hints.lookup::<String>("image-path").unwrap().as_deref(),
            Some("file:///music/cover.png")
        );
        assert_eq!(
            hints.lookup::<String>("image_path").unwrap().as_deref(),
            Some("file:///music/cover.png")
        );
    }

    #[test]
    fn freedesktop_notification_parameters_replace_the_previous_notification() {
        let notification = NativeNowPlayingNotification {
            title: "Track".to_string(),
            body: "Artist - Album".to_string(),
            artwork_uri: None,
        };
        let parameters = now_playing_notification_parameters(&notification, 41);

        assert_eq!(
            parameters.try_child_get::<String>(0).unwrap().as_deref(),
            Some(APP_NAME)
        );
        assert_eq!(parameters.try_child_get::<u32>(1).unwrap(), Some(41));
        assert_eq!(
            parameters.try_child_get::<String>(3).unwrap().as_deref(),
            Some("Track")
        );
        assert_eq!(
            parameters.try_child_get::<String>(4).unwrap().as_deref(),
            Some("Artist - Album")
        );
        assert_eq!(
            parameters.try_child_get::<i32>(7).unwrap(),
            Some(NOW_PLAYING_NOTIFICATION_TIMEOUT_MSEC)
        );
    }

    #[test]
    fn notification_artwork_path_becomes_a_file_uri() {
        let uri = now_playing_notification_artwork_uri(Path::new("/tmp/cover art.png"))
            .expect("absolute artwork path");

        assert_eq!(uri, "file:///tmp/cover%20art.png");
    }
}
