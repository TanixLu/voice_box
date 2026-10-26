//! Chunked audio byte streams. Transaction ownership stays with the caller.
use crate::i18n::Text;
use anyhow::ensure;
use rusqlite::{Connection, params};
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    sync::Mutex,
};
use symphonia::core::io::MediaSource;

const AUDIO_CHUNK_BYTES: usize = 64 * 1024;

pub(crate) struct AudioSource {
    connection: Mutex<Connection>,
    id: i64,
    length: u64,
    position: u64,
}

impl AudioSource {
    pub fn open(id: i64) -> anyhow::Result<Self> {
        Self::new(super::open()?, id)
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

pub(crate) struct AudioWriter<'a> {
    connection: &'a Connection,
    id: i64,
    chunk: i64,
    buffer: Vec<u8>,
    written: u64,
    length: u64,
}

impl Write for AudioWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes
            .len()
            .min(AUDIO_CHUNK_BYTES - self.buffer.len())
            .min((self.length - self.written) as usize);
        self.buffer.extend_from_slice(&bytes[..count]);
        self.written += count as u64;
        if self.buffer.len() == AUDIO_CHUNK_BYTES {
            self.flush()?;
            self.buffer.clear();
            self.chunk += 1;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            self.connection
                .prepare_cached(
                    "INSERT INTO sound_data(sound_id, chunk_index, content) VALUES (?1, ?2, ?3)
                 ON CONFLICT(sound_id, chunk_index) DO UPDATE SET content = excluded.content",
                )
                .and_then(|mut insert| insert.execute(params![self.id, self.chunk, self.buffer]))
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}

impl<'a> AudioWriter<'a> {
    pub(crate) fn new(connection: &'a Connection, id: i64, length: u64) -> Self {
        Self {
            connection,
            id,
            chunk: 0,
            buffer: Vec::with_capacity(AUDIO_CHUNK_BYTES),
            written: 0,
            length,
        }
    }

    pub(crate) fn finish(mut self) -> anyhow::Result<()> {
        self.flush()?;
        ensure!(self.written == self.length, Text::AudioChangedDuringImport);
        Ok(())
    }
}
