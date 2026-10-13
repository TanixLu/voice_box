//! Audio engine plugin: COM activation, DSP, settings watching and ETW.
mod com;
mod dsp;
mod etw;
mod settings_watcher;
mod worker;

pub use com::{DllCanUnloadNow, DllGetClassObject};
