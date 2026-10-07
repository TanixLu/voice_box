//! Shared 64-bit machine-wide configuration schema.
use winreg::{
    RegKey,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY},
};

pub const ROOT: &str = r"SOFTWARE\VoiceBox";
pub const PATH: &str = constcat::concat!(ROOT, r"\Settings");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub gain_millidb: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            gain_millidb: 0,
        }
    }
}

impl Settings {
    pub fn read(key: &RegKey) -> Self {
        Self {
            enabled: key.get_value::<u32, _>("Enabled").unwrap_or(1) != 0,
            gain_millidb: (key.get_value::<u32, _>("GainMilliDb").unwrap_or(0) as i32)
                .clamp(-60_000, 12_000),
        }
    }
    pub fn load() -> Self {
        RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(PATH, KEY_READ | KEY_WOW64_64KEY)
            .map(|key| Self::read(&key))
            .unwrap_or_default()
    }
    pub fn gain_linear(self) -> f32 {
        10.0_f32.powf(self.gain_millidb as f32 / 20_000.0)
    }
}
