use std::{
    collections::HashMap,
    fs::File,
    io::{self, Seek},
    path::Path,
};

use crate::{
    database::audio::AudioSource,
    i18n::{Message, Text, UserContext},
    soundboard::{decoder::open_format, sound::Sound},
};

const MAX_FILENAME_ATTEMPTS: usize = 10_000;

pub fn export_sounds(sounds: &[Sound], folder: &Path) -> anyhow::Result<()> {
    export_sounds_with(sounds, folder, |sound| AudioSource::open(sound.id))
}

fn export_sounds_with(
    sounds: &[Sound],
    folder: &Path,
    open_source: impl Fn(&Sound) -> anyhow::Result<AudioSource>,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let mut filenames = HashMap::<String, u64>::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        filenames.extend(
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .map(|name| (name, 1)),
        );
    }
    for sound in sounds {
        if let Err(error) = open_source(sound)
            .and_then(|source| export_sound(source, &sound.name, folder, &mut filenames))
            .with_user_context(|| Message::ExportSoundFailed {
                name: sound.name.clone(),
            })
        {
            errors.push(Message::error(error));
        }
    }
    if !errors.is_empty() {
        return Err(anyhow::anyhow!(Message::Many(errors)));
    }
    Ok(())
}

fn export_sound(
    source: AudioSource,
    name: &str,
    folder: &Path,
    filenames: &mut HashMap<String, u64>,
) -> anyhow::Result<()> {
    let format = open_format(Box::new(source))?;
    let extension = match format.format_info().short_name {
        "wave" => "wav",
        "mp3" => "mp3",
        "flac" => "flac",
        "ogg" => "ogg",
        _ => anyhow::bail!(Text::UnsupportedAudioFormat),
    };
    let mut source = format.into_inner();
    source.rewind()?;
    write_audio(&mut source, folder, &file_stem(name), extension, filenames)
}

fn file_stem(name: &str) -> String {
    // Leave room for the extension and collision suffix within Windows' filename limit.
    let name: String = name
        .chars()
        .take(100)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let name = name.trim_end_matches([' ', '.']);
    let device = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device
            .strip_prefix("COM")
            .or_else(|| device.strip_prefix("LPT"))
            .is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
    if name.is_empty() {
        "audio".into()
    } else if reserved {
        format!("_{name}")
    } else {
        name.to_owned()
    }
}

fn write_audio(
    source: &mut impl io::Read,
    folder: &Path,
    stem: &str,
    extension: &str,
    filenames: &mut HashMap<String, u64>,
) -> anyhow::Result<()> {
    let base = format!("{stem}.{extension}");
    let start = filenames.get(&base).copied().unwrap_or(0);
    for suffix in (start..=u64::MAX).take(MAX_FILENAME_ATTEMPTS) {
        let name = if suffix == 0 {
            base.clone()
        } else {
            format!("{stem} ({suffix}).{extension}")
        };
        if filenames.contains_key(&name) {
            continue;
        }
        let path = folder.join(&name);
        let mut file = match File::create_new(&path) {
            Ok(file) => file,
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    || path.symlink_metadata().is_ok() =>
            {
                filenames.entry(name).or_insert(1);
                continue;
            }
            Err(error) => {
                return Err(error).with_user_context(|| Message::WritePathFailed {
                    path: path.display().to_string(),
                });
            }
        };
        let result = io::copy(source, &mut file).and_then(|_| file.sync_all());
        if result.is_ok() {
            filenames.entry(name).or_insert(1);
            filenames.insert(base, suffix.saturating_add(1));
        }
        drop(file);
        if result.is_err()
            && let Err(error) = std::fs::remove_file(&path)
        {
            tracing::warn!(path = %path.display(), "Failed to remove incomplete export: {error}");
        }
        return result.with_user_context(|| Message::WritePathFailed {
            path: path.display().to_string(),
        });
    }
    anyhow::bail!(Text::ExportFilenameAttemptsExceeded)
}
