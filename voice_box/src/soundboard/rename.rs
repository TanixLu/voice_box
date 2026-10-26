use super::{
    library::Library,
    sound::{SoundId, is_valid_name},
};
use crate::i18n::{Language, Text, UserContext, current_language};
use anyhow::ensure;
use std::{collections::HashSet, ops::Range};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenameLocation {
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

pub(crate) struct RenameRule {
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

pub(crate) struct RenameEntry {
    pub index: usize,
    pub id: SoundId,
    pub original: String,
    pub name: String,
    pub old_marks: Vec<Range<usize>>,
    pub new_marks: Vec<Range<usize>>,
    pub error: Option<Text>,
}

impl RenameEntry {
    pub fn changed(&self) -> bool {
        self.original != self.name
    }
}

pub(crate) struct RenamePlan {
    pub entries: Vec<RenameEntry>,
}

impl RenamePlan {
    pub fn new(library: &Library, selected: &HashSet<SoundId>, rule: &RenameRule) -> Self {
        let width = selected.len().max(1).ilog10().saturating_add(1).max(3) as usize;
        let mut entries = Vec::with_capacity(selected.len());
        for (index, sound) in library.sounds.iter().enumerate() {
            if !selected.contains(&sound.id) {
                continue;
            }
            let stem = &sound.name;
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
            let error = (!is_valid_name(&name)).then_some(Text::InvalidSoundName);
            entries.push(RenameEntry {
                index,
                id: sound.id,
                original: sound.name.clone(),
                name,
                old_marks,
                new_marks,
                error,
            });
        }
        Self { entries }
    }

    pub fn validate(&mut self, library: &Library) {
        for entry in &mut self.entries {
            if library
                .sounds
                .get(entry.index)
                .is_none_or(|sound| sound.id != entry.id || sound.name != entry.original)
            {
                entry.error = Some(Text::SoundNotFound);
            } else if !is_valid_name(&entry.name) {
                entry.error = Some(Text::InvalidSoundName);
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

impl Library {
    pub(super) fn rename_batch(
        &mut self,
        mut plan: RenamePlan,
        save: impl FnOnce(&Self) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        plan.validate(self);
        ensure!(
            plan.error_count() == 0 && plan.change_count() > 0,
            Text::RenameValidationFailed
        );
        let original = self.sounds.clone();
        for entry in plan.entries {
            self.sounds
                .get_mut(entry.index)
                .user_context(Text::SoundNotFound)?
                .name = entry.name;
        }
        if let Err(error) = save(self) {
            self.sounds = original;
            return Err(error);
        }
        Ok(())
    }
}
