//! Application settings and immutable audio payloads share one transactional store.
use super::i18n::{Text, UserContext};
use crate::shared::paths;
use anyhow::{Context, ensure};
use rusqlite::{Connection, params};
use std::{
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::Mutex,
    time::Duration,
};
use symphonia::core::io::MediaSource;

pub(super) const AUDIO_CHUNK_BYTES: usize = 64 * 1024;

pub(super) fn open() -> anyhow::Result<Connection> {
    let directory = paths::data_dir()?;
    std::fs::create_dir_all(&directory).user_context(Text::CreateDataDirectory)?;
    open_at(&directory.join("voice_box.db")).user_context(Text::OpenDatabase)
}

pub(super) fn open_at(path: &Path) -> anyhow::Result<Connection> {
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
             content BLOB NOT NULL CHECK(length(content) BETWEEN 1 AND 65536),
             PRIMARY KEY(sound_id, chunk_index)
         ) STRICT, WITHOUT ROWID;",
    )?;
    Ok(connection)
}

pub(super) fn reclaim_space(connection: &Connection) -> anyhow::Result<()> {
    let free_pages: i64 =
        connection.pragma_query_value(None, "freelist_count", |row| row.get(0))?;
    if free_pages == 0 {
        tracing::info!("Database space reclamation skipped: no free pages");
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
    let page_size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
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

pub(super) struct AudioSource {
    connection: Mutex<Connection>,
    id: i64,
    length: u64,
    position: u64,
}

impl AudioSource {
    pub fn open(id: i64) -> anyhow::Result<Self> {
        Self::new(open()?, id)
    }

    pub fn new(connection: Connection, id: i64) -> anyhow::Result<Self> {
        let length = connection.query_row(
            "SELECT byte_length FROM sounds WHERE id = ?1",
            [id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            id,
            length: u64::try_from(length)?,
            position: 0,
        })
    }
}

impl Read for AudioSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let offset = self.position as usize % AUDIO_CHUNK_BYTES;
        let count = self
            .length
            .saturating_sub(self.position)
            .min(output.len().min(AUDIO_CHUNK_BYTES - offset) as u64) as usize;
        if count == 0 {
            return Ok(0);
        }
        let connection = self
            .connection
            .get_mut()
            .map_err(|_| io::Error::other("Audio connection poisoned"))?;
        // Each bounded query ends its snapshot, allowing writers and WAL checkpoints.
        let bytes: Vec<u8> = connection.prepare_cached(
            "SELECT substr(content, ?3 + 1, ?4) FROM sound_data WHERE sound_id = ?1 AND chunk_index = ?2",
        ).and_then(|mut query| query.query_row(
            params![self.id, (self.position / AUDIO_CHUNK_BYTES as u64) as i64, offset as i64, count as i64],
            |row| row.get(0),
        )).map_err(io::Error::other)?;
        if bytes.len() != count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Incomplete audio chunk",
            ));
        }
        output[..count].copy_from_slice(&bytes);
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for AudioSource {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.length.checked_add_signed(offset),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid audio offset"))?;
        self.position = position;
        Ok(position)
    }
}

impl MediaSource for AudioSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.length)
    }
}
