//! Application settings, separate from APO parameters and eframe window state.
use super::{
    i18n::{LanguagePreference, Text, UserContext},
    settings_writer,
    soundboard::config::Config as SoundboardConfig,
};
use crate::shared::paths;
use serde::{Deserialize, Serialize};
use std::{fs::File, io, sync::Mutex};

const APP_SETTINGS_FILE: &str = "app_settings.json";
// Serialize the complete read-modify-write operation, not just the file commit.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Settings {
    pub language: LanguagePreference,
    pub soundboard: SoundboardConfig,
}

fn read<T: serde::de::DeserializeOwned + Default>() -> anyhow::Result<T> {
    let path = paths::data_dir()?.join(APP_SETTINGS_FILE);
    match File::open(path) {
        Ok(file) => {
            serde_json::from_reader(io::BufReader::new(file)).user_context(Text::InvalidAppSettings)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error).user_context(Text::ReadAppSettings),
    }
}

impl Settings {
    pub(super) fn try_load() -> anyhow::Result<Self> {
        read()
    }
}

pub(super) fn load_language() -> LanguagePreference {
    // Read language before validating the soundboard or starting the GUI so
    // startup failures can use the chosen language too.
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct LanguageSettings {
        language: LanguagePreference,
    }

    read::<LanguageSettings>().unwrap_or_default().language
}

fn update(change: impl FnOnce(&mut Settings)) -> anyhow::Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut settings = Settings::try_load()?;
    change(&mut settings);
    let mut bytes = serde_json::to_vec_pretty(&settings)?;
    bytes.push(b'\n');
    let directory = paths::data_dir()?;
    std::fs::create_dir_all(&directory).user_context(Text::CreateDataDirectory)?;
    settings_writer::save_bytes(&directory, APP_SETTINGS_FILE, &bytes)
}

pub(super) fn save_language(language: LanguagePreference) -> anyhow::Result<()> {
    update(|settings| settings.language = language).user_context(Text::SaveLanguageFailed)
}

pub(super) fn save_soundboard(soundboard: &SoundboardConfig) -> anyhow::Result<()> {
    update(|settings| settings.soundboard = soundboard.clone())
        .user_context(Text::SaveSoundboardConfig)
}
