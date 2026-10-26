use super::decoder::SoundDecoder;
use crate::app::{
    database,
    hotkeys::Hotkey,
    i18n::{Details, Message, Text, UserContext},
};
use crate::shared::pcm_transport::SAMPLE_RATE;
use anyhow::ensure;
use rusqlite::{
    Connection, TransactionBehavior, params,
    types::{FromSql, FromSqlError, FromSqlResult, ValueRef},
};
use std::{
    collections::HashSet,
    fs::File,
    io::{self, Seek, Write},
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    thread,
};

pub(in crate::app) type SoundId = i64;

#[derive(Clone, Debug)]
pub(in crate::app) struct Sound {
    pub id: SoundId,
    pub name: String,
    pub hotkey: Option<Hotkey>,
    pub duration: f64,
}

#[derive(Clone, Default)]
pub(in crate::app) struct Library {
    pub sounds: Vec<Sound>,
}

impl Library {
    pub(in crate::app) fn try_load() -> anyhow::Result<Self> {
        Self::load(&database::open()?)
    }

    pub(super) fn save(&self) -> anyhow::Result<()> {
        self.save_to(&mut database::open()?)
            .user_context(Text::SaveSoundLibrary)
    }

    fn save_to(&self, connection: &mut Connection) -> anyhow::Result<()> {
        let transaction = connection.transaction()?;
        {
            let mut update = transaction
                .prepare("UPDATE sounds SET name = ?1, position = ?2, hotkey = ?3 WHERE id = ?4")?;
            for (position, sound) in self.sounds.iter().enumerate() {
                ensure!(is_valid_name(&sound.name), Text::InvalidSoundName);
                ensure!(
                    update
                        .execute(params![sound.name, position as i64, sound.hotkey, sound.id,])?
                        == 1,
                    Text::SoundNotFound
                );
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub(super) fn move_sounds(
        &mut self,
        ids: &HashSet<SoundId>,
        to_top: bool,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<usize>> {
        ensure!(
            !ids.is_empty()
                && self
                    .sounds
                    .iter()
                    .filter(|sound| ids.contains(&sound.id))
                    .count()
                    == ids.len(),
            Text::SoundNotFound
        );
        let mut order: Vec<_> = (0..self.sounds.len()).collect();
        order.sort_by_key(|&index| ids.contains(&self.sounds[index].id) != to_top);
        let original = self.sounds.clone();
        self.sounds = order.iter().map(|&index| original[index].clone()).collect();
        if let Err(error) = save(self) {
            self.sounds = original;
            return Err(error);
        }
        Ok(order)
    }

    pub(super) fn move_sound(
        &mut self,
        from: usize,
        to: usize,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        ensure!(
            from < self.sounds.len() && to < self.sounds.len(),
            Text::SoundNotFound
        );
        if from == to {
            return Ok(());
        }
        let sound = self.sounds.remove(from);
        self.sounds.insert(to, sound);
        if let Err(error) = save(self) {
            let sound = self.sounds.remove(to);
            self.sounds.insert(from, sound);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn set_hotkeys(
        &mut self,
        ids: &HashSet<SoundId>,
        key: Option<Hotkey>,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let indices: Vec<_> = self
            .sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| ids.contains(&sound.id))
            .map(|(index, _)| index)
            .collect();
        ensure!(
            !indices.is_empty() && indices.len() == ids.len(),
            Text::SoundNotFound
        );
        let previous: Vec<_> = indices
            .iter()
            .map(|&index| std::mem::replace(&mut self.sounds[index].hotkey, key))
            .collect();
        if let Err(error) = save(self) {
            for (index, key) in indices.into_iter().zip(previous) {
                self.sounds[index].hotkey = key;
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn rename(
        &mut self,
        index: usize,
        name: &str,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let name = name.trim();
        ensure!(is_valid_name(name), Text::InvalidSoundName);
        let sound = self
            .sounds
            .get_mut(index)
            .user_context(Text::SoundNotFound)?;
        let original = std::mem::replace(&mut sound.name, name.to_owned());
        if let Err(error) = save(self) {
            self.sounds[index].name = original;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn remove(
        &mut self,
        connection: &mut Connection,
        ids: &HashSet<SoundId>,
        reclaim_space: bool,
    ) -> anyhow::Result<()> {
        let transaction = connection.transaction()?;
        {
            let mut delete = transaction.prepare("DELETE FROM sounds WHERE id = ?1")?;
            for id in ids {
                delete.execute([id])?;
            }
        }
        transaction.commit()?;
        self.sounds.retain(|sound| !ids.contains(&sound.id));
        if reclaim_space {
            // Maintenance errors must not turn a committed deletion into a failure.
            if let Err(error) = database::reclaim_space(connection) {
                tracing::warn!("Database space reclamation incomplete: {error:#}");
            }
        } else {
            tracing::info!("Database space reclamation skipped: playback is still active");
        }
        Ok(())
    }

    pub(in crate::app) fn load(connection: &Connection) -> anyhow::Result<Self> {
        let mut query = connection
            .prepare("SELECT id, name, duration, hotkey FROM sounds ORDER BY position, id")?;
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
            let (id, name, duration, hotkey) = row?;
            ensure!(is_valid_name(&name), Text::InvalidSoundName);
            ensure!(
                duration.is_finite() && duration > 0.0,
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
                duration,
                hotkey,
            });
        }
        Ok(Self { sounds })
    }
}

fn read_hotkey(value: ValueRef<'_>) -> FromSqlResult<Option<Hotkey>> {
    let hotkey = Option::<Hotkey>::column_result(value)?;
    match hotkey {
        Some(key) if !key.is_valid() => Err(FromSqlError::Other("invalid sound hotkey".into())),
        _ => Ok(hotkey),
    }
}

pub(super) struct ImportedSounds {
    pub(super) sounds: Vec<Sound>,
    pub(super) errors: Vec<Message>,
}

pub(in crate::app) fn is_valid_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.chars().any(char::is_control)
}

struct AudioWriter<'a> {
    connection: &'a Connection,
    id: SoundId,
    chunk: i64,
    buffer: Vec<u8>,
    written: u64,
    length: u64,
}

impl Write for AudioWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes
            .len()
            .min(database::AUDIO_CHUNK_BYTES - self.buffer.len())
            .min((self.length - self.written) as usize);
        self.buffer.extend_from_slice(&bytes[..count]);
        self.written += count as u64;
        if self.buffer.len() == database::AUDIO_CHUNK_BYTES {
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

pub(in crate::app) fn insert_sound(
    connection: &mut Connection,
    name: &str,
    duration: f64,
    length: u64,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<Sound> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let sound = write_sound(&transaction, name, duration, length, write)?;
    transaction.commit()?;
    Ok(sound)
}

fn write_sound(
    connection: &Connection,
    name: &str,
    duration: f64,
    length: u64,
    write: impl FnOnce(&mut dyn Write) -> anyhow::Result<()>,
) -> anyhow::Result<Sound> {
    let name = name.trim();
    ensure!(is_valid_name(name), Text::InvalidSoundName);
    ensure!(length > 0, Text::NoPlayableAudio);
    ensure!(
        duration.is_finite() && duration > 0.0,
        Text::InvalidAudioDuration
    );
    connection.prepare_cached(
        "INSERT INTO sounds(name, duration, byte_length, position) SELECT ?1, ?2, ?3, COALESCE(MAX(position) + 1, 0) FROM sounds",
    )?.execute(params![name, duration, i64::try_from(length)?])?;
    let id = connection.last_insert_rowid();
    {
        let mut output = AudioWriter {
            connection,
            id,
            chunk: 0,
            buffer: Vec::with_capacity(database::AUDIO_CHUNK_BYTES),
            written: 0,
            length,
        };
        write(&mut output)?;
        output.flush()?;
        ensure!(output.written == length, Text::AudioChangedDuringImport);
    }
    Ok(Sound {
        id,
        name: name.to_owned(),
        duration,
        hotkey: None,
    })
}

pub(super) fn import_files(sources: Vec<PathBuf>) -> anyhow::Result<ImportedSounds> {
    let (files, mut imported) = validate_files(sources)?;
    if !files.is_empty() {
        let result = database::open().and_then(|mut connection| {
            connection.busy_timeout(std::time::Duration::ZERO)?;
            connection.pragma_update(None, "cache_size", -32768)?;
            insert_files(&mut connection, files)
        });
        match result {
            Ok(sounds) => imported.sounds = sounds,
            Err(error) => imported.errors.push(Message::ImportSoundsFailed {
                error: Details::new(error),
            }),
        }
    }
    Ok(imported)
}

struct ValidatedFile {
    file: File,
    source: PathBuf,
    name: String,
    duration: f64,
    length: u64,
}

fn validate_files(sources: Vec<PathBuf>) -> anyhow::Result<(Vec<ValidatedFile>, ImportedSounds)> {
    let mut imported = ImportedSounds {
        sounds: Vec::new(),
        errors: Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut jobs = Vec::new();
    for (index, source) in sources.into_iter().enumerate() {
        let path = source
            .canonicalize()
            .user_context(Text::ReadSourceAudioPath);
        if path.as_ref().is_ok_and(|path| !seen.insert(path.clone())) {
            continue;
        }
        jobs.push((index, source, path));
    }
    let workers = thread::available_parallelism()
        .map_or(1, |count| count.get().saturating_sub(1).max(1))
        .min(jobs.len());
    let pending = Mutex::new(jobs.into_iter());
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| -> anyhow::Result<()> {
        let mut threads = Vec::new();
        for _ in 0..workers {
            threads.push(
                thread::Builder::new()
                    .name("VoiceBoxImportDecoder".into())
                    .spawn_scoped(scope, || {
                        loop {
                            let Some((index, source, path)) =
                                pending.lock().expect("pending audio imports").next()
                            else {
                                break;
                            };
                            let result = path.and_then(|path| validate_file(&path));
                            let _ = sender.send((index, source, result));
                        }
                    })?,
            );
        }
        for worker in threads {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!(Text::ImportThreadExited))?;
        }
        Ok(())
    })?;
    drop(sender);
    let mut results: Vec<_> = receiver.into_iter().collect();
    results.sort_unstable_by_key(|(index, _, _)| *index);
    let mut files = Vec::new();
    for (_, source, result) in results {
        match result {
            Ok(file) => files.push(file),
            Err(error) => imported.errors.push(Message::ImportFileFailed {
                path: source.display().to_string(),
                error: Details::new(error),
            }),
        }
    }
    Ok((files, imported))
}

fn validate_file(source: &Path) -> anyhow::Result<ValidatedFile> {
    let mut file = File::open(source).user_context(Text::OpenAudioFile)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), Text::AudioIsNotFile);
    let name = source
        .file_stem()
        .user_context(Text::InvalidAudioFileName)?
        .to_string_lossy()
        .trim()
        .to_owned();
    ensure!(is_valid_name(&name), Text::InvalidSoundName);
    let mut decoder = SoundDecoder::new(Box::new(file.try_clone()?))?;
    let mut frames = 0_u64;
    while decoder.next_frame()?.is_some() {
        frames += 1;
    }
    ensure!(frames > 0, Text::NoPlayableAudio);
    file.rewind()?;
    ensure!(metadata.len() > 0, Text::NoPlayableAudio);
    Ok(ValidatedFile {
        file,
        source: source.to_owned(),
        name,
        duration: frames as f64 / f64::from(SAMPLE_RATE),
        length: metadata.len(),
    })
}

fn insert_files(
    connection: &mut Connection,
    files: Vec<ValidatedFile>,
) -> anyhow::Result<Vec<Sound>> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut sounds = Vec::with_capacity(files.len());
    for mut file in files {
        let sound = write_sound(
            &transaction,
            &file.name,
            file.duration,
            file.length,
            |output| {
                io::copy(&mut file.file, output)?;
                Ok(())
            },
        )
        .map_err(|error| {
            anyhow::anyhow!(Message::ImportFileFailed {
                path: file.source.display().to_string(),
                error: Details::new(error),
            })
        })?;
        sounds.push(sound);
    }
    transaction.commit()?;
    Ok(sounds)
}
