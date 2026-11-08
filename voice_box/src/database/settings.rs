//! Settings persistence; the field list is owned by the settings model.

use std::collections::HashMap;

use rusqlite::{
    Connection, ToSql, Transaction, params,
    types::{FromSql, FromSqlError, FromSqlResult, Value},
};

use crate::{i18n::LanguagePreference, settings::Settings, utils::GainPercent};

macro_rules! impl_storage {
    ($($field:ident: $ty:ty = $default:expr;)*) => {
        pub fn load() -> anyhow::Result<Settings> {
            let connection = super::open_read()?;
            let values = load_values(&connection)?;
            Ok(Settings {
                $($field: read_setting(&values, stringify!($field), $default),)*
            })
        }

        pub fn save(settings: &Settings) -> anyhow::Result<()> {
            let mut connection = super::open()?;
            let transaction = connection.transaction()?;
            $(write_setting(&transaction, stringify!($field), &settings.$field)?;)*
            transaction.commit()?;
            Ok(())
        }
    };
}

crate::settings::fields!(impl_storage);

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
    SimplifiedChinese => "zh-hans",
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

impl Stored for GainPercent {
    fn read(values: &Values, key: &str) -> FromSqlResult<Option<Self>> {
        Ok(scalar::<u16>(values, key)?.map(GainPercent::new))
    }

    fn write(&self, transaction: &Transaction<'_>, key: &str) -> rusqlite::Result<()> {
        write_scalar(transaction, key, &self.as_u16())
    }
}
