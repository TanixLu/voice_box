use super::decoder::SoundDecoder;
use crate::app::i18n::Text;
use crate::app::i18n::{Details, Message, UserContext};
use crate::{app::hotkeys::Hotkey, shared::paths};
use anyhow::ensure;
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
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

    pub(super) fn set_hotkey(
        &mut self,
        index: usize,
        key: Option<Hotkey>,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let sound = self
            .sounds
            .get_mut(index)
            .user_context(Text::SoundNotFound)?;
        let previous = std::mem::replace(&mut sound.hotkey, key);
        if let Err(error) = save(self) {
            self.sounds[index].hotkey = previous;
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
        // Do not replace an existing library file when names collide.
        let move_file = |from: &Path, to: &Path| unsafe {
            windows::Win32::Storage::FileSystem::MoveFileExW(
                windows::core::PCWSTR(crate::shared::utils::wide_null(from).as_ptr()),
                windows::core::PCWSTR(crate::shared::utils::wide_null(to).as_ptr()),
                windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(0),
            )
        };
        move_file(&sound.path, &destination).user_context(Text::RenameSoundFailed)?;
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

    pub(super) fn remove(&mut self, index: usize) -> anyhow::Result<Sound> {
        let sound = self.sounds.get(index).user_context(Text::SoundNotFound)?;
        if let Err(error) = fs::remove_file(&sound.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error.into());
        }
        Ok(self.sounds.remove(index))
    }
}

pub(super) struct ImportedSounds {
    pub(super) sounds: Vec<Sound>,
    pub(super) errors: Vec<Message>,
}

pub(in crate::app) fn is_valid_file_name(name: &str) -> bool {
    if name.is_empty()
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
