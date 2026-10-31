//! Typed application settings in database's key/value table.

mod storage;

use anyhow::ensure;

use voice_box_shared::pcm_transport::GainPercent;

use crate::{
    database,
    hotkeys::Hotkey,
    i18n::{LanguagePreference, Message, Text, UserContext},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindingTarget {
    Sound(usize),
    Hold,
    Stop,
    Random,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

macro_rules! define_settings {
    ($($field:ident: $ty:ty = $default:expr;)*) => {
        #[derive(Clone, PartialEq, Eq)]
        pub struct Settings {
            $(pub $field: $ty,)*
        }

        impl Default for Settings {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        storage::impl_storage! { $($field: $ty = $default;)* }
    };
}

define_settings! {
    language_preference: LanguagePreference = LanguagePreference::System;
    theme_preference: egui::ThemePreference = egui::ThemePreference::System;
    startup_enabled: bool = false;
    playback_microphone_output_device: Option<OutputDevice> = None;
    playback_speaker_output_device: Option<OutputDevice> = None;
    playback_speaker_output_gain_percent: GainPercent = GainPercent::new(100);
    playback_microphone_output_gain_percent: GainPercent = GainPercent::new(100);
    playback_include_microphone_audio: bool = true;
    playback_auto_hold_hotkey: Option<Hotkey> = None;
    playback_stop_hotkey: Option<Hotkey> = None;
    playback_random_hotkey: Option<Hotkey> = None;
    playback_show_overlay: bool = true;
    playback_repeat: bool = false;
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let mut connection = database::open()?;
        let transaction = connection.transaction()?;
        Self::read_from(&transaction)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&mut database::open()?)
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
