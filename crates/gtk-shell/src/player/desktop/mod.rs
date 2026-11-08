pub(crate) mod lifecycle;

use std::rc::Rc;
use std::sync::mpsc::{TryRecvError, channel};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use playback::{CurrentMedia, PositionDiscontinuity};
use tracing::{info, warn};

use crate::shell::Shell;
use gtk_widgets::artwork::THUMB_COVER_SIZE;

const QUIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct DesktopState {
    pub(crate) media_controls: Rc<desktop_integration::MediaControls>,
}

impl DesktopState {
    pub(crate) fn new(transport: std::sync::Arc<slopsonic_core::playback::PlaybackOwner>) -> Self {
        Self {
            media_controls: desktop_integration::MediaControls::start(transport),
        }
    }
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
        let (completed, completion) = channel();
        if let Err(error) = std::thread::Builder::new()
            .name("slopsonic-shutdown".to_string())
            .spawn(move || {
                transport.shutdown();
                scrobbling.shutdown();
                let _ = completed.send(settings.flush());
            })
        {
            warn!(%error, "could not start playback shutdown");
            self.chrome.application.quit();
            return;
        }

        let application = self.chrome.application.clone();
        let started_at = Instant::now();
        glib::timeout_add_local(QUIT_POLL_INTERVAL, move || match completion.try_recv() {
            Ok(result) => {
                if let Err(error) = result {
                    warn!(%error, "could not save settings");
                }
                info!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown finished"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Disconnected) => {
                warn!(
                    elapsed_ms = started_at.elapsed().as_millis(),
                    "playback shutdown worker stopped before reporting completion"
                );
                application.quit();
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) if started_at.elapsed() >= QUIT_TIMEOUT => {
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
}

pub(crate) fn install_desktop_lifecycle(shell: &Rc<Shell>) {
    let close_shell = Rc::clone(shell);
    shell.chrome.window.connect_close_request(move |_| {
        close_shell.request_quit("window close");
        glib::Propagation::Stop
    });
}
