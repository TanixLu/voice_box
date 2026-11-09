//! Application persistence. Connections, SQL and transactions stay inside this module.

pub mod audio;
mod codecs;
mod maintenance;
pub mod sounds;

use std::time::Duration;

use crate::{
    i18n::{Text, UserContext},
    paths::{data_dir, database_path},
};
use rusqlite::{Connection, OpenFlags};

pub fn init() -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    let mut connection =
        open_with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE)?;
    connection.execute_batch("PRAGMA journal_mode = WAL;")?;
    let transaction = connection.transaction()?;
    transaction.execute_batch(include_str!("schema.sql"))?;
    transaction.commit()?;
    Ok(())
}

fn open() -> anyhow::Result<Connection> {
    open_with_flags(OpenFlags::SQLITE_OPEN_READ_WRITE)
}

fn open_read() -> anyhow::Result<Connection> {
    open_with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn open_with_flags(flags: OpenFlags) -> anyhow::Result<Connection> {
    let connection =
        Connection::open_with_flags(database_path(), flags).user_context(Text::OpenDatabase)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    // Mapping hot pages avoids a separate Windows read call for every database page.
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA mmap_size = 268435456;")?;
    Ok(connection)
}
