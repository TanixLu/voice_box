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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16) -> Hotkey {
        Hotkey {
            virtual_key_code: code,
            modifiers: 0,
        }
    }

    fn sound(name: &str, hotkey: Option<Hotkey>) -> Sound {
        Sound {
            path: PathBuf::from(name),
            hotkey,
            ..Sound::default()
        }
    }

    #[test]
    fn empty_single_and_cleared_lists_are_safe() {
        let mut shuffle = Shuffle::default();
        assert_eq!(shuffle.choose(&[], None), None);
        let sounds = [sound("only.wav", Some(key(65)))];
        for _ in 0..5 {
            assert_eq!(shuffle.choose(&sounds, None), Some(0));
            assert_eq!(shuffle.choose(&sounds, Some(key(65))), Some(0));
            assert_eq!(shuffle.choose(&sounds, Some(key(66))), None);
        }
        assert_eq!(shuffle.choose(&[], None), None);
        assert_eq!(shuffle.choose(&[], Some(key(65))), None);
        assert_eq!(shuffle.choose(&sounds, None), Some(0));
    }

    #[test]
    fn each_round_covers_every_sound_without_boundary_repeats() {
        for count in [2, 3, 10] {
            let sounds: Vec<_> = (0..count)
                .map(|index| sound(&format!("{index}.wav"), None))
                .collect();
            let mut shuffle = Shuffle::default();
            let mut last = None;
            for _ in 0..20 {
                let mut round = HashSet::new();
                for _ in 0..count {
                    let index = shuffle.choose(&sounds, None).unwrap();
                    assert_ne!(Some(index), last);
                    assert!(round.insert(index));
                    last = Some(index);
                }
                assert_eq!(round.len(), count);
            }
        }
    }

    #[test]
    fn groups_and_global_have_independent_rounds() {
        let a = Some(key(65));
        let b = Some(key(66));
        let sounds = [
            sound("a1.wav", a),
            sound("a2.wav", a),
            sound("b1.wav", b),
            sound("b2.wav", b),
            sound("unbound.wav", None),
        ];
        let mut shuffle = Shuffle::default();
        let mut global = HashSet::new();
        for _ in 0..sounds.len() {
            for group in [a, b] {
                let first = shuffle.choose(&sounds, group).unwrap();
                let second = shuffle.choose(&sounds, group).unwrap();
                assert_eq!(sounds[first].hotkey, group);
                assert_eq!(sounds[second].hotkey, group);
                assert_ne!(first, second);
            }
            assert!(global.insert(shuffle.choose(&sounds, None).unwrap()));
        }
        assert_eq!(global.len(), sounds.len());
    }

    #[test]
    fn adding_and_reordering_keep_existing_round_progress() {
        let mut sounds = vec![sound("a.wav", None), sound("b.wav", None)];
        let mut shuffle = Shuffle::default();
        let first = sounds[shuffle.choose(&sounds, None).unwrap()].path.clone();
        sounds.push(sound("new.wav", None));
        sounds.reverse();
        let mut round = HashSet::from([first]);
        for _ in 0..2 {
            let index = shuffle.choose(&sounds, None).unwrap();
            assert!(round.insert(sounds[index].path.clone()));
        }
        assert_eq!(round.len(), 3);
        assert!(round.contains(&PathBuf::from("new.wav")));
    }

    #[test]
    fn deleting_replacing_and_renaming_never_return_stale_indices() {
        let mut sounds: Vec<_> = ["a.wav", "b.wav", "c.wav", "d.wav"]
            .into_iter()
            .map(|name| sound(name, None))
            .collect();
        let mut shuffle = Shuffle::default();
        let first = sounds[shuffle.choose(&sounds, None).unwrap()].path.clone();
        let removed = sounds.iter().position(|sound| sound.path != first).unwrap();
        sounds.swap_remove(removed);
        let mut round = HashSet::from([first]);
        for _ in 0..2 {
            let index = shuffle.choose(&sounds, None).unwrap();
            assert!(round.insert(sounds[index].path.clone()));
        }
        sounds[0] = sound("replacement.wav", None);
        assert_eq!(shuffle.choose(&sounds, None), Some(0));
        sounds[0].path = PathBuf::from("renamed.wav");
        assert_eq!(shuffle.choose(&sounds, None), Some(0));
        sounds.truncate(1);
        assert_eq!(shuffle.choose(&sounds, None), Some(0));
    }

    #[test]
    fn rebinding_uses_current_group_membership() {
        let a = Some(key(65));
        let b = Some(key(66));
        let mut sounds = [sound("a1.wav", a), sound("a2.wav", a), sound("b.wav", b)];
        let mut shuffle = Shuffle::default();
        let first = shuffle.choose(&sounds, a).unwrap();
        sounds[first].hotkey = b;
        sounds[2].hotkey = a;
        let mut round = HashSet::new();
        for _ in 0..2 {
            let index = shuffle.choose(&sounds, a).unwrap();
            assert_eq!(sounds[index].hotkey, a);
            assert!(round.insert(index));
        }
        sounds.iter_mut().for_each(|sound| sound.hotkey = None);
        assert_eq!(shuffle.choose(&sounds, a), None);
        assert_eq!(shuffle.choose(&sounds, b), None);
    }

    #[test]
    fn avoids_last_manual_or_other_group_selection_when_possible() {
        let a = Some(key(65));
        let sounds = [sound("a.wav", a), sound("b.wav", None)];
        let mut shuffle = Shuffle::default();
        shuffle.last = Some(sounds[1].path.clone());
        assert_eq!(shuffle.choose(&sounds, None), Some(0));
        assert_eq!(shuffle.choose(&sounds, a), Some(0));
        assert_eq!(shuffle.choose(&sounds, None), Some(1));
        assert_eq!(shuffle.choose(&sounds, None), Some(0));
    }
}
