//! File validation and batch import; the caller owns the live library.

use std::{
    collections::HashSet,
    fs::File,
    io::{self, Seek},
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    thread,
};

use anyhow::ensure;
use rusqlite::{Connection, TransactionBehavior};

use voice_box_shared::pcm_transport::SAMPLE_RATE;

use crate::{
    database,
    i18n::{Details, Message, Text, UserContext},
    soundboard::{
        decoder::SoundDecoder,
        sound::{Sound, is_valid_name},
        storage::write_sound,
    },
};

pub struct ImportedSounds {
    pub sounds: Vec<Sound>,
    pub errors: Vec<Message>,
}

pub fn import_files(sources: Vec<PathBuf>) -> anyhow::Result<ImportedSounds> {
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
    duration_secs: f64,
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
                            let Some((index, source, path)) = pending
                                .lock()
                                .expect(
                                    "UNREACHABLE: pending audio imports mutex should not be poisoned",
                                )
                                .next()
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
        duration_secs: frames as f64 / f64::from(SAMPLE_RATE),
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
            file.duration_secs,
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
