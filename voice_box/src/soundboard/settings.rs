//! Persisted soundboard settings, stored inside the application `Settings`.

use serde::{Deserialize, Serialize};

use crate::soundboard::bindings::Keys;

/// A playback endpoint the user picked, remembered by name for when it is unplugged.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SoundboardSettings {
    pub microphone_device: Option<Endpoint>,
    pub microphone_gain_percent: GainPercent,
    pub speaker_device: Option<Endpoint>,
    pub speaker_gain_percent: GainPercent,
    pub show_overlay: bool,
    pub repeat: bool,
    pub keys: Keys,
}

impl Default for SoundboardSettings {
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

/// A playback volume, 0–200 % of the decoded level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GainPercent(u16);

impl<'de> Deserialize<'de> for GainPercent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        u16::deserialize(deserializer).map(Self::new)
    }
}

impl GainPercent {
    pub const MAX_VALUE: u16 = 200;

    pub fn new(value: u16) -> Self {
        Self(value.min(Self::MAX_VALUE))
    }

    pub fn is_mute(&self) -> bool {
        self.0 == 0
    }

    pub fn as_u16(&self) -> u16 {
        self.0
    }

    pub fn as_f32_gain(&self) -> f32 {
        self.0 as f32 / 100.0
    }
}
