//! Application settings persisted as JSON.

use std::{fs, io};

use crate::{
    i18n::{LanguagePreference, Text, UserContext},
    paths::{WriteMode, settings_path, write_file_with_parents},
    soundboard::settings::SoundboardSettings,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub language_preference: LanguagePreference,
    pub theme_preference: egui::ThemePreference,
    pub startup_enabled: bool,
    pub soundboard: SoundboardSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language_preference: LanguagePreference::System,
            theme_preference: egui::ThemePreference::System,
            startup_enabled: false,
            soundboard: SoundboardSettings::default(),
        }
    }
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let bytes = match fs::read(settings_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).user_context(Text::LoadAppSettingsFailed),
        };
        serde_json::from_slice(&bytes).user_context(Text::LoadAppSettingsFailed)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).user_context(Text::SaveAppSettingsFailed)?;
        write_file_with_parents(settings_path(), WriteMode::AtomicReplace, |file| {
            file.write_all(&bytes)
        })
        .user_context(Text::SaveAppSettingsFailed)
    }
}
