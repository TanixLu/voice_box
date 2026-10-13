//! Machine-wide settings shared by the GUI and APO settings watcher.
use super::paths;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    io::{self, Read},
    path::Path,
};

pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;
const MAX_SETTINGS_BYTES: usize = 16_384;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ReferenceDevice {
    #[default]
    DefaultPlayback,
    DefaultCommunications,
    Device(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub gain_millidb: i32,
    pub aec_enabled: bool,
    pub reference_device: ReferenceDevice,
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
        // Missing fields use current defaults; unknown fields do not prevent
        // reading the settings this build understands.
        let settings: Self = serde_json::from_value(Value::Object(read_document(path)?))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        settings.validate()?;
        Ok(settings)
    }

    // Written by the GUI; the same schema module is read-only in the APO DLL.
    #[allow(dead_code)]
    pub fn encode(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.validate()?;
        // Re-read at save time so even unknown fields added since the GUI
        // opened survive a save or a reset to current defaults.
        let mut document = match read_document(path) {
            Ok(document) => document,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Map::new(),
            Err(error) => return Err(error),
        };
        let Value::Object(known) = serde_json::to_value(self).map_err(io::Error::other)? else {
            unreachable!("Settings serializes to an object");
        };
        for (key, mut value) in known {
            if key == "reference_device"
                && let Value::Object(reference) = &mut value
                && let Some(Value::Object(mut previous)) = document.remove(&key)
            {
                // kind/id are ours; retain any additional device metadata.
                previous.remove("kind");
                previous.remove("id");
                reference.extend(previous);
            }
            document.insert(key, value);
        }
        let mut bytes = serde_json::to_vec_pretty(&document).map_err(io::Error::other)?;
        bytes.push(b'\n');
        check_size(bytes.len())?;
        Ok(bytes)
    }

    pub fn try_load() -> io::Result<Self> {
        Self::read(&paths::data_dir()?.join(paths::SETTINGS_FILE))
    }
}

fn read_document(path: &Path) -> io::Result<Map<String, Value>> {
    // Bound allocation even if an editable settings file is corrupted.
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_SETTINGS_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    check_size(bytes.len())?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
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
