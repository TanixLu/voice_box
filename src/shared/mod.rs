//! Settings, plugin registration properties and Windows helpers shared by both targets.
#[allow(dead_code)] // These shared modules have different consumers in the two targets.
pub mod audio;
pub mod paths;
pub mod plugin;
#[allow(dead_code)]
pub mod resample;
pub mod settings;
pub mod utils;
