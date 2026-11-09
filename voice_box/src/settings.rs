//! Application settings persisted as JSON.

use std::{fs, io};

use anyhow::ensure;
use serde::{Deserialize, Serialize};
use voice_box_shared::utils::{WriteMode, settings_path, write_file_with_parents};

use crate::{
    hotkeys::Hotkey,
    i18n::{LanguagePreference, Message, Text, UserContext},
    utils::GainPercent,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingTarget {
    Sound(usize),
    Hold,
    Stop,
    Random,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub language_preference: LanguagePreference,
    pub theme_preference: egui::ThemePreference,
    pub startup_enabled: bool,
    pub playback_microphone_output_device: Option<OutputDevice>,
    pub playback_microphone_output_gain_percent: GainPercent,
    pub playback_speaker_output_device: Option<OutputDevice>,
    pub playback_speaker_output_gain_percent: GainPercent,
    pub playback_auto_hold_hotkey: Option<Hotkey>,
    pub playback_stop_hotkey: Option<Hotkey>,
    pub playback_random_hotkey: Option<Hotkey>,
    pub playback_show_overlay: bool,
    pub playback_repeat: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language_preference: LanguagePreference::System,
            theme_preference: egui::ThemePreference::System,
            startup_enabled: false,
            playback_microphone_output_device: None,
            playback_microphone_output_gain_percent: GainPercent::new(100),
            playback_speaker_output_device: None,
            playback_speaker_output_gain_percent: GainPercent::new(100),
            playback_auto_hold_hotkey: None,
            playback_stop_hotkey: None,
            playback_random_hotkey: None,
            playback_show_overlay: true,
            playback_repeat: false,
        }
    }
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let bytes = match fs::read(settings_path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).user_context(Text::LoadAppSettingsFailed),
        };
        let mut settings: Self =
            serde_json::from_slice(&bytes).user_context(Text::LoadAppSettingsFailed)?;
        settings.sanitize();
        Ok(settings)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).user_context(Text::SaveAppSettingsFailed)?;
        write_file_with_parents(settings_path(), WriteMode::AtomicReplace, |file| {
            file.write_all(&bytes)
        })
        .user_context(Text::SaveAppSettingsFailed)
    }

    pub fn validate_binding(
        &self,
        sound_hotkeys: impl Iterator<Item = Hotkey> + Clone,
        target: BindingTarget,
        key: Hotkey,
    ) -> anyhow::Result<()> {
        ensure!(
            if target == BindingTarget::Hold {
                key.is_valid_for_hold()
            } else {
                key.is_valid()
            },
            Text::InvalidHotkey
        );
        let duplicate = (target != BindingTarget::Stop && self.playback_stop_hotkey == Some(key))
            || (target != BindingTarget::Random && self.playback_random_hotkey == Some(key))
            || (matches!(target, BindingTarget::Stop | BindingTarget::Random)
                && sound_hotkeys.clone().any(|bound| bound == key));
        ensure!(
            !duplicate || target == BindingTarget::Hold,
            Message::HotkeyConflict { key }
        );
        let overlaps_hold = match target {
            BindingTarget::Sound(_) | BindingTarget::Stop | BindingTarget::Random => self
                .playback_auto_hold_hotkey
                .is_some_and(|hold| hold.virtual_key_code == key.virtual_key_code),
            BindingTarget::Hold => {
                self.playback_stop_hotkey
                    .is_some_and(|shortcut| shortcut.virtual_key_code == key.virtual_key_code)
                    || self
                        .playback_random_hotkey
                        .is_some_and(|shortcut| shortcut.virtual_key_code == key.virtual_key_code)
                    || sound_hotkeys
                        .clone()
                        .any(|shortcut| shortcut.virtual_key_code == key.virtual_key_code)
            }
        };
        ensure!(!overlaps_hold, Text::HoldKeyConflict);
        Ok(())
    }

    pub fn validate(
        &self,
        sound_hotkeys: impl Iterator<Item = Hotkey> + Clone,
    ) -> anyhow::Result<()> {
        for (target, key) in [
            (BindingTarget::Hold, self.playback_auto_hold_hotkey),
            (BindingTarget::Stop, self.playback_stop_hotkey),
            (BindingTarget::Random, self.playback_random_hotkey),
        ] {
            if let Some(key) = key {
                self.validate_binding(sound_hotkeys.clone(), target, key)?;
            }
        }
        Ok(())
    }

    pub fn sanitize(&mut self) {
        let _ = self
            .playback_auto_hold_hotkey
            .take_if(|key| !key.is_valid_for_hold());
        let _ = self.playback_stop_hotkey.take_if(|key| !key.is_valid());
        let _ = self.playback_random_hotkey.take_if(|key| !key.is_valid());
    }
}
