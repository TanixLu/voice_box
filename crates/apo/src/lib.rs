//! VoiceBox APO: high-pass filtering, UL-UNAS denoising and peak limiting.
//!
//! APO implementation lives in [`apo`]; shared data formats live in [`shared`].

mod apo;
use voice_box_shared as shared;

pub use apo::{DllCanUnloadNow, DllGetClassObject};
