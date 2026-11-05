//! Native desktop services and playback integrations.
//!
//! Each integration keeps only the state required by its external protocol.
//! GTK window policy and Rufin's current-media authority remain with their
//! existing owners.

mod folders;
mod media_controls;
mod path_display;
#[cfg(test)]
mod test_support;

pub use folders::folder_target;
pub use media_controls::MediaControls;
pub use path_display::display_path;
