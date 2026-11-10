//! Global key bindings: the playback keys in the settings plus each sound's key.

use std::collections::{HashMap, HashSet};

use anyhow::ensure;
use serde::{Deserialize, Serialize};

use crate::{
    i18n::{Message, Text},
    model::{Hotkey, Sound, SoundId},
};

/// What a captured key is bound to.
#[derive(Clone, PartialEq, Eq)]
pub enum Target {
    Sounds(HashSet<SoundId>),
    Hold,
    Stop,
    Random,
}

/// What a pressed global shortcut asks the soundboard to do.
#[derive(Clone, Copy)]
pub enum Action {
    Stop,
    /// A random sound bound to this key, or from the whole library.
    Random(Option<Hotkey>),
}

/// Saved keys that are no longer valid load as unbound.
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Keys {
    /// Held down by the soundboard while a sound plays.
    #[serde(deserialize_with = "hold_key")]
    pub hold: Option<Hotkey>,
    #[serde(deserialize_with = "shortcut_key")]
    pub stop: Option<Hotkey>,
    #[serde(deserialize_with = "shortcut_key")]
    pub random: Option<Hotkey>,
}

fn hold_key<'de, D: serde::Deserializer<'de>>(keys: D) -> Result<Option<Hotkey>, D::Error> {
    Ok(Option::<Hotkey>::deserialize(keys)?.filter(|key| key.is_valid_for_hold()))
}

fn shortcut_key<'de, D: serde::Deserializer<'de>>(keys: D) -> Result<Option<Hotkey>, D::Error> {
    Ok(Option::<Hotkey>::deserialize(keys)?.filter(|key| key.is_valid()))
}

impl Keys {
    /// The preference a non-sound target is stored in.
    fn slot(&mut self, target: &Target) -> Option<&mut Option<Hotkey>> {
        match target {
            Target::Sounds(_) => None,
            Target::Hold => Some(&mut self.hold),
            Target::Stop => Some(&mut self.stop),
            Target::Random => Some(&mut self.random),
        }
    }

    /// Validates `key` for `target` and stores it in the matching preference.
    /// Sound keys live in the library, so the caller saves those.
    pub fn bind(&mut self, sounds: &[Sound], target: &Target, key: Hotkey) -> anyhow::Result<()> {
        self.validate(sounds, target, key)?;
        if let Some(slot) = self.slot(target) {
            *slot = Some(key);
        }
        Ok(())
    }

    pub fn actions(&self, sounds: &[Sound]) -> HashMap<Hotkey, Action> {
        let sound_keys = sounds.iter().filter_map(|sound| sound.hotkey);
        sound_keys
            .map(|key| (key, Action::Random(Some(key))))
            .chain(self.stop.map(|key| (key, Action::Stop)))
            .chain(self.random.map(|key| (key, Action::Random(None))))
            .collect()
    }

    /// Sounds may share a key, which then plays a random sound from the group. Every
    /// other binding is exclusive, and nothing may reuse the hold key's base key.
    pub fn validate(&self, sounds: &[Sound], target: &Target, key: Hotkey) -> anyhow::Result<()> {
        let hold = *target == Target::Hold;
        let valid = if hold {
            key.is_valid_for_hold()
        } else {
            key.is_valid()
        };
        ensure!(valid, Text::InvalidHotkey);
        let same_base_key = |other: Hotkey| other.virtual_key_code == key.virtual_key_code;
        let mut others = [(Target::Stop, self.stop), (Target::Random, self.random)]
            .into_iter()
            .filter(|(other, _)| other != target)
            .filter_map(|(_, key)| key);
        let mut sound_keys = sounds.iter().filter_map(|sound| sound.hotkey);
        if hold {
            ensure!(
                !others.chain(sound_keys).any(same_base_key),
                Text::HoldKeyConflict
            );
        } else {
            let shares_sound_key =
                !matches!(target, Target::Sounds(_)) && sound_keys.any(|other| other == key);
            ensure!(
                !shares_sound_key && !others.any(|other| other == key),
                Message::HotkeyConflict { key }
            );
            ensure!(!self.hold.is_some_and(same_base_key), Text::HoldKeyConflict);
        }
        Ok(())
    }

    pub fn validate_all(&self, sounds: &[Sound]) -> anyhow::Result<()> {
        [
            (Target::Hold, self.hold),
            (Target::Stop, self.stop),
            (Target::Random, self.random),
        ]
        .into_iter()
        .filter_map(|(target, key)| Some((target, key?)))
        .try_for_each(|(target, key)| self.validate(sounds, &target, key))
    }
}
