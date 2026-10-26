//! VoiceBox APO: high-pass filtering, UL-UNAS denoising and peak limiting.
mod apo_cfg_watcher;
mod com;
mod dsp;
mod etw;
mod memory;
mod preview;
mod soundboard;
mod worker;

pub use com::factory::{DllCanUnloadNow, DllGetClassObject};
