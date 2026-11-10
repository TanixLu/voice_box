//! VoiceBox APO: high-pass filtering, UL-UNAS denoising and peak limiting.
//!
//! Layout: `com` is the only module the audio engine calls into. It reaches the non-real-time
//! services below it: `config` (registry watcher feeding `RtConfig`), `injection` (soundboard
//! audio read from the EXE's PCM transport), `memory` (page residency) and `worker` (detached
//! threads). DSP itself lives in `voice_box_shared::dsp` so the EXE previews the same chain.

mod com;
mod config;
mod etw;
mod injection;
mod memory;
mod worker;

pub use crate::com::factory::{DllCanUnloadNow, DllGetClassObject};
