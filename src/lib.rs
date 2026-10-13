//! VoiceBox APO: high-pass filtering, DeepFilterNet3/UL-UNAS/GTCRN and peak limiting.
//!
//! APO implementation lives in [`apo`]; shared data formats live in [`shared`].

mod apo;
mod shared;

pub use apo::{DllCanUnloadNow, DllGetClassObject};
