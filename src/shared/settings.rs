//! Machine-wide settings shared by the GUI and APO settings watcher.
use super::paths;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::Path,
};

pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub gain_millidb: i32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettings {
    version: u32,
    enabled: bool,
    gain_millidb: i32,
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
    pub fn validate(self) -> io::Result<Self> {
        if !(MIN_GAIN_MILLIDB..=MAX_GAIN_MILLIDB).contains(&self.gain_millidb) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "增益必须介于 -60 和 +60 dB 之间",
            ));
        }
        Ok(self)
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        // Bound allocation even if an editable settings file is corrupted.
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(16_385)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16_384 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VoiceBox 配置文件过大",
            ));
        }
        let stored: StoredSettings = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if stored.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "不支持的 VoiceBox 配置版本",
            ));
        }
        Self {
            enabled: stored.enabled,
            gain_millidb: stored.gain_millidb,
        }
        .validate()
    }

    // Written by the GUI; the same schema module is read-only in the APO DLL.
    #[allow(dead_code)]
    pub fn encode(self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(&StoredSettings {
            version: 1,
            enabled: self.enabled,
            gain_millidb: self.gain_millidb,
        })
        .map_err(io::Error::other)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn try_load() -> io::Result<Self> {
        Self::read(&paths::data_dir()?.join(paths::SETTINGS_FILE))
    }
}
