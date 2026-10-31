//! Machine-wide apo.cfg shared by the GUI and APO watcher.

use std::io::{self, Read};

use crate::{
    field_name,
    paths::apo_cfg_path,
    utils::{WriteMode, write_file_with_parents},
};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub struct GainMillidb(i32);

impl GainMillidb {
    pub const MIN_VALUE: i32 = -60_000;
    pub const MAX_VALUE: i32 = 60_000;

    pub fn new(value: i32) -> Self {
        Self(value.clamp(Self::MIN_VALUE, Self::MAX_VALUE))
    }

    pub fn parse(s: &str) -> Result<Self, std::num::ParseIntError> {
        let value = s.parse::<i32>()?;
        Ok(Self::new(value))
    }

    pub fn as_i32(&self) -> i32 {
        self.0
    }

    pub fn as_f32_gain(&self) -> f32 {
        if self.0 == Self::MIN_VALUE {
            0.0
        } else {
            10.0_f32.powf(self.0 as f32 / 20_000.0)
        }
    }
}

#[derive(Default, Clone, PartialEq, Eq)]
pub struct ApoCfg {
    pub denoising_enabled: bool,
    pub gain_millidb: GainMillidb,
}

impl ApoCfg {
    const MAX_CFG_BYTES: usize = 16_384;

    pub fn read() -> io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(apo_cfg_path()?)?
            .take((Self::MAX_CFG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        let bytes = &bytes[..];
        let mut cfg = Self::default();
        if bytes.len() > Self::MAX_CFG_BYTES {
            return Ok(cfg);
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Ok(cfg);
        };
        for line in text.split_inclusive('\n') {
            let entry = line.trim();
            if entry.chars().any(|c| c.is_control() && c != '\t') {
                continue;
            }
            let Some((key, value)) = entry.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key == field_name!(cfg, denoising_enabled) {
                if let Ok(v) = value.parse() {
                    cfg.denoising_enabled = v;
                }
            } else if key == field_name!(cfg, gain_millidb) {
                if let Ok(v) = GainMillidb::parse(value) {
                    cfg.gain_millidb = v;
                }
            }
        }
        Ok(cfg)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let bytes = format!(
            "{}={}\n{}={}\n",
            field_name!(self, denoising_enabled),
            self.denoising_enabled,
            field_name!(self, gain_millidb),
            self.gain_millidb.as_i32(),
        )
        .into_bytes();
        if bytes.len() > Self::MAX_CFG_BYTES {
            anyhow::bail!("The VoiceBox APO configuration file is too large");
        }
        write_file_with_parents(apo_cfg_path()?, WriteMode::AtomicReplace, |file| {
            file.write_all(&bytes)
        })?;
        Ok(())
    }
}
