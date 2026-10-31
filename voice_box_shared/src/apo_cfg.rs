//! Machine-wide apo.cfg shared by the GUI and APO watcher.

use std::io::{self, Read};

use crate::{error::SharedError, paths::apo_cfg_path};

pub const MIN_GAIN_MILLIDB: i32 = -60_000;
pub const MAX_GAIN_MILLIDB: i32 = 60_000;
const MAX_CFG_BYTES: usize = 16_384;

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct ApoConfig {
    pub denoising_enabled: bool,
    pub gain_millidb: i32,
}

impl ApoConfig {
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
}

#[derive(Default)]
pub struct ApoCfgFile {
    pub config: ApoConfig,
    unknown: String,
}

impl ApoCfgFile {
    pub fn read() -> io::Result<Self> {
        // Bound allocation even if an editable configuration file is corrupted.
        let mut bytes = Vec::new();
        std::fs::File::open(apo_cfg_path()?)?
            .take((MAX_CFG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok(Self::decode(&bytes))
    }

    fn decode(bytes: &[u8]) -> Self {
        let mut cfg = Self::default();
        let Ok(text) = std::str::from_utf8(bytes) else {
            return cfg;
        };
        if bytes.len() > MAX_CFG_BYTES {
            return cfg;
        }
        for line in text.split_inclusive('\n') {
            let entry = line.trim();
            if entry.chars().any(|c| c.is_control() && c != '\t') {
                continue;
            }
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("", _) => {}
                ("denoising_enabled", value) => {
                    if let Ok(value) = value.parse() {
                        cfg.config.denoising_enabled = value;
                    }
                }
                ("gain_millidb", value) => {
                    if let Ok(value) = value.parse()
                        && (MIN_GAIN_MILLIDB..=MAX_GAIN_MILLIDB).contains(&value)
                    {
                        cfg.config.gain_millidb = value;
                    }
                }
                _ => cfg.unknown.push_str(line),
            }
        }
        cfg
    }

    // Written by the GUI; the same schema module is read-only in the APO DLL.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.config.validate()?;
        let bytes = format!(
            "denoising_enabled={}\ngain_millidb={}\n{}",
            self.config.denoising_enabled, self.config.gain_millidb, self.unknown
        )
        .into_bytes();
        check_size(bytes.len())?;
        Ok(bytes)
    }
}

fn check_size(size: usize) -> io::Result<()> {
    if size > MAX_CFG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            SharedError::ApoCfgTooLarge,
        ));
    }
    Ok(())
}
