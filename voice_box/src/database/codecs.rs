//! SQL representation shared by settings and sound metadata.

use rusqlite::{
    ToSql,
    types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, Value, ValueRef},
};

use crate::hotkeys::Hotkey;

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
