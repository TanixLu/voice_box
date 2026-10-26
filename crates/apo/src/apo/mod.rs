//! Audio engine plugin: COM activation, DSP, APO configuration watching and ETW.
mod apo_cfg_watcher;
mod com;
mod dsp;
mod etw;
mod memory;
mod preview;
mod soundboard;
mod worker;

pub use com::factory::{DllCanUnloadNow, DllGetClassObject};
