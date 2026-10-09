//! VoiceBox APO: AEC3 echo cancellation followed by GTCRN speech enhancement.
//!
//! APO implementation lives in [`apo`]; shared data formats live in [`shared`].

mod apo;
mod shared;

pub use apo::{DllCanUnloadNow, DllGetClassObject};
