//! Machine-wide apo.cfg shared by the GUI and APO watcher.

use std::io::{self, Read};

use crate::utils::{GainDb, WriteMode, apo_cfg_path, field_name, write_file_with_parents};

#[derive(Clone, PartialEq, Eq)]
pub struct ApoCfg {
    pub denoising_enabled: bool,
    pub gain_db: GainDb,
    pub playback_include_microphone_audio: bool,
}

impl Default for ApoCfg {
    fn default() -> Self {
        Self {
            denoising_enabled: false,
            gain_db: GainDb::new(0),
            playback_include_microphone_audio: true,
        }
    }
}

impl ApoCfg {
    const MAX_CFG_BYTES: usize = 16_384;

    pub fn read() -> io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(apo_cfg_path())?
            .take((Self::MAX_CFG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > Self::MAX_CFG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The VoiceBox APO configuration file is too large",
            ));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut cfg = Self::default();
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
            } else if key == field_name!(cfg, gain_db) {
                if let Ok(v) = value.parse() {
                    cfg.gain_db = GainDb::new(v);
                }
            } else if key == field_name!(cfg, playback_include_microphone_audio) {
                if let Ok(v) = value.parse() {
                    cfg.playback_include_microphone_audio = v;
                }
            }
        }
        Ok(cfg)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let bytes = format!(
            "{}={}\n{}={}\n{}={}\n",
            field_name!(self, denoising_enabled),
            self.denoising_enabled,
            field_name!(self, gain_db),
            self.gain_db.as_i8(),
            field_name!(self, playback_include_microphone_audio),
            self.playback_include_microphone_audio,
        )
        .into_bytes();
        if bytes.len() > Self::MAX_CFG_BYTES {
            anyhow::bail!("The VoiceBox APO configuration file is too large");
        }
        write_file_with_parents(apo_cfg_path(), WriteMode::AtomicReplace, |file| {
            file.write_all(&bytes)
        })?;
        Ok(())
    }
}
