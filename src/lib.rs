//! VoiceBox APO: streaming GTCRN microphone speech enhancement.
//!
//! Windows/COM plumbing lives in [`com`]; the real-time algorithm lives in
//! [`dsp`].

mod com;
mod config;
mod dsp;
mod etw;
mod settings;

pub use com::{DllCanUnloadNow, DllGetClassObject};
