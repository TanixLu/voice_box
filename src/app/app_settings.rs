//! Application settings and sound metadata in voice_box.db.
use super::{
    database,
    i18n::{LanguagePreference, Text, UserContext},
    soundboard::{config::Config as SoundboardConfig, library::Library},
};
use anyhow::ensure;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Default)]
pub(super) struct Settings {
    pub language: LanguagePreference,
    pub soundboard: SoundboardConfig,
}

fn read<T: serde::de::DeserializeOwned + Default>(
    connection: &Connection,
    key: &str,
) -> anyhow::Result<T> {
    let value: Option<String> = connection
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    match value {
        Some(value) => serde_json::from_str(&value).user_context(Text::InvalidAppSettings),
        None => Ok(T::default()),
    }
}

fn write(connection: &Connection, key: &str, value: &impl serde::Serialize) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, serde_json::to_string(value)?],
    )?;
    Ok(())
}

impl Settings {
    pub(super) fn try_load() -> anyhow::Result<Self> {
        let mut connection = database::open()?;
        let transaction = connection.transaction()?;
        Ok(Self {
            language: read(&transaction, "language")?,
            soundboard: SoundboardConfig {
                options: read(&transaction, "soundboard")?,
                library: Library::load(&transaction)?,
            },
        })
    }
}

pub(super) fn load_language() -> LanguagePreference {
    database::open()
        .and_then(|connection| read(&connection, "language"))
        .unwrap_or_default()
}

pub(super) fn save_language(language: LanguagePreference) -> anyhow::Result<()> {
    write(&database::open()?, "language", &language).user_context(Text::SaveLanguageFailed)
}

pub(super) fn save_soundboard(soundboard: &SoundboardConfig) -> anyhow::Result<()> {
    save_soundboard_to(&mut database::open()?, soundboard).user_context(Text::SaveSoundboardConfig)
}

fn save_soundboard_to(
    connection: &mut Connection,
    soundboard: &SoundboardConfig,
) -> anyhow::Result<()> {
    let transaction = connection.transaction()?;
    write(&transaction, "soundboard", &soundboard.options)?;
    {
        let mut update = transaction
            .prepare("UPDATE sounds SET name = ?1, position = ?2, hotkey = ?3 WHERE id = ?4")?;
        for (position, sound) in soundboard.library.sounds.iter().enumerate() {
            ensure!(
                super::soundboard::library::is_valid_name(&sound.name),
                Text::InvalidSoundName
            );
            let hotkey = sound
                .hotkey
                .map(|key| serde_json::to_string(&key))
                .transpose()?;
            ensure!(
                update.execute(params![sound.name, position as i64, hotkey, sound.id])? == 1,
                Text::SoundNotFound
            );
        }
    }
    transaction.commit()?;
    Ok(())
}
