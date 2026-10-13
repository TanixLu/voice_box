//! Machine-wide settings shared by the GUI and APO settings watcher.
use super::paths;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::Path,
};

pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;
const MAX_SETTINGS_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum DenoiseModel {
    #[default]
    #[serde(rename = "deep_filter_net3")]
    DeepFilterNet3 = 0,
    #[serde(rename = "ul_unas")]
    UlUnas = 1,
    #[serde(rename = "gtcrn")]
    Gtcrn = 2,
    #[serde(rename = "off")]
    Off = 3,
}

impl DenoiseModel {
    #[allow(dead_code)] // Only the GUI enumerates the selectable options.
    pub const ALL: [Self; 4] = [Self::Off, Self::DeepFilterNet3, Self::UlUnas, Self::Gtcrn];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gtcrn => "gtcrn",
            Self::Off => "off",
            Self::DeepFilterNet3 => "deep_filter_net3",
            Self::UlUnas => "ul_unas",
        }
    }

    // GUI labels are not used by the APO DLL.
    #[allow(dead_code)]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Gtcrn => "GTCRN",
            Self::Off => "关闭",
            Self::DeepFilterNet3 => "DeepFilterNet3",
            Self::UlUnas => "UL-UNAS",
        }
    }

    // Index encoding is only used by the APO's real-time snapshot and DSP.
    #[allow(dead_code)]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[allow(dead_code)]
    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::UlUnas,
            2 => Self::Gtcrn,
            3 => Self::Off,
            _ => Self::DeepFilterNet3,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub denoise_model: DenoiseModel,
    pub gain_millidb: i32,
}

impl Settings {
    pub fn validate(&self) -> io::Result<()> {
        if !(MIN_GAIN_MILLIDB..=MAX_GAIN_MILLIDB).contains(&self.gain_millidb) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "增益必须介于 -60 和 +60 dB 之间",
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
        Self::read(&paths::data_dir()?.join(paths::SETTINGS_FILE))
    }
}

fn check_size(size: usize) -> io::Result<()> {
    if size > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "VoiceBox 配置文件过大",
        ));
    }
    Ok(())
}
