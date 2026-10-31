//! Transactional creation of sound metadata and immutable audio payloads.

use std::io::Write;

use anyhow::ensure;
use rusqlite::{Connection, TransactionBehavior, params};

use crate::{
    database::audio::AudioWriter,
    i18n::Text,
    soundboard::sound::{Sound, is_valid_name},
};

pub fn insert_sound(
    connection: &mut Connection,
    name: &str,
    duration_secs: f64,
    length: u64,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<Sound> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let sound = write_sound(&transaction, name, duration_secs, length, write)?;
    transaction.commit()?;
    Ok(sound)
}

pub fn write_sound(
    connection: &Connection,
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
    connection.prepare_cached(
        "INSERT INTO sounds(name, duration_secs, byte_length, position) SELECT ?1, ?2, ?3, COALESCE(MAX(position) + 1, 0) FROM sounds",
    )?.execute(params![name, duration_secs, i64::try_from(length)?])?;
    let id = connection.last_insert_rowid();
    {
        let mut output = AudioWriter::new(connection, id, length);
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
