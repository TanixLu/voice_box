//! VoiceBox SFX APO: a format-agnostic five-second microphone delay.
//!
//! Windows/COM plumbing lives in [`com`]; the real-time algorithm lives in
//! [`dsp`].

mod com;
mod dsp;
mod etw;

pub use com::{DllCanUnloadNow, DllGetClassObject};
