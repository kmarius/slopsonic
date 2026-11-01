//! Slopsonic's application identity shared by runtime and platform integrations.

pub const STABLE_APP_ID: &str = "io.github.kmarius.slopsonic";
pub const STABLE_DISPLAY_NAME: &str = "Slopsonic";
pub const STABLE_PROJECT_NAME: &str = "slopsonic";
pub const DEVELOPMENT_APP_ID: &str = "io.github.kmarius.slopsonic.Devel";
pub const DEVELOPMENT_DISPLAY_NAME: &str = "Slopsonic (Development)";

#[cfg(feature = "development")]
pub const APP_ID: &str = DEVELOPMENT_APP_ID;
#[cfg(not(feature = "development"))]
pub const APP_ID: &str = STABLE_APP_ID;

#[cfg(feature = "development")]
pub const DISPLAY_NAME: &str = DEVELOPMENT_DISPLAY_NAME;
#[cfg(not(feature = "development"))]
pub const DISPLAY_NAME: &str = STABLE_DISPLAY_NAME;

#[cfg(feature = "development")]
pub const PROJECT_NAME: &str = "slopsonic.devel";
#[cfg(not(feature = "development"))]
pub const PROJECT_NAME: &str = STABLE_PROJECT_NAME;
