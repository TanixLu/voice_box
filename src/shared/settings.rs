//! Machine-wide settings shared by the GUI and APO settings watcher.
use super::paths;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read},
    path::Path,
};

pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ReferenceDevice {
    #[default]
    DefaultPlayback,
    DefaultCommunications,
    Device(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub gain_millidb: i32,
    pub aec_enabled: bool,
    pub reference_device: ReferenceDevice,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSettings {
    version: u32,
    enabled: bool,
    gain_millidb: i32,
    aec_enabled: bool,
    reference_device: ReferenceDevice,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            gain_millidb: 0,
            aec_enabled: false,
            reference_device: ReferenceDevice::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> io::Result<()> {
        if !(MIN_GAIN_MILLIDB..=MAX_GAIN_MILLIDB).contains(&self.gain_millidb) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "增益必须介于 -60 和 +60 dB 之间",
            ));
        }
        if let ReferenceDevice::Device(id) = &self.reference_device
            && (id.is_empty() || id.len() > 1024 || id.contains('\0'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "无效的回声参考输出设备",
            ));
        }
        Ok(())
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
        Self::decode(&bytes)
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let stored: StoredSettings = serde_json::from_slice(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if stored.version != 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "不支持的 VoiceBox 配置版本",
            ));
        }
        let settings = Self {
            enabled: stored.enabled,
            gain_millidb: stored.gain_millidb,
            aec_enabled: stored.aec_enabled,
            reference_device: stored.reference_device,
        };
        settings.validate()?;
        Ok(settings)
    }

    // Written by the GUI; the same schema module is read-only in the APO DLL.
    #[allow(dead_code)]
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(&StoredSettings {
            version: 2,
            enabled: self.enabled,
            gain_millidb: self.gain_millidb,
            aec_enabled: self.aec_enabled,
            reference_device: self.reference_device.clone(),
        })
        .map_err(io::Error::other)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn try_load() -> io::Result<Self> {
        Self::read(&paths::data_dir()?.join(paths::SETTINGS_FILE))
    }
}
