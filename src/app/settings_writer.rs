//! Management-only initialization and atomic settings commits.
use super::data_permissions;
use crate::shared::{paths, settings::Settings};
use anyhow::{Context, ensure};
use atomic_write_file::AtomicWriteFile;
use std::{
    io::{self, Write},
    path::Path,
};

pub fn ready() -> bool {
    let Ok(directory) = paths::data_dir() else {
        return false;
    };
    data_permissions::ready(&directory).unwrap_or(false) && Settings::try_load().is_ok()
}

pub fn initialize() -> anyhow::Result<()> {
    let directory = paths::data_dir()?;
    data_permissions::initialize(&directory)?;
    let path = directory.join(paths::SETTINGS_FILE);
    match Settings::read(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            save_to(&directory, &Settings::default())?;
        }
        Err(error) => return Err(error).context("读取 VoiceBox 配置文件失败，保留原文件"),
    }
    ensure!(ready(), "VoiceBox 数据目录或配置文件检查未通过");
    Ok(())
}

pub fn save(settings: Settings) -> anyhow::Result<()> {
    save_to(&paths::data_dir()?, &settings).context("无法保存 VoiceBox 配置文件")?;
    tracing::info!(
        enable_denoising = settings.enable_denoising,
        gain_millidb = settings.gain_millidb,
        "Saved audio settings"
    );
    Ok(())
}

fn save_to(directory: &Path, settings: &Settings) -> anyhow::Result<()> {
    let path = directory.join(paths::SETTINGS_FILE);
    let bytes = settings.encode()?;
    // The temporary file inherits Everyone Modify from its parent directory.
    // Only commit publishes the fully written file to the APO watcher.
    let mut file = AtomicWriteFile::open(&path).context("创建临时配置文件失败")?;
    file.write_all(&bytes).context("写入临时配置文件失败")?;
    file.commit().context("提交配置文件失败")
}
