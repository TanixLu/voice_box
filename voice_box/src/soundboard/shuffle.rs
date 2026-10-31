use std::{
    collections::{HashMap, HashSet},
    hash::{BuildHasher, RandomState},
};

use crate::{
    hotkeys::Hotkey,
    soundboard::sound::{Sound, SoundId},
};

#[derive(Default)]
pub struct Shuffle {
    played: HashMap<Option<Hotkey>, HashSet<SoundId>>,
    pub last: Option<SoundId>,
}

impl Shuffle {
    pub fn choose(&mut self, sounds: &[Sound], hotkey: Option<Hotkey>) -> Option<usize> {
        self.played
            .retain(|key, _| key.is_none() || sounds.iter().any(|sound| sound.hotkey == *key));
        let candidates: HashSet<_> = sounds
            .iter()
            .filter(|sound| hotkey.is_none() || sound.hotkey == hotkey)
            .map(|sound| &sound.id)
            .collect();
        let played = self.played.entry(hotkey).or_default();
        // IDs survive renaming and list reordering; reconcile membership before every draw.
        played.retain(|sound_id| candidates.contains(sound_id));
        if played.len() == candidates.len() {
            played.clear();
        }
        let random = RandomState::new();
        let (index, sound) = sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| candidates.contains(&sound.id) && !played.contains(&sound.id))
            .min_by_key(|(_, sound)| {
                (
                    self.last.as_ref() == Some(&sound.id),
                    random.hash_one(sound.id),
                )
            })?;
        played.insert(sound.id);
        self.last = Some(sound.id);
        Some(index)
    }
}
