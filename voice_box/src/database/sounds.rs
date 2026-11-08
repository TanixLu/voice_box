//! Sound metadata and atomic creation of metadata with audio payloads.

use std::{collections::HashSet, io::Write};

use anyhow::ensure;
use rusqlite::{
    Transaction, TransactionBehavior, params,
    types::{FromSql, FromSqlError, FromSqlResult, ValueRef},
};

use crate::{
    hotkeys::Hotkey,
    i18n::Text,
    soundboard::sound::{Sound, SoundId, is_valid_name},
};

use super::audio::AudioWriter;

pub fn save(sounds: &[Sound]) -> anyhow::Result<()> {
    let mut connection = super::open()?;
    let transaction = connection.transaction()?;
    {
        let mut update = transaction
            .prepare("UPDATE sounds SET name = ?1, position = ?2, hotkey = ?3 WHERE id = ?4")?;
        for (position, sound) in sounds.iter().enumerate() {
            ensure!(is_valid_name(&sound.name), Text::InvalidSoundName);
            ensure!(
                update.execute(params![sound.name, position as i64, sound.hotkey, sound.id,])? == 1,
                Text::SoundNotFound
            );
        }
    }
    transaction.commit()?;
    Ok(())
}

pub fn remove(ids: &HashSet<SoundId>, reclaim_space: bool) -> anyhow::Result<()> {
    let mut connection = super::open()?;
    let transaction = connection.transaction()?;
    {
        let mut delete = transaction.prepare("DELETE FROM sounds WHERE id = ?1")?;
        for id in ids {
            delete.execute([id])?;
        }
    }
    transaction.commit()?;
    if reclaim_space {
        // Maintenance errors must not turn a committed deletion into a failure.
        if let Err(error) = super::maintenance::reclaim_space(&connection) {
            tracing::warn!("Database space reclamation incomplete: {error:#}");
        }
    } else {
        tracing::info!("Database space reclamation skipped: playback is still active");
    }
    Ok(())
}

pub fn load() -> anyhow::Result<Vec<Sound>> {
    let connection = super::open_read()?;
    let mut query = connection
        .prepare("SELECT id, name, duration_secs, hotkey FROM sounds ORDER BY position, id")?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
            read_hotkey(row.get_ref(3)?),
        ))
    })?;
    let mut sounds = Vec::new();
    for row in rows {
        let (id, name, duration_secs, hotkey) = row?;
        ensure!(is_valid_name(&name), Text::InvalidSoundName);
        ensure!(
            duration_secs.is_finite() && duration_secs > 0.0,
            Text::InvalidAudioDuration
        );
        let hotkey = match hotkey {
            Ok(hotkey) => hotkey,
            Err(error) => {
                tracing::warn!(sound_id = id, %error, "Invalid sound hotkey; clearing binding");
                None
            }
        };
        sounds.push(Sound {
            id,
            name,
            duration_secs,
            hotkey,
        });
    }
    Ok(sounds)
}

fn read_hotkey(value: ValueRef<'_>) -> FromSqlResult<Option<Hotkey>> {
    let hotkey = Option::<Hotkey>::column_result(value)?;
    match hotkey {
        Some(key) if !key.is_valid() => Err(FromSqlError::Other("invalid sound hotkey".into())),
        _ => Ok(hotkey),
    }
}

pub fn insert(
    name: &str,
    duration_secs: f64,
    length: u64,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<Sound> {
    let mut connection = super::open()?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let sound = write_sound(&transaction, name, duration_secs, length, write)?;
    transaction.commit()?;
    Ok(sound)
}

/// A batch can add sounds but cannot commit partial imports or access the connection.
pub struct SoundBatch<'a> {
    transaction: Transaction<'a>,
}

impl SoundBatch<'_> {
    pub fn insert(
        &mut self,
        name: &str,
        duration_secs: f64,
        length: u64,
        write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
    ) -> anyhow::Result<Sound> {
        write_sound(&self.transaction, name, duration_secs, length, write)
    }
}

pub fn insert_batch<T>(
    write: impl FnOnce(&mut SoundBatch<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let mut connection = super::open()?;
    connection.busy_timeout(std::time::Duration::ZERO)?;
    connection.pragma_update(None, "cache_size", -32768)?;
    let mut batch = SoundBatch {
        transaction: connection.transaction_with_behavior(TransactionBehavior::Immediate)?,
    };
    let result = write(&mut batch)?;
    batch.transaction.commit()?;
    Ok(result)
}

fn write_sound(
    transaction: &Transaction<'_>,
    name: &str,
    duration_secs: f64,
    length: u64,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<Sound> {
    let name = name.trim();
    ensure!(is_valid_name(name), Text::InvalidSoundName);
    ensure!(length > 0, Text::NoPlayableAudio);
    ensure!(
        duration_secs.is_finite() && duration_secs > 0.0,
        Text::InvalidAudioDuration
    );
    transaction.prepare_cached(
        "INSERT INTO sounds(name, duration_secs, byte_length, position) SELECT ?1, ?2, ?3, COALESCE(MAX(position) + 1, 0) FROM sounds",
    )?.execute(params![name, duration_secs, i64::try_from(length)?])?;
    let id = transaction.last_insert_rowid();
    {
        let mut output = AudioWriter::new(transaction, id, length);
        write(&mut output)?;
        output.finish()?;
    }
    Ok(Sound {
        id,
        name: name.to_owned(),
        duration_secs,
        hotkey: None,
    })
}
