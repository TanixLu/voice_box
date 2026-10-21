use crate::{app::hotkeys::Hotkey, shared::paths};
use anyhow::{Context, ensure};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(in crate::app) struct Sound {
    pub file_name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub hotkey: Option<Hotkey>,
    #[serde(skip)]
    pub duration: Option<f64>,
    #[serde(skip)]
    pub duration_error: Option<String>,
}
impl Sound {
    pub fn name(&self) -> &str {
        Path::new(&self.file_name)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or(&self.file_name)
    }
}
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub(in crate::app) struct Library {
    pub sounds: Vec<Sound>,
}

pub(super) struct ImportedSounds {
    pub(super) sounds: Vec<Sound>,
    pub(super) errors: Vec<String>,
}

pub(super) fn valid_file_name(name: &str) -> bool {
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
    fs::create_dir_all(&directory).context("创建音效库目录失败")?;
    let canonical_directory = directory.canonicalize()?;
    let mut seen = HashSet::new();
    for source in sources {
        let result = (|| -> anyhow::Result<Sound> {
            let source = source.canonicalize().context("读取原音频路径失败")?;
            ensure!(source.is_file(), "所选音频不是文件");
            let original_name = source
                .file_name()
                .and_then(|name| name.to_str())
                .context("音频文件名无效")?;
            ensure!(valid_file_name(original_name), "音频文件名无效");
            let (file_name, path) = if source.parent() == Some(canonical_directory.as_path()) {
                (original_name.to_owned(), directory.join(original_name))
            } else {
                let stem = source
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .context("音频文件名无效")?;
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
                                return Err(error).context("复制音频到音效库失败");
                            }
                            break (file_name, destination);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            number += 1
                        }
                        Err(error) => return Err(error).context("创建音效库文件失败"),
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
            Err(error) => imported
                .errors
                .push(format!("{}：{error:#}", source.display())),
        }
    }
    Ok(imported)
}
