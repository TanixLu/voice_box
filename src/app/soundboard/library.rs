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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_hotkeys_only_change_selected_sounds_and_roll_back_failed_saves() {
        let first = Hotkey {
            virtual_key_code: 0x75,
            modifiers: 0,
        };
        let second = Hotkey {
            virtual_key_code: 0x76,
            modifiers: 2,
        };
        let mut library = Library {
            sounds: (0..3)
                .map(|index| Sound {
                    path: PathBuf::from(format!("{index}.wav")),
                    hotkey: if index == 2 { None } else { Some(first) },
                    ..Sound::default()
                })
                .collect(),
        };
        let selected = HashSet::from([
            library.sounds[0].path.clone(),
            library.sounds[2].path.clone(),
        ]);
        assert!(
            library
                .set_hotkeys(&selected, Some(second), |_| anyhow::bail!("save failed"))
                .is_err()
        );
        assert_eq!(
            library
                .sounds
                .iter()
                .map(|sound| sound.hotkey)
                .collect::<Vec<_>>(),
            [Some(first), Some(first), None]
        );
        library
            .set_hotkeys(&selected, Some(second), |saved| {
                assert_eq!(
                    saved
                        .sounds
                        .iter()
                        .map(|sound| sound.hotkey)
                        .collect::<Vec<_>>(),
                    [Some(second), Some(first), Some(second)]
                );
                Ok(())
            })
            .unwrap();
        assert!(
            library
                .set_hotkeys(&selected, None, |_| anyhow::bail!("save failed"))
                .is_err()
        );
        assert_eq!(
            library
                .sounds
                .iter()
                .map(|sound| sound.hotkey)
                .collect::<Vec<_>>(),
            [Some(second), Some(first), Some(second)]
        );
        library.set_hotkeys(&selected, None, |_| Ok(())).unwrap();
        assert_eq!(
            library
                .sounds
                .iter()
                .map(|sound| sound.hotkey)
                .collect::<Vec<_>>(),
            [None, Some(first), None]
        );
        let invalid = HashSet::from([library.sounds[0].path.clone(), PathBuf::from("missing.wav")]);
        assert!(
            library
                .set_hotkeys(&invalid, Some(second), |_| panic!(
                    "must not save missing sounds"
                ))
                .is_err()
        );
        assert!(library.sounds[0].hotkey.is_none());
    }

    #[test]
    fn bulk_delete_keeps_unselected_and_failed_files() {
        let directory = std::env::temp_dir().join(format!(
            "voice-box-delete-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir(&directory).unwrap();
        let mut library = Library {
            sounds: (0..5)
                .map(|index| Sound {
                    file_name: format!("{index}.wav"),
                    path: directory.join(format!("{index}.wav")),
                    ..Sound::default()
                })
                .collect(),
        };
        for sound in &library.sounds[..3] {
            fs::write(&sound.path, b"audio").unwrap();
        }
        // A directory deterministically fails file deletion, without relying on permissions.
        fs::create_dir(&library.sounds[4].path).unwrap();
        let selected = [0, 2, 3, 4].map(|index| library.sounds[index].path.clone());
        let (removed, errors) = library.remove(&HashSet::from(selected.clone()));
        assert_eq!(removed.len(), 3);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            library
                .sounds
                .iter()
                .map(|sound| sound.file_name.as_str())
                .collect::<Vec<_>>(),
            ["1.wav", "4.wav"]
        );
        assert!(library.sounds[0].path.is_file());
        assert!(library.sounds[1].path.is_dir());
        assert!(selected[..3].iter().all(|path| !path.exists()));
        assert!(errors[0].to_string().contains("4.wav"));
        fs::remove_dir_all(directory).unwrap();
    }
}
