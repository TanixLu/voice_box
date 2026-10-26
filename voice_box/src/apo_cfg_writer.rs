//! Management-only initialization and atomic writes to apo.cfg.
use crate::i18n::Text;
use crate::i18n::UserContext;
use voice_box_shared::{
    apo_cfg::{ApoCfgFile, ApoConfig},
    paths,
};

use atomic_write_file::AtomicWriteFile;
use std::{
    io::{self, Write},
    path::Path,
};

pub fn is_readable() -> bool {
    ApoCfgFile::try_load().is_ok()
}

pub fn initialize() -> anyhow::Result<()> {
    let directory = paths::data_dir()?;
    std::fs::create_dir_all(&directory).user_context(Text::CreateDataDirectory)?;
    let path = directory.join(paths::APO_CFG_FILE);
    match ApoCfgFile::read(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            save_to(&path, &ApoConfig::default())?;
        }
        Err(error) => return Err(error).user_context(Text::ReadApoCfgPreserved),
    }
    Ok(())
}

pub fn save(config: ApoConfig) -> anyhow::Result<()> {
    save_to(&paths::data_dir()?.join(paths::APO_CFG_FILE), &config)
        .user_context(Text::SaveApoCfgFailed)?;
    tracing::info!(
        denoising_enabled = config.denoising_enabled,
        gain_millidb = config.gain_millidb,
        "Saved APO configuration"
    );
    Ok(())
}

fn save_to(path: &Path, config: &ApoConfig) -> anyhow::Result<()> {
    let mut cfg = match ApoCfgFile::read(path) {
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
