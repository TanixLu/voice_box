use super::decoder::SoundDecoder;
use crate::app::{
    database,
    hotkeys::Hotkey,
    i18n::{Details, Message, Text, UserContext},
};
use crate::shared::pcm_transport::SAMPLE_RATE;
use anyhow::ensure;
use rusqlite::{Connection, TransactionBehavior, params};
use std::{
    collections::HashSet,
    fs::File,
    io::{self, Seek, Write},
    path::{Path, PathBuf},
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
    ) -> anyhow::Result<()> {
        let transaction = connection.transaction()?;
        for id in ids {
            transaction.execute("DELETE FROM sounds WHERE id = ?1", [id])?;
        }
        transaction.commit()?;
        self.sounds.retain(|sound| !ids.contains(&sound.id));
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
                row.get::<_, Option<String>>(3)?,
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
            sounds.push(Sound {
                id,
                name,
                duration,
                hotkey: hotkey
                    .map(|value| serde_json::from_str::<Hotkey>(&value))
                    .transpose()?
                    .filter(|key| key.is_valid()),
            });
        }
        Ok(Self { sounds })
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
    let name = name.trim();
    ensure!(is_valid_name(name), Text::InvalidSoundName);
    ensure!(length > 0, Text::NoPlayableAudio);
    ensure!(
        duration.is_finite() && duration > 0.0,
        Text::InvalidAudioDuration
    );
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO sounds(name, duration, byte_length, position) SELECT ?1, ?2, ?3, COALESCE(MAX(position) + 1, 0) FROM sounds",
        params![name, duration, i64::try_from(length)?],
    )?;
    let id = transaction.last_insert_rowid();
    {
        let mut output = AudioWriter {
            connection: &transaction,
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
    transaction.commit()?;
    Ok(Sound {
        id,
        name: name.to_owned(),
        duration,
        hotkey: None,
    })
}

pub(super) fn import_files(sources: Vec<PathBuf>) -> anyhow::Result<ImportedSounds> {
    let mut imported = ImportedSounds {
        sounds: Vec::new(),
        errors: Vec::new(),
    };
    if sources.is_empty() {
        return Ok(imported);
    }
    let mut connection = database::open()?;
    let mut seen = HashSet::new();
    for source in sources {
        let result = (|| -> anyhow::Result<Option<Sound>> {
            let source = source
                .canonicalize()
                .user_context(Text::ReadSourceAudioPath)?;
            if !seen.insert(source.clone()) {
                return Ok(None);
            }
            import_file(&mut connection, &source).map(Some)
        })();
        match result {
            Ok(Some(sound)) => imported.sounds.push(sound),
            Ok(None) => {}
            Err(error) => imported.errors.push(Message::ImportFileFailed {
                path: source.display().to_string(),
                error: Details::new(error),
            }),
        }
    }
    Ok(imported)
}

fn import_file(connection: &mut Connection, source: &Path) -> anyhow::Result<Sound> {
    let mut file = File::open(source).user_context(Text::OpenAudioFile)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), Text::AudioIsNotFile);
    let name = source
        .file_stem()
        .user_context(Text::InvalidAudioFileName)?
        .to_string_lossy();
    let mut decoder = SoundDecoder::new(Box::new(file.try_clone()?))?;
    let mut frames = 0_u64;
    while decoder.next_frame()?.is_some() {
        frames += 1;
    }
    ensure!(frames > 0, Text::NoPlayableAudio);
    file.rewind()?;
    insert_sound(
        connection,
        &name,
        frames as f64 / f64::from(SAMPLE_RATE),
        metadata.len(),
        |output| {
            io::copy(&mut file, output)?;
            Ok(())
        },
    )
}
