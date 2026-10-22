//! Management-only initialization and atomic settings commits.
use crate::app::i18n::Text;
use crate::app::i18n::UserContext;
use crate::shared::{paths, settings::Settings};

use atomic_write_file::AtomicWriteFile;
use std::{
    io::{self, Write},
    path::Path,
};

pub fn has_valid_settings() -> bool {
    Settings::try_load().is_ok()
}

pub fn initialize() -> anyhow::Result<()> {
    let directory = paths::data_dir()?;
    std::fs::create_dir_all(&directory).user_context(Text::CreateDataDirectory)?;
    let path = directory.join(paths::APO_SETTINGS_FILE);
    match Settings::read(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            save_to(&directory, &Settings::default())?;
        }
        Err(error) => return Err(error).user_context(Text::ReadSettingsPreserved),
    }
    Ok(())
}

pub fn save(settings: Settings) -> anyhow::Result<()> {
    save_to(&paths::data_dir()?, &settings).user_context(Text::SaveSettingsFailed)?;
    tracing::info!(
        denoising_enabled = settings.denoising_enabled,
        gain_millidb = settings.gain_millidb,
        "Saved audio settings"
    );
    Ok(())
}

fn save_to(directory: &Path, settings: &Settings) -> anyhow::Result<()> {
    let bytes = settings.encode()?;
    save_bytes(directory, paths::APO_SETTINGS_FILE, &bytes)
}

pub(super) fn save_bytes(directory: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = directory.join(name);
    // Publish the complete configuration with a single atomic commit.
    let mut file = AtomicWriteFile::open(&path).user_context(Text::CreateTemporarySettings)?;
    file.write_all(bytes)
        .user_context(Text::WriteTemporarySettings)?;
    file.commit().user_context(Text::CommitSettings)
}
