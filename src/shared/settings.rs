//! Machine-wide settings shared by the GUI and APO settings watcher.
use super::paths;
use crate::shared::error::SharedError;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::Path,
};

pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;
const MAX_SETTINGS_BYTES: usize = 16_384;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub denoising_enabled: bool,
    pub gain_millidb: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            denoising_enabled: true,
            gain_millidb: 0,
        }
    }
}

impl Settings {
    #[allow(dead_code)] // Conversion stays in the DLL, including offline preview.
    pub fn linear_gain(&self) -> f32 {
        if self.gain_millidb == MIN_GAIN_MILLIDB {
            0.0
        } else {
            10.0_f32.powf(self.gain_millidb as f32 / 20_000.0)
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if !(MIN_GAIN_MILLIDB..=MAX_GAIN_MILLIDB).contains(&self.gain_millidb) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                SharedError::GainOutOfRange,
            ));
        }
        Ok(())
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        // Bound allocation even if an editable settings file is corrupted.
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((MAX_SETTINGS_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        check_size(bytes.len())?;
        let settings: Self = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        settings.validate()?;
        Ok(settings)
    }

    // Written by the GUI; the same schema module is read-only in the APO DLL.
    #[allow(dead_code)]
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        check_size(bytes.len())?;
        Ok(bytes)
    }

    pub fn try_load() -> io::Result<Self> {
        Self::read(&paths::data_dir()?.join(paths::APO_SETTINGS_FILE))
    }
}

fn check_size(size: usize) -> io::Result<()> {
    if size > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            SharedError::SettingsTooLarge,
        ));
    }
    Ok(())
}
