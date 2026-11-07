//! Slopsonic's application identity shared by runtime and platform integrations.

pub const STABLE_APP_ID: &str = "io.github.kmarius.slopsonic";

#[cfg(feature = "development")]
pub const APP_ID: &str = "io.github.kmarius.slopsonic.Devel";
#[cfg(not(feature = "development"))]
pub const APP_ID: &str = STABLE_APP_ID;

#[cfg(feature = "development")]
pub const DISPLAY_NAME: &str = "Slopsonic (Development)";
#[cfg(not(feature = "development"))]
pub const DISPLAY_NAME: &str = "Slopsonic";

#[cfg(feature = "development")]
pub const PROJECT_NAME: &str = "slopsonic.devel";
#[cfg(not(feature = "development"))]
pub const PROJECT_NAME: &str = "slopsonic";
