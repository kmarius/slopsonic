pub(crate) mod lifecycle;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{TryRecvError, channel};
use std::time::{Duration, Instant};

use adw::prelude::*;
use ashpd::desktop::background::Background;
use gtk::glib;
use playback::{CurrentMedia, PlaybackView, PositionDiscontinuity};
use tracing::{info, warn};

use crate::Settings as UiSettings;
use crate::shell::Shell;
use gtk_widgets::artwork::THUMB_COVER_SIZE;

const QUIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackgroundAccess {
    Unavailable,
    Pending,
    Granted,
    Denied,
}

pub(crate) struct DesktopState {
    pub(crate) media_controls: Rc<desktop_integration::MediaControls>,
    pub(crate) notifications: Rc<desktop_integration::Notifications>,
    background_access: Cell<BackgroundAccess>,
}

impl DesktopState {
    pub(crate) fn new(
        application: &adw::Application,
        transport: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    ) -> Self {
        let weak = application.downgrade();
        Self {
            media_controls: desktop_integration::MediaControls::start(transport, move |uri| {
                if let Some(application) = weak.upgrade() {
                    application.open(&[gtk::gio::File::for_uri(uri)], "");
                }
            }),
            notifications: desktop_integration::Notifications::new(application.clone().upcast()),
            background_access: Cell::new(BackgroundAccess::Unavailable),
        }
    }
}

pub(crate) fn now_playing_notification_can_send(
    settings: &UiSettings,
    player: Option<&PlaybackView>,
) -> bool {
    desktop_integration::now_playing_notification_can_send(settings.allows_notifications(), player)
}

pub(crate) fn now_playing_notification_should_withdraw(
    settings: &UiSettings,
    player: Option<&PlaybackView>,
) -> bool {
    desktop_integration::now_playing_notification_should_withdraw(
        settings.allows_notifications(),
        player,
    )
}

impl Shell {
    pub(crate) fn request_quit(self: &Rc<Self>, reason: &'static str) {
        if self.quitting.replace(true) {
            return;
        }
        info!(reason, "stopping Slopsonic");
        self.save_window_state();
        self.chrome.window.set_visible(false);

        let transport = self.products.playback.clone();
        let scrobbling = self.products.scrobbling.clone();
        let settings = self.products.settings.clone();
        let storage_reset = Rc::clone(&self.storage_reset);
        let resetting = storage_reset.borrow().is_some();
        let products = resetting.then(|| self.products.clone());
        let (completed, completion) = channel();
        if let Err(error) = std::thread::Builder::new()
            .name("rufin-shutdown".to_string())
            .spawn(move || {
                transport.shutdown();
                scrobbling.shutdown();
                let result = match products {
                    Some(products) => products.runtime.block_on(products.close_storage()),
                    None => settings.flush(),
                };
                let _ = completed.send(result);
            })
        {
            if resetting {
                storage_reset.replace(Some(Err(error.to_string())));
            }
            warn!(%error, "could not start playback shutdown");
            self.chrome.application.quit();
            return;
        }

        let application = self.chrome.application.clone();
        let started_at = Instant::now();
        glib::timeout_add_local(QUIT_POLL_INTERVAL, move || match completion.try_recv() {
            Ok(result) => {
                if let Err(error) = result {
                    warn!(%error, "could not finish saving settings and storage");
                    if resetting {
                        storage_reset.replace(Some(Err(error)));
                    }
                }
                info!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown finished"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                if resetting {
                    storage_reset.replace(Some(Err(
                        "Slopsonic could not finish closing its storage".into(),
                    )));
                }
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown worker stopped before reporting completion"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) if !resetting && started_at.elapsed() >= QUIT_TIMEOUT => {
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown did not finish before the quit deadline"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
        });
    }

    pub(crate) fn notify_now_playing(self: &Rc<Self>, player: Option<&PlaybackView>) {
        self.observe_now_playing_notification(player, false);
    }

    pub(crate) fn refresh_now_playing_notification(self: &Rc<Self>, player: Option<&PlaybackView>) {
        self.observe_now_playing_notification(player, true);
    }

    fn observe_now_playing_notification(
        self: &Rc<Self>,
        player: Option<&PlaybackView>,
        refresh_current: bool,
    ) {
        let artwork = player.and_then(|player| {
            player.transport.current.as_ref().and_then(|media| {
                self.artwork
                    .current_playback_cached_artwork_path(media, THUMB_COVER_SIZE)
                    .map(|artwork| artwork.path)
            })
        });
        self.desktop.notifications.observe(
            player,
            self.settings.current.borrow().allows_notifications(),
            artwork,
            refresh_current,
        );
    }

    pub(crate) fn withdraw_now_playing_notification(&self) {
        self.desktop.notifications.withdraw();
    }

    pub(crate) fn update_media_controls(&self) {
        self.update_media_controls_after(None);
    }

    pub(crate) fn update_media_controls_after(&self, discontinuity: Option<PositionDiscontinuity>) {
        let playback = self.player_ui.selected_playback();
        let art_url = playback.as_ref().and_then(|playback| {
            playback
                .transport
                .current
                .as_ref()
                .and_then(|media| self.current_art_url(media))
        });
        self.desktop
            .media_controls
            .observe(playback.as_deref(), art_url, discontinuity);
    }

    pub(crate) fn update_media_controls_position_after(
        &self,
        position_millis: Option<u64>,
        discontinuity: Option<PositionDiscontinuity>,
    ) {
        self.desktop
            .media_controls
            .observe_position(position_millis, discontinuity);
    }

    fn current_art_url(&self, media: &CurrentMedia) -> Option<String> {
        let artwork = self
            .artwork
            .current_playback_cached_artwork_path(media, THUMB_COVER_SIZE)?;
        glib::filename_to_uri(artwork.path, None)
            .ok()
            .map(|uri| uri.to_string())
    }

    pub(crate) fn set_keep_running_after_close(self: &Rc<Self>, enabled: bool) {
        if self
            .settings
            .update_app_settings("keep running after close setting", |settings| {
                if settings.keep_running_after_close == enabled {
                    return false;
                }
                settings.keep_running_after_close = enabled;
                true
            })
            .is_none()
        {
            return;
        }
        if enabled {
            self.request_background_access();
        }
    }

    fn request_background_access(self: &Rc<Self>) {
        let settings = self.settings.current.borrow();
        if !ashpd::is_sandboxed()
            || !settings.keep_running_after_close
            || matches!(
                self.desktop.background_access.get(),
                BackgroundAccess::Pending | BackgroundAccess::Granted
            )
        {
            return;
        }
        self.desktop
            .background_access
            .set(BackgroundAccess::Pending);
        let shell = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let reason = "Keep Slopsonic running after closing the window".to_string();
            let access = match Background::request()
                .reason(reason.as_str())
                .auto_start(false)
                .send()
                .await
                .and_then(|request| request.response())
            {
                Ok(response) if response.run_in_background() => {
                    info!("background access granted");
                    BackgroundAccess::Granted
                }
                Ok(_) => {
                    warn!("background access was not granted");
                    BackgroundAccess::Denied
                }
                Err(error) => {
                    warn!(%error, "could not request background access");
                    BackgroundAccess::Denied
                }
            };
            let Some(shell) = shell.upgrade() else {
                return;
            };
            shell.desktop.background_access.set(access);
            if access == BackgroundAccess::Denied
                && shell.settings.current.borrow().keep_running_after_close
                && !shell.chrome.window.is_visible()
            {
                shell.request_quit("background access denied");
            }
        });
    }

    fn background_access_available(&self) -> bool {
        matches!(
            self.desktop.background_access.get(),
            BackgroundAccess::Pending | BackgroundAccess::Granted
        )
    }
}

pub(crate) fn install_desktop_lifecycle(shell: &Rc<Shell>) {
    let settings = shell.settings.current.borrow().clone();
    if settings.keep_running_after_close {
        shell.request_background_access();
    }
    let close_shell = Rc::clone(shell);
    shell
        .chrome
        .window
        .connect_close_request(move |_| close_window(&close_shell));
}

fn close_window(shell: &Rc<Shell>) -> glib::Propagation {
    let settings = shell.settings.current.borrow().clone();
    if settings.keep_running_after_close {
        shell.request_background_access();
    }
    if close_should_hide(
        settings.keep_running_after_close,
        host_is_sandboxed(),
        shell.background_access_available(),
    ) {
        shell.save_window_state();
        shell.chrome.window.set_visible(false);
    } else {
        shell.request_quit("window close");
    }
    glib::Propagation::Stop
}

fn close_should_hide(
    keep_running: bool,
    sandboxed: bool,
    background_access_available: bool,
) -> bool {
    if !keep_running {
        return false;
    }
    !sandboxed || background_access_available
}

fn host_is_sandboxed() -> bool {
    ashpd::is_sandboxed()
}

#[cfg(test)]
mod tests {
    use super::close_should_hide;

    #[test]
    fn close_setting_off_never_hides() {
        assert!(!close_should_hide(false, false, true));
    }

    #[test]
    fn background_running_needs_native_or_portal_support() {
        assert!(close_should_hide(true, false, false));
        assert!(close_should_hide(true, true, true));
        assert!(!close_should_hide(true, true, false));
    }
}
