//! Typed application settings in voice_box.db's key/value table.

use super::{
    database,
    hotkeys::Hotkey,
    i18n::{LanguagePreference, Message, Text, UserContext},
    soundboard::library::Library,
};
use anyhow::ensure;
use rusqlite::{
    Connection, ToSql, Transaction, params,
    types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Value, ValueRef},
};
use std::collections::HashMap;
use voice_box_shared::pcm_transport::MAX_VOLUME;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BindingTarget {
    Sound(usize),
    Hold,
    Stop,
    Random,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct OutputDevice {
    pub id: String,
    pub name: String,
}

macro_rules! define_settings {
    ($($field:ident: $ty:ty = $default:expr;)*) => {
        #[derive(Clone, Debug, PartialEq)]
        pub(super) struct Settings {
            $(pub $field: $ty,)*
        }

        impl Default for Settings {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        impl Settings {
            fn read_from(connection: &Connection) -> anyhow::Result<Self> {
                let values = load_values(connection)?;
                let mut settings = Self {
                    $($field: read_setting(&values, stringify!($field), $default),)*
                };
                settings.sanitize();
                Ok(settings)
            }

            fn save_to(&self, connection: &mut Connection) -> anyhow::Result<()> {
                let transaction = connection.transaction()?;
                $(write_setting(&transaction, stringify!($field), &self.$field)?;)*
                transaction.commit()?;
                Ok(())
            }
        }
    };
}

define_settings! {
    language: LanguagePreference = LanguagePreference::System;
    theme: egui::ThemePreference = egui::ThemePreference::System;
    playback_microphone_output_device: Option<OutputDevice> = None;
    playback_speaker_output_device: Option<OutputDevice> = None;
    playback_speaker_output_volume: f32 = 1.0;
    playback_microphone_output_volume: f32 = 1.0;
    playback_include_microphone_audio: bool = true;
    playback_auto_hold_hotkey: Option<Hotkey> = None;
    playback_stop_hotkey: Option<Hotkey> = None;
    playback_random_hotkey: Option<Hotkey> = None;
    playback_show_overlay: bool = true;
    playback_repeat: bool = false;
}

impl Settings {
    pub(super) fn load() -> anyhow::Result<Self> {
        let mut connection = database::open()?;
        let transaction = connection.transaction()?;
        Self::read_from(&transaction)
    }

    pub(super) fn save(&self) -> anyhow::Result<()> {
        self.save_to(&mut database::open()?)
            .user_context(Text::SaveAppSettingsFailed)
    }

    pub(super) fn validate_binding(
        &self,
        library: &Library,
        target: BindingTarget,
        key: Hotkey,
    ) -> anyhow::Result<()> {
        ensure!(
            if target == BindingTarget::Hold {
                key.is_valid_for_hold()
            } else {
                key.is_valid()
            },
            Text::InvalidHotkey
        );
        let duplicate = (target != BindingTarget::Stop && self.playback_stop_hotkey == Some(key))
            || (target != BindingTarget::Random && self.playback_random_hotkey == Some(key))
            || (matches!(target, BindingTarget::Stop | BindingTarget::Random)
                && library.sounds.iter().any(|sound| sound.hotkey == Some(key)));
        ensure!(
            !duplicate || target == BindingTarget::Hold,
            Message::HotkeyConflict { key }
        );
        let overlaps_hold = match target {
            BindingTarget::Sound(_) | BindingTarget::Stop | BindingTarget::Random => self
                .playback_auto_hold_hotkey
                .is_some_and(|hold| hold.virtual_key_code == key.virtual_key_code),
            BindingTarget::Hold => {
                self.playback_stop_hotkey
                    .is_some_and(|shortcut| shortcut.virtual_key_code == key.virtual_key_code)
                    || self
                        .playback_random_hotkey
                        .is_some_and(|shortcut| shortcut.virtual_key_code == key.virtual_key_code)
                    || library.sounds.iter().any(|sound| {
                        sound.hotkey.is_some_and(|shortcut| {
                            shortcut.virtual_key_code == key.virtual_key_code
                        })
                    })
            }
        };
        ensure!(!overlaps_hold, Text::HoldKeyConflict);
        Ok(())
    }

    pub(super) fn validate(&self, library: &Library) -> anyhow::Result<()> {
        ensure!(
            [
                self.playback_speaker_output_volume,
                self.playback_microphone_output_volume
            ]
            .iter()
            .all(|volume| { volume.is_finite() && (0.0..=MAX_VOLUME).contains(volume) }),
            Text::InvalidPlaybackVolume
        );
        for (target, key) in [
            (BindingTarget::Hold, self.playback_auto_hold_hotkey),
            (BindingTarget::Stop, self.playback_stop_hotkey),
            (BindingTarget::Random, self.playback_random_hotkey),
        ] {
            if let Some(key) = key {
                self.validate_binding(library, target, key)?;
            }
        }
        Ok(())
    }

    pub(super) fn sanitize(&mut self) {
        for (key, volume) in [
            (
                "playback_speaker_output_volume",
                &mut self.playback_speaker_output_volume,
            ),
            (
                "playback_microphone_output_volume",
                &mut self.playback_microphone_output_volume,
            ),
        ] {
            let sanitized = if volume.is_finite() {
                volume.clamp(0.0, MAX_VOLUME)
            } else {
                1.0
            };
            if *volume != sanitized {
                tracing::warn!(key, "Invalid playback volume; sanitizing setting");
                *volume = sanitized;
            }
        }
        for (key, hotkey, hold) in [
            (
                "playback_auto_hold_hotkey",
                &mut self.playback_auto_hold_hotkey,
                true,
            ),
            (
                "playback_stop_hotkey",
                &mut self.playback_stop_hotkey,
                false,
            ),
            (
                "playback_random_hotkey",
                &mut self.playback_random_hotkey,
                false,
            ),
        ] {
            if hotkey.is_some_and(|key| {
                if hold {
                    !key.is_valid_for_hold()
                } else {
                    !key.is_valid()
                }
            }) {
                tracing::warn!(key, "Invalid hotkey; clearing setting");
                *hotkey = None;
            }
        }
    }
    pub(super) fn for_output(&self, microphone_available: bool) -> Self {
        let mut settings = self.clone();
        if !microphone_available {
            settings.playback_microphone_output_volume = 0.0;
            settings.playback_include_microphone_audio = true;
        }
        settings
    }
}

// SQL queries fail normally; only conversion of successfully read values can fall back.
type Values = HashMap<String, Value>;

fn load_values(connection: &Connection) -> rusqlite::Result<Values> {
    let mut query = connection.prepare_cached("SELECT key, value FROM settings")?;
    query
        .query_map([], |row| {
            let key: String = row.get(0)?;
            match Value::column_result(row.get_ref(1)?) {
                Ok(value) => Ok(Some((key, value))),
                Err(error) => {
                    tracing::warn!(key, %error, "Invalid setting; using default");
                    Ok(None)
                }
            }
        })?
        .filter_map(|row| row.transpose())
        .collect()
}

trait Stored: Sized {
    fn read(values: &Values, key: &str) -> FromSqlResult<Option<Self>>;
    fn write(&self, transaction: &Transaction<'_>, key: &str) -> rusqlite::Result<()>;
}

fn read_setting<T: Stored>(values: &Values, key: &str, default: T) -> T {
    match T::read(values, key) {
        Ok(Some(value)) => value,
        Ok(None) => default,
        Err(error) => {
            tracing::warn!(key, %error, "Invalid setting; using default");
            default
        }
    }
}

fn write_setting<T: Stored>(
    transaction: &Transaction<'_>,
    key: &str,
    value: &T,
) -> rusqlite::Result<()> {
    value.write(transaction, key)
}

fn scalar<T: FromSql>(values: &Values, key: &str) -> FromSqlResult<Option<T>> {
    values
        .get(key)
        .map(|value| T::column_result(value.into()))
        .transpose()
}

fn write_scalar<T: ToSql + ?Sized>(
    transaction: &Transaction<'_>,
    key: &str,
    value: &T,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn invalid(message: &str) -> FromSqlError {
    FromSqlError::Other(message.into())
}

macro_rules! scalar_settings {
    ($($ty:ty),* $(,)?) => {$(
        impl Stored for $ty {
            fn read(values: &Values, key: &str) -> FromSqlResult<Option<Self>> {
                scalar(values, key)
            }

            fn write(&self, transaction: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
                write_scalar(transaction, key, self)
            }
        }
    )*};
}

scalar_settings!(String, f32, f64, i32, i64, u16, u32);

impl Stored for bool {
    fn read(values: &Values, key: &str) -> FromSqlResult<Option<Self>> {
        match scalar::<i64>(values, key)? {
            None => Ok(None),
            Some(0) => Ok(Some(false)),
            Some(1) => Ok(Some(true)),
            Some(_) => Err(invalid("boolean must be 0 or 1")),
        }
    }

    fn write(&self, transaction: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
        write_scalar(transaction, key, self)
    }
}

macro_rules! enum_setting {
    ($ty:ty { $($variant:ident => $value:literal),* $(,)? }) => {
        impl Stored for $ty {
            fn read(values: &Values, key: &str) -> FromSqlResult<Option<Self>> {
                match scalar::<String>(values, key)?.as_deref() {
                    None => Ok(None),
                    $(Some($value) => Ok(Some(Self::$variant)),)*
                    Some(_) => Err(invalid("unknown enum value")),
                }
            }

            fn write(&self, transaction: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
                let value = match self { $(Self::$variant => $value,)* };
                write_scalar(transaction, key, value)
            }
        }
    };
}

enum_setting!(LanguagePreference {
    System => "system",
    English => "en",
    SimplifiedChinese => "zh-CN",
});

enum_setting!(egui::ThemePreference {
    System => "system",
    Light => "light",
    Dark => "dark",
});

impl<T: FromSql + ToSql> Stored for Option<T> {
    fn read(values: &Values, key: &str) -> FromSqlResult<Option<Self>> {
        scalar(values, key)
    }

    fn write(&self, transaction: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
        if let Some(value) = self {
            write_scalar(transaction, key, value)?;
        } else {
            transaction.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        }
        Ok(())
    }
}

impl FromSql for OutputDevice {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let (id, name) = value
            .as_str()?
            .split_once('\0')
            .ok_or_else(|| invalid("output device must contain ID and name separated by NUL"))?;
        if id.is_empty() {
            return Err(invalid("output device ID is empty"));
        }
        Ok(Self {
            id: id.to_owned(),
            name: name.to_owned(),
        })
    }
}

impl ToSql for OutputDevice {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        // Windows endpoint IDs cannot contain NUL; the name occupies the remaining text.
        if self.id.is_empty() || self.id.contains('\0') {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "output device ID must be nonempty and contain no NUL".into(),
            ));
        }
        Ok(ToSqlOutput::Owned(Value::Text(format!(
            "{}\0{}",
            self.id, self.name
        ))))
    }
}

impl FromSql for Hotkey {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let (virtual_key_code, modifiers) = value
            .as_str()?
            .split_once(':')
            .ok_or_else(|| invalid("hotkey must be key_code:modifiers"))?;
        Ok(Self {
            virtual_key_code: virtual_key_code
                .parse()
                .map_err(|error| FromSqlError::Other(Box::new(error)))?,
            modifiers: modifiers
                .parse()
                .map_err(|error| FromSqlError::Other(Box::new(error)))?,
        })
    }
}

impl ToSql for Hotkey {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Text(format!(
            "{}:{}",
            self.virtual_key_code, self.modifiers
        ))))
    }
}
