//! Application settings persisted as JSON.

use std::{fs, io};

use anyhow::ensure;
use serde::{Deserialize, Serialize};
use voice_box_shared::utils::{WriteMode, settings_path, write_file_with_parents};

use crate::{
    hotkeys::Hotkey,
    i18n::{LanguagePreference, Message, Text, UserContext},
    soundboard::PlaybackConfig,
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

    pub fn playback(&self) -> PlaybackConfig {
        let device_id =
            |device: &Option<OutputDevice>| device.as_ref().map(|device| device.id.clone());
        PlaybackConfig {
            microphone_device: device_id(&self.playback_microphone_output_device),
            speaker_device: device_id(&self.playback_speaker_output_device),
            microphone_gain_percent: self.playback_microphone_output_gain_percent,
            speaker_gain_percent: self.playback_speaker_output_gain_percent,
            hold_hotkey: self.playback_auto_hold_hotkey,
            repeat: self.playback_repeat,
        }
    }

    /// Every bound global shortcut; `sound_hotkeys` is indexed like the sound list.
    pub fn shortcuts(
        &self,
        sound_hotkeys: impl IntoIterator<Item = Option<Hotkey>>,
    ) -> impl Iterator<Item = (BindingTarget, Hotkey)> {
        [
            (BindingTarget::Stop, self.playback_stop_hotkey),
            (BindingTarget::Random, self.playback_random_hotkey),
        ]
        .into_iter()
        .chain(
            sound_hotkeys
                .into_iter()
                .enumerate()
                .map(|(index, key)| (BindingTarget::Sound(index), key)),
        )
        .filter_map(|(target, key)| Some((target, key?)))
    }

    pub fn validate_binding(
        &self,
        sound_hotkeys: impl IntoIterator<Item = Option<Hotkey>>,
        target: BindingTarget,
        key: Hotkey,
    ) -> anyhow::Result<()> {
        let hold = target == BindingTarget::Hold;
        ensure!(
            if hold {
                key.is_valid_for_hold()
            } else {
                key.is_valid()
            },
            Text::InvalidHotkey
        );
        let same_base_key = |other: Hotkey| other.virtual_key_code == key.virtual_key_code;
        let mut others = self
            .shortcuts(sound_hotkeys)
            .filter(|&(other_target, _)| other_target != target);
        if hold {
            ensure!(
                !others.any(|(_, other)| same_base_key(other)),
                Text::HoldKeyConflict
            );
        } else {
            // Sounds may share a shortcut; it then plays a random sound from the group.
            let is_sound = |target| matches!(target, BindingTarget::Sound(_));
            ensure!(
                !others.any(|(other_target, other)| other == key
                    && !(is_sound(target) && is_sound(other_target))),
                Message::HotkeyConflict { key }
            );
            ensure!(
                !self.playback_auto_hold_hotkey.is_some_and(same_base_key),
                Text::HoldKeyConflict
            );
        }
        Ok(())
    }

    pub fn validate(
        &self,
        sound_hotkeys: impl Iterator<Item = Option<Hotkey>> + Clone,
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
