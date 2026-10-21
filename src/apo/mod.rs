//! Audio engine plugin: COM activation, DSP, settings watching and ETW.
mod com;
mod dsp;
mod etw;
mod preview;
mod settings_watcher;
mod soundboard;
mod worker;

pub use com::factory::{DllCanUnloadNow, DllGetClassObject};
