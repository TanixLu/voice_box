use super::library::{Library, is_valid_file_name};
use crate::app::i18n::{Details, Language, Message, Text, UserContext, current_language};
use anyhow::ensure;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    ops::Range,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering as AtomicOrdering},
    time::SystemTime,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum RenameLocation {
    Start,
    End,
    Entire,
    Matches,
}

impl RenameLocation {
    pub fn label(self) -> Text {
        match self {
            Self::Start => Text::RenameStart,
            Self::End => Text::RenameEnd,
            Self::Matches => Text::RenameMatches,
            Self::Entire => Text::RenameEntire,
        }
    }

    pub fn default_content(self) -> String {
        self.default_content_in_language(current_language())
    }

    fn default_content_in_language(self, language: Language) -> String {
        let sequence = Text::RenameSequence.in_language(language);
        match self {
            Self::Start => format!("{sequence}_"),
            Self::End => format!("_{sequence}"),
            Self::Matches => String::new(),
            Self::Entire => format!("{}_{sequence}", Text::RenameSoundName.in_language(language)),
        }
    }
}

pub(in crate::app) struct RenameRule {
    pub location: RenameLocation,
    pub find: String,
    pub content: String,
}

impl Default for RenameRule {
    fn default() -> Self {
        Self {
            location: RenameLocation::Start,
            find: String::new(),
            content: RenameLocation::Start.default_content(),
        }
    }
}

pub(in crate::app) struct RenameEntry {
    pub index: usize,
    pub original: PathBuf,
    pub name: String,
    pub old_marks: Vec<Range<usize>>,
    pub new_marks: Vec<Range<usize>>,
    pub error: Option<Text>,
}

impl RenameEntry {
    pub fn changed(&self) -> bool {
        self.original
            .file_name()
            .is_some_and(|name| name != self.name.as_str())
    }
}

pub(in crate::app) struct RenamePlan {
    pub entries: Vec<RenameEntry>,
}

impl RenamePlan {
    pub fn new(library: &Library, selected: &HashSet<PathBuf>, rule: &RenameRule) -> Self {
        let width = selected.len().max(1).ilog10().saturating_add(1).max(3) as usize;
        let mut entries = Vec::with_capacity(selected.len());
        for (index, sound) in library.sounds.iter().enumerate() {
            if !selected.contains(&sound.path) {
                continue;
            }
            let path = Path::new(&sound.file_name);
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let extension = &sound.file_name[stem.len()..];
            let sequence = format!("{:0width$}", entries.len() + 1);
            let replacement = [Language::English, Language::SimplifiedChinese]
                .into_iter()
                .fold(rule.content.clone(), |content, language| {
                    content.replace(Text::RenameSequence.in_language(language), &sequence)
                });
            let old_marks = match rule.location {
                RenameLocation::Start => std::iter::once(0..0).collect(),
                RenameLocation::End => std::iter::once(stem.len()..stem.len()).collect(),
                RenameLocation::Entire => std::iter::once(0..stem.len()).collect(),
                RenameLocation::Matches if !rule.find.is_empty() => stem
                    .match_indices(&rule.find)
                    .map(|(start, text)| start..start + text.len())
                    .collect(),
                RenameLocation::Matches => Vec::new(),
            };
            let mut name = String::new();
            let mut new_marks = Vec::with_capacity(old_marks.len());
            let mut end = 0;
            for range in &old_marks {
                name.push_str(&stem[end..range.start]);
                let start = name.len();
                name.push_str(&replacement);
                new_marks.push(start..name.len());
                end = range.end;
            }
            name.push_str(&stem[end..]);
            let valid_stem = is_valid_file_name(&name);
            name.push_str(extension);
            let error = if !valid_stem || !is_valid_file_name(&name) {
                Some(Text::InvalidFileName)
            } else if name.encode_utf16().count() > 255
                || sound
                    .path
                    .with_file_name(&name)
                    .as_os_str()
                    .encode_wide()
                    .count()
                    >= 260
            {
                Some(Text::RenameNameTooLong)
            } else {
                None
            };
            entries.push(RenameEntry {
                index,
                original: sound.path.clone(),
                name,
                old_marks,
                new_marks,
                error,
            });
        }
        Self { entries }
    }

    pub fn validate(&mut self, library: &Library, directory: &DirectorySnapshot) {
        let originals: BTreeSet<_> = self
            .entries
            .iter()
            .map(|entry| NameKey::new(entry.original.file_name().unwrap()))
            .collect();
        let library_names: BTreeSet<_> = library
            .sounds
            .iter()
            .map(|sound| NameKey::new(sound.file_name.as_ref()))
            .collect();
        let mut targets = BTreeMap::new();
        for row in 0..self.entries.len() {
            let entry = &mut self.entries[row];
            if entry.error.is_some() {
                continue;
            }
            let source = NameKey::new(entry.original.file_name().unwrap());
            if !directory.files.contains(&source)
                || library
                    .sounds
                    .get(entry.index)
                    .is_none_or(|sound| sound.path != entry.original)
            {
                entry.error = Some(Text::RenameSourceMissing);
                continue;
            }
            let target = NameKey::new(entry.name.as_ref());
            if !originals.contains(&target)
                && (directory.names.contains(&target) || library_names.contains(&target))
            {
                entry.error = Some(Text::RenameConflict);
            }
            if let Some(previous) = targets.insert(target, row) {
                entry.error = Some(Text::RenameDuplicate);
                self.entries[previous].error = Some(Text::RenameDuplicate);
            }
        }
    }

    pub fn error_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.error.is_some())
            .count()
    }

    pub fn change_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.changed()).count()
    }
}

// Use Windows' ordinal case comparison rather than Unicode case expansion (e.g. ß → SS).
#[derive(Clone, Eq)]
struct NameKey(Vec<u16>);

impl NameKey {
    fn new(name: &std::ffi::OsStr) -> Self {
        Self(name.encode_wide().collect())
    }
}

impl PartialEq for NameKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl PartialOrd for NameKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for NameKey {
    fn cmp(&self, other: &Self) -> Ordering {
        let result =
            unsafe { windows::Win32::Globalization::CompareStringOrdinal(&self.0, &other.0, true) };
        result.0.cmp(&2)
    }
}

pub(in crate::app) struct DirectorySnapshot {
    names: BTreeSet<NameKey>,
    files: BTreeSet<NameKey>,
    pub modified: SystemTime,
}

impl DirectorySnapshot {
    pub fn read(directory: &Path) -> anyhow::Result<Self> {
        let mut result = Self {
            names: BTreeSet::new(),
            files: BTreeSet::new(),
            modified: fs::metadata(directory)
                .and_then(|metadata| metadata.modified())
                .user_context(Text::ReadRenameDirectory)?,
        };
        for entry in fs::read_dir(directory).user_context(Text::ReadRenameDirectory)? {
            let entry = entry.user_context(Text::ReadRenameDirectory)?;
            let name = NameKey::new(&entry.file_name());
            if !entry
                .file_type()
                .user_context(Text::ReadRenameDirectory)?
                .is_dir()
            {
                result.files.insert(name.clone());
            }
            result.names.insert(name);
        }
        Ok(result)
    }

    pub fn read_if_changed(
        directory: &Path,
        modified: Option<SystemTime>,
    ) -> anyhow::Result<Option<Self>> {
        // Checking one directory timestamp avoids enumerating every file on each refresh.
        let current = fs::metadata(directory)
            .and_then(|metadata| metadata.modified())
            .user_context(Text::ReadRenameDirectory)?;
        if modified == Some(current) {
            Ok(None)
        } else {
            Self::read(directory).map(Some)
        }
    }
}

pub(super) fn move_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    unsafe {
        windows::Win32::Storage::FileSystem::MoveFileExW(
            windows::core::PCWSTR(crate::shared::utils::wide_null(from).as_ptr()),
            windows::core::PCWSTR(crate::shared::utils::wide_null(to).as_ptr()),
            windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(0),
        )
    }
    .user_context(Text::RenameSoundFailed)
}

impl Library {
    pub(super) fn rename_batch(
        &mut self,
        mut plan: RenamePlan,
        mut save: impl FnMut(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let first = plan.entries.first().user_context(Text::SoundNotFound)?;
        let directory = first.original.parent().user_context(Text::SoundNotFound)?;
        let snapshot = DirectorySnapshot::read(directory)?;
        plan.validate(self, &snapshot);
        ensure!(plan.error_count() == 0, Text::RenameValidationFailed);
        let entries: Vec<_> = plan
            .entries
            .iter()
            .filter(|entry| entry.changed())
            .collect();
        ensure!(!entries.is_empty(), Text::RenameValidationFailed);
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let mut reserved = snapshot.names;
        reserved.extend(
            plan.entries
                .iter()
                .map(|entry| NameKey::new(entry.name.as_ref())),
        );
        let temporary: Vec<_> = entries
            .iter()
            .map(|entry| {
                loop {
                    let nonce = NEXT_ID.fetch_add(1, AtomicOrdering::Relaxed);
                    let extension = entry
                        .original
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| format!(".{s}"))
                        .unwrap_or_default();
                    let name = format!(".vb{:x}-{nonce:x}{extension}", std::process::id());
                    if reserved.insert(NameKey::new(name.as_ref())) {
                        break entry.original.with_file_name(name);
                    }
                }
            })
            .collect();
        let original_sounds: Vec<_> = entries
            .iter()
            .map(|entry| self.sounds[entry.index].clone())
            .collect();
        let mut staged = 0;
        let mut finished = 0;
        let result = (|| {
            for (entry, temporary) in entries.iter().zip(&temporary) {
                move_file(&entry.original, temporary)?;
                let sound = &mut self.sounds[entry.index];
                sound.path = temporary.clone();
                sound.file_name = temporary
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                staged += 1;
            }
            for (entry, temporary) in entries.iter().zip(&temporary) {
                let destination = entry.original.with_file_name(&entry.name);
                move_file(temporary, &destination)?;
                let sound = &mut self.sounds[entry.index];
                sound.path = destination;
                sound.file_name.clone_from(&entry.name);
                finished += 1;
            }
            save(self)
        })();
        if let Err(error) = result {
            let mut errors = vec![Message::error(error)];
            // Vacate all completed targets before restoring originals, including swaps/cycles.
            for row in (0..finished).rev() {
                let sound = &mut self.sounds[entries[row].index];
                match move_file(&sound.path, &temporary[row]) {
                    Ok(()) => {
                        sound.path = temporary[row].clone();
                        sound.file_name = temporary[row]
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned();
                    }
                    Err(error) => errors.push(Message::RestoreFileName {
                        error: Details::new(error),
                        path: sound.path.display().to_string(),
                    }),
                }
            }
            for row in (0..staged).rev() {
                let sound = &mut self.sounds[entries[row].index];
                let original = &original_sounds[row];
                match move_file(&sound.path, &original.path) {
                    Ok(()) => *sound = original.clone(),
                    Err(error) => errors.push(Message::RestoreFileName {
                        error: Details::new(error),
                        path: sound.path.display().to_string(),
                    }),
                }
            }
            // Persist actual surviving paths if external interference prevented a full rollback.
            if errors.len() > 1
                && let Err(error) = save(self)
            {
                errors.push(Message::error(error));
            }
            return Err(anyhow::anyhow!(Message::Many(errors)));
        }
        Ok(())
    }
}
