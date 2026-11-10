//! In-memory library edits and value-oriented persistence entry points.

use std::collections::HashSet;

use anyhow::ensure;

use crate::{
    database,
    i18n::{Text, UserContext},
    model::{Hotkey, Sound, SoundId, is_valid_sound_name},
    soundboard::rename::RenamePlan,
};

#[derive(Clone, Default)]
pub struct Library {
    pub sounds: Vec<Sound>,
}

impl Library {
    pub fn load() -> anyhow::Result<Self> {
        Ok(Self {
            sounds: database::sounds::load()?,
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        database::sounds::save(&self.sounds).user_context(Text::SaveSoundLibraryFailed)
    }

    pub fn move_sounds(&mut self, ids: &HashSet<SoundId>, to_top: bool) -> anyhow::Result<()> {
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
        self.sounds
            .sort_by_key(|sound| ids.contains(&sound.id) != to_top);
        Ok(())
    }

    pub fn move_sound(&mut self, from: usize, to: usize) -> anyhow::Result<()> {
        ensure!(
            from < self.sounds.len() && to < self.sounds.len(),
            Text::SoundNotFound
        );
        if from == to {
            return Ok(());
        }
        let sound = self.sounds.remove(from);
        self.sounds.insert(to, sound);
        Ok(())
    }

    pub fn set_hotkeys(
        &mut self,
        ids: &HashSet<SoundId>,
        key: Option<Hotkey>,
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
        for index in indices {
            self.sounds[index].hotkey = key;
        }
        Ok(())
    }

    pub fn rename_batch(&mut self, mut plan: RenamePlan) -> anyhow::Result<()> {
        plan.validate(self);
        ensure!(
            plan.error_count() == 0 && plan.change_count() > 0,
            Text::RenameValidationFailed
        );
        for entry in plan.entries {
            self.sounds
                .get_mut(entry.index)
                .user_context(Text::SoundNotFound)?
                .name = entry.name;
        }
        Ok(())
    }

    pub fn rename(&mut self, index: usize, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        ensure!(is_valid_sound_name(name), Text::InvalidSoundName);
        let sound = self
            .sounds
            .get_mut(index)
            .user_context(Text::SoundNotFound)?;
        sound.name = name.to_owned();
        Ok(())
    }

    pub fn delete(&mut self, ids: &HashSet<SoundId>, reclaim_space: bool) -> anyhow::Result<()> {
        database::sounds::delete(ids, reclaim_space)?;
        self.sounds.retain(|sound| !ids.contains(&sound.id));
        Ok(())
    }
}
