//! Application settings and immutable audio payloads share one transactional store.

pub mod audio;
mod codecs;

use std::{io, path::Path, time::Duration};

use anyhow::{Context, ensure};
use rusqlite::Connection;

use voice_box_shared::paths::data_dir;

use crate::{
    i18n::{Text, UserContext},
    utils::database_path,
};

const MIN_RECLAIM_BYTES: i64 = 64 * 1024 * 1024;
const MIN_RECLAIM_RATIO: f64 = 0.25;

pub fn open() -> anyhow::Result<Connection> {
    std::fs::create_dir_all(data_dir()?).user_context(Text::CreateDataDirectory)?;
    open_at(database_path()?).user_context(Text::OpenDatabase)
}

pub fn open_at(path: &Path) -> anyhow::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    // Mapping hot pages avoids a separate Windows read call for every database page.
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA mmap_size = 268435456;
         CREATE TABLE IF NOT EXISTS settings (
             key TEXT PRIMARY KEY,
             value ANY NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS sounds (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT NOT NULL CHECK(length(trim(name)) > 0),
             duration_secs REAL NOT NULL CHECK(duration_secs > 0),
             byte_length INTEGER NOT NULL CHECK(byte_length > 0),
             position INTEGER NOT NULL,
             hotkey TEXT
         ) STRICT;
         CREATE INDEX IF NOT EXISTS sounds_position ON sounds(position);
         CREATE TABLE IF NOT EXISTS sound_data (
             sound_id INTEGER NOT NULL REFERENCES sounds(id) ON DELETE CASCADE,
             chunk_index INTEGER NOT NULL CHECK(chunk_index >= 0),
             content BLOB NOT NULL CHECK(length(content) BETWEEN 1 AND 32768),
             PRIMARY KEY(sound_id, chunk_index)
         ) STRICT, WITHOUT ROWID;",
    )?;
    Ok(connection)
}

pub fn reclaim_space(connection: &Connection) -> anyhow::Result<()> {
    let free_pages: i64 =
        connection.pragma_query_value(None, "freelist_count", |row| row.get(0))?;
    if free_pages == 0 {
        tracing::info!("Database space reclamation skipped: no free pages");
        return Ok(());
    }
    let page_count: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0))?;
    let page_size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
    let free_bytes = free_pages
        .checked_mul(page_size)
        .context("Database size overflow")?;
    let free_ratio = free_pages as f64 / page_count as f64;
    if (free_bytes < MIN_RECLAIM_BYTES || free_ratio < MIN_RECLAIM_RATIO)
        && connection.query_row("SELECT EXISTS(SELECT 1 FROM sounds)", [], |row| {
            row.get::<_, bool>(0)
        })?
    {
        tracing::info!(
            free_bytes,
            free_ratio,
            "Database space reclamation skipped: below thresholds"
        );
        return Ok(());
    }
    let path = Path::new(connection.path().context("Database has no file path")?);
    let before = storage_size(path)?;
    // Only this short-lived deletion connection gives up its Windows mapping.
    connection.pragma_update(None, "mmap_size", 0)?;
    let mmap_size: i64 = connection.pragma_query_value(None, "mmap_size", |row| row.get(0))?;
    ensure!(mmap_size == 0, "Database mapping is still in use");
    connection.execute_batch("VACUUM;")?;
    let (busy, log, checkpointed) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
    let after = storage_size(path)?;
    let bytes_before = before
        .0
        .checked_add(before.1)
        .context("Database size overflow")?;
    let bytes_after = after
        .0
        .checked_add(after.1)
        .context("Database size overflow")?;
    tracing::info!(
        free_pages,
        bytes_saved = bytes_before.saturating_sub(bytes_after),
        db_bytes_before = before.0,
        wal_bytes_before = before.1,
        db_bytes_after = after.0,
        wal_bytes_after = after.1,
        busy,
        log,
        checkpointed,
        "Database space reclamation checked"
    );
    ensure!(
        busy == 0,
        "Database checkpoint was blocked by another connection"
    );
    let page_count: i64 = connection.pragma_query_value(None, "page_count", |row| row.get(0))?;
    let expected_bytes = u64::try_from(
        page_count
            .checked_mul(page_size)
            .context("Database size overflow")?,
    )?;
    ensure!(
        after.0 <= expected_bytes && after.1 == 0,
        "Database files were not fully truncated; another connection may still hold a mapping"
    );
    Ok(())
}

fn storage_size(path: &Path) -> io::Result<(u64, u64)> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let wal_bytes = match std::fs::metadata(wal) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    Ok((std::fs::metadata(path)?.len(), wal_bytes))
}
