//! Audio primitives, APO configuration, EXE/APO contracts and Windows helpers shared by both targets.
pub mod apo_cfg;
#[allow(dead_code)] // These shared modules have different consumers in the two targets.
pub mod audio;
#[allow(dead_code)]
pub mod error;
#[allow(dead_code)] // Windows installation paths are only used by the EXE.
pub mod paths;
#[allow(dead_code)]
pub mod pcm_transport;
pub mod plugin;
#[allow(dead_code)] // ABI symbols are used by the EXE; limits are used by both.
pub mod preview;
#[allow(dead_code)]
pub mod resample;
pub mod utils;
