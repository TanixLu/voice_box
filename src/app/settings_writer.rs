//! Management-only initialization and atomic settings commits.
use super::soundboard::config::Config as SoundboardConfig;
use crate::shared::{paths, settings::Settings};
use anyhow::Context;
use atomic_write_file::AtomicWriteFile;
use std::{
    io::{self, Write},
    path::Path,
};

pub fn ready() -> bool {
    Settings::try_load().is_ok()
}

pub fn initialize() -> anyhow::Result<()> {
    let directory = paths::data_dir()?;
    std::fs::create_dir_all(&directory).context("创建 VoiceBox 数据目录失败")?;
    let path = directory.join(paths::SETTINGS_FILE);
    match Settings::read(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            save_to(&directory, &Settings::default())?;
        }
        Err(error) => return Err(error).context("读取 VoiceBox 配置文件失败，保留原文件"),
    }
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
    let bytes = settings.encode()?;
    save_bytes(directory, paths::SETTINGS_FILE, &bytes)
}

pub fn save_soundboard(config: &SoundboardConfig) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(config)?;
    bytes.push(b'\n');
    save_bytes(&paths::data_dir()?, paths::SOUNDBOARD_SETTINGS_FILE, &bytes)
        .context("保存音效配置失败")
}

fn save_bytes(directory: &Path, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let path = directory.join(name);
    // Publish the complete configuration with a single atomic commit.
    let mut file = AtomicWriteFile::open(&path).context("创建临时配置文件失败")?;
    file.write_all(bytes).context("写入临时配置文件失败")?;
    file.commit().context("提交配置文件失败")
}
