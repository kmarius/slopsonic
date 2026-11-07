//! Native desktop services and playback integrations.
//!
//! Each integration keeps only the state required by its external protocol.
//! GTK window policy and Slopsonic's current-media authority remain with their
//! existing owners.

mod media_controls;
#[cfg(test)]
mod test_support;

pub use media_controls::MediaControls;
