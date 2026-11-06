//! VoiceBox APO: high-pass filtering, UL-UNAS denoising and peak limiting.

mod apo_cfg_watcher;
mod com;
mod etw;
mod memory;
mod soundboard;
mod worker;

pub use crate::com::factory::{DllCanUnloadNow, DllGetClassObject};
