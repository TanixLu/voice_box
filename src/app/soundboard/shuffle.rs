use super::library::Sound;
use crate::app::hotkeys::Hotkey;
use std::{
    collections::{HashMap, HashSet},
    hash::{BuildHasher, RandomState},
    path::PathBuf,
};

#[derive(Default)]
pub(super) struct Shuffle {
    played: HashMap<Option<Hotkey>, HashSet<PathBuf>>,
    pub(super) last: Option<PathBuf>,
}

impl Shuffle {
    pub(super) fn choose(&mut self, sounds: &[Sound], hotkey: Option<Hotkey>) -> Option<usize> {
        self.played
            .retain(|key, _| key.is_none() || sounds.iter().any(|sound| sound.hotkey == *key));
        let candidates: HashSet<_> = sounds
            .iter()
            .filter(|sound| hotkey.is_none() || sound.hotkey == hotkey)
            .map(|sound| &sound.path)
            .collect();
        let played = self.played.entry(hotkey).or_default();
        // Paths survive list reordering; reconcile membership before every draw.
        played.retain(|path| candidates.contains(path));
        if played.len() == candidates.len() {
            played.clear();
        }
        let random = RandomState::new();
        let (index, sound) = sounds
            .iter()
            .enumerate()
            .filter(|(_, sound)| candidates.contains(&sound.path) && !played.contains(&sound.path))
            .min_by_key(|(_, sound)| {
                (
                    self.last.as_ref() == Some(&sound.path),
                    random.hash_one(&sound.path),
                )
            })?;
        played.insert(sound.path.clone());
        self.last = Some(sound.path.clone());
        Some(index)
    }
}
