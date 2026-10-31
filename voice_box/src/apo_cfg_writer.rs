//! Management-only initialization and atomic writes to apo.cfg.

use std::{
    io::{self, Write},
    path::Path,
};

use atomic_write_file::AtomicWriteFile;

use voice_box_shared::{
    apo_cfg::{ApoCfgFile, ApoConfig},
    paths::{apo_cfg_path, data_dir},
};

use crate::i18n::{Text, UserContext};

pub fn initialize() -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir()?).user_context(Text::CreateDataDirectory)?;
    match ApoCfgFile::read() {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            save_to(apo_cfg_path()?, &ApoConfig::default())?;
        }
        Err(error) => return Err(error).user_context(Text::ReadApoCfgPreserved),
    }
    Ok(())
}

pub fn save(config: ApoConfig) -> anyhow::Result<()> {
    save_to(apo_cfg_path()?, &config).user_context(Text::SaveApoCfgFailed)?;
    tracing::info!(
        denoising_enabled = config.denoising_enabled,
        gain_millidb = config.gain_millidb,
        "Saved APO configuration"
    );
    Ok(())
}

fn save_to(path: &Path, config: &ApoConfig) -> anyhow::Result<()> {
    let mut cfg = match ApoCfgFile::read() {
        Ok(cfg) => cfg,
        Err(error) if error.kind() == io::ErrorKind::NotFound => ApoCfgFile::default(),
        Err(error) => return Err(error).user_context(Text::ReadApoCfgPreserved),
    };
    cfg.config = config.clone();
    let bytes = cfg.encode()?;
    let mut file = AtomicWriteFile::open(path).user_context(Text::SaveApoCfgFailed)?;
    file.write_all(&bytes)
        .user_context(Text::SaveApoCfgFailed)?;
    file.commit().user_context(Text::SaveApoCfgFailed)
}
