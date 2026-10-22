//! Audio engine plugin: COM activation, DSP, settings watching and ETW.
mod com;
mod dsp;
mod etw;
mod memory;
mod preview;
mod settings_watcher;
mod soundboard;
mod worker;

pub use com::factory::{DllCanUnloadNow, DllGetClassObject};
