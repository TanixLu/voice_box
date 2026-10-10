//! VoiceBox APO: high-pass filtering, AEC3, GTCRN and final peak limiting.
//!
//! APO implementation lives in [`apo`]; shared data formats live in [`shared`].

mod apo;
mod shared;

pub use apo::{DllCanUnloadNow, DllGetClassObject};
