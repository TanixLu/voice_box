//! Audio engine plugin: COM activation, DSP, settings watching and ETW.
mod com;
mod dsp;
mod etw;
mod loopback;
mod reference;
mod reference_buffer;
mod settings_watcher;
mod worker;

pub use com::{DllCanUnloadNow, DllGetClassObject};
