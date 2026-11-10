//! Persisted soundboard preferences.

use serde::{Deserialize, Serialize};

use crate::{soundboard::bindings::Keys, utils::GainPercent};

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Prefs {
    pub microphone_device: Option<OutputDevice>,
    pub microphone_gain_percent: GainPercent,
    pub speaker_device: Option<OutputDevice>,
    pub speaker_gain_percent: GainPercent,
    pub show_overlay: bool,
    pub repeat: bool,
    pub keys: Keys,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            microphone_device: None,
            microphone_gain_percent: GainPercent::new(100),
            speaker_device: None,
            speaker_gain_percent: GainPercent::new(100),
            show_overlay: true,
            repeat: false,
            keys: Keys::default(),
        }
    }
}
