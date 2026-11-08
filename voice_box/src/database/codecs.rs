//! SQL encoding and decoding for custom application types.

use rusqlite::{
    ToSql,
    types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Value, ValueRef},
};

use crate::{hotkeys::Hotkey, settings::OutputDevice};

fn invalid(message: &str) -> FromSqlError {
    FromSqlError::Other(message.into())
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
