//! Native desktop services and playback integrations.
//!
//! Each integration keeps only the state required by its external protocol.
//! GTK window policy and Rufin's current-media authority remain with their
//! existing owners.

mod folders;
mod media_controls;
mod notification;
mod path_display;
#[cfg(test)]
mod test_support;
mod tray;

#[cfg(unix)]
pub(crate) use app_identity::APP_ID;

pub use folders::folder_target;
pub use media_controls::MediaControls;
pub use notification::{
    Notifications, now_playing_notification_can_send, now_playing_notification_should_withdraw,
};
pub use path_display::display_path;
pub use tray::{Tray, TrayIntent};
