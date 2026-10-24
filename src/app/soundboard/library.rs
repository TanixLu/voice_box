use super::decoder::SoundDecoder;
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message, UserContext};
use crate::{app::hotkeys::Hotkey, shared::paths};
use anyhow::ensure;
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    path::PathBuf,
};

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(in crate::app) struct Sound {
    pub file_name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub hotkey: Option<Hotkey>,
    #[serde(skip)]
    pub duration: Option<f64>,
    #[serde(skip)]
    pub duration_error: Option<Message>,
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(in crate::app) struct Library {
    pub sounds: Vec<Sound>,
}

impl Library {
    pub(super) fn move_sounds(
        &mut self,
        paths: &HashSet<PathBuf>,
        to_top: bool,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<Vec<usize>> {
        ensure!(
            !paths.is_empty()
                && self
                    .sounds
                    .iter()
                    .filter(|sound| paths.contains(&sound.path))
                    .count()
                    == paths.len(),
            Text::SoundNotFound
        );
        let mut order: Vec<_> = (0..self.sounds.len()).collect();
        order.sort_by_key(|&index| paths.contains(&self.sounds[index].path) != to_top);
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
        paths: &HashSet<PathBuf>,
        key: Option<Hotkey>,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let indices: Vec<_> = self
            .sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| paths.contains(&sound.path))
            .map(|(index, _)| index)
            .collect();
        ensure!(
            !indices.is_empty() && indices.len() == paths.len(),
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
        let sound = self
            .sounds
            .get_mut(index)
            .user_context(Text::SoundNotFound)?;
        let file_name = name.trim();
        ensure!(is_valid_file_name(file_name), Text::InvalidFileName);
        if file_name == sound.file_name {
            return save(self);
        }
        let destination = sound.path.with_file_name(file_name);
        let move_file = super::rename::move_file;
        move_file(&sound.path, &destination)?;
        let original_path = std::mem::replace(&mut sound.path, destination);
        let original_name = std::mem::replace(&mut sound.file_name, file_name.to_owned());
        if let Err(error) = save(self) {
            let sound = &mut self.sounds[index];
            if let Err(rollback) = move_file(&sound.path, &original_path) {
                return Err(error.user_context(Message::RestoreFileName {
                    error: Details::new(rollback),
                    path: sound.path.display().to_string(),
                }));
            }
            sound.path = original_path;
            sound.file_name = original_name;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn remove(&mut self, paths: &HashSet<PathBuf>) -> (Vec<Sound>, Vec<Message>) {
        let mut removed = Vec::new();
        let mut errors = Vec::new();
        for index in (0..self.sounds.len()).rev() {
            let sound = &self.sounds[index];
            if !paths.contains(&sound.path) {
                continue;
            }
            if let Err(error) = fs::remove_file(&sound.path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                errors.push(Message::DeleteSoundFailed {
                    error: Details::new(anyhow::Error::new(error).context(sound.file_name.clone())),
                });
                continue;
            }
            removed.push(self.sounds.remove(index));
        }
        (removed, errors)
    }
}

pub(super) struct ImportedSounds {
    pub(super) sounds: Vec<Sound>,
    pub(super) errors: Vec<Message>,
}

pub(in crate::app) fn is_valid_file_name(name: &str) -> bool {
    if name.is_empty()
        || name.encode_utf16().count() > 255
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
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
    let directory = paths::sound_library_dir()?;
    fs::create_dir_all(&directory).user_context(Text::CreateSoundLibrary)?;
    let canonical_directory = directory.canonicalize()?;
    let mut seen = HashSet::new();
    for source in sources {
        let result = (|| -> anyhow::Result<Sound> {
            let source = source
                .canonicalize()
                .user_context(Text::ReadSourceAudioPath)?;
            ensure!(source.is_file(), Text::AudioIsNotFile);
            let original_name = source
                .file_name()
                .and_then(|name| name.to_str())
                .user_context(Text::InvalidAudioFileName)?;
            ensure!(
                is_valid_file_name(original_name),
                Text::InvalidAudioFileName
            );
            {
                let mut decoder = SoundDecoder::open(&source)?;
                ensure!(decoder.next_frame()?.is_some(), Text::NoPlayableAudio);
                while decoder.next_frame()?.is_some() {}
            }
            let (file_name, path) = if source.parent() == Some(canonical_directory.as_path()) {
                (original_name.to_owned(), directory.join(original_name))
            } else {
                let stem = source
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .user_context(Text::InvalidAudioFileName)?;
                let mut number = 1_u64;
                loop {
                    let mut file_name = if number == 1 {
                        stem.to_owned()
                    } else {
                        format!("{stem} ({number})")
                    };
                    if let Some(extension) = source.extension().and_then(|ext| ext.to_str()) {
                        file_name.push('.');
                        file_name.push_str(extension);
                    }
                    let destination = directory.join(&file_name);
                    match OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&destination)
                    {
                        Ok(mut output) => {
                            let copied = (|| -> std::io::Result<()> {
                                std::io::copy(&mut fs::File::open(&source)?, &mut output)?;
                                output.sync_all()
                            })();
                            drop(output);
                            if let Err(error) = copied {
                                let _ = fs::remove_file(&destination);
                                return Err(error).user_context(Text::CopySoundFile);
                            }
                            break (file_name, destination);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            number += 1
                        }
                        Err(error) => return Err(error).user_context(Text::CreateSoundFile),
                    }
                }
            };
            Ok(Sound {
                file_name,
                path,
                hotkey: None,
                duration: None,
                duration_error: None,
            })
        })();
        match result {
            Ok(sound) if seen.insert(sound.file_name.to_lowercase()) => imported.sounds.push(sound),
            Ok(_) => {}
            Err(error) => imported.errors.push(Message::ImportFileFailed {
                path: source.display().to_string(),
                error: Details::new(error),
            }),
        }
    }
    Ok(imported)
}
