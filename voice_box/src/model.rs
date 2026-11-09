//! Plain domain data shared by every layer; depends on nothing else in the crate.

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub type SoundId = i64;

#[derive(Clone)]
pub struct Sound {
    pub id: SoundId,
    pub name: String,
    pub hotkey: Option<Hotkey>,
    pub duration_secs: f64,
}

pub fn is_valid_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.chars().any(char::is_control)
}

/// Modifier flags in display and press order, with the key each one stands for.
pub const MODIFIERS: [(HOT_KEY_MODIFIERS, VIRTUAL_KEY, &str); 4] = [
    (MOD_CONTROL, VK_CONTROL, "Ctrl"),
    (MOD_SHIFT, VK_SHIFT, "Shift"),
    (MOD_ALT, VK_MENU, "Alt"),
    (MOD_WIN, VK_LWIN, "Win"),
];
const ALL_MODIFIERS: u32 = MOD_CONTROL.0 | MOD_SHIFT.0 | MOD_ALT.0 | MOD_WIN.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct Hotkey {
    pub virtual_key_code: u16,
    pub modifiers: u32,
}

impl Hotkey {
    pub fn is_valid(self) -> bool {
        self.is_valid_for_hold() && !is_modifier(self.virtual_key_code)
    }
    pub fn is_valid_for_hold(self) -> bool {
        key_name(self.virtual_key_code).is_some() && self.modifiers & !ALL_MODIFIERS == 0
    }
    pub fn is_escape(self) -> bool {
        self.virtual_key_code == VK_ESCAPE.0
    }
    /// `invalid` names a key code that has no label.
    pub fn label_with(self, invalid: &str) -> String {
        let key = key_name(self.virtual_key_code).unwrap_or_else(|| invalid.into());
        self.modifier_keys()
            .map(|(_, _, name)| name.to_owned())
            .chain([key])
            .collect::<Vec<_>>()
            .join(" + ")
    }
    pub fn modifier_keys(
        self,
    ) -> impl Iterator<Item = (HOT_KEY_MODIFIERS, VIRTUAL_KEY, &'static str)> {
        MODIFIERS
            .into_iter()
            .filter(move |(flag, ..)| self.modifiers & flag.0 != 0)
    }
}

pub fn is_modifier(key: u16) -> bool {
    matches!(
        VIRTUAL_KEY(key),
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LWIN
            | VK_RWIN
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
    )
}

pub fn key_name(key: u16) -> Option<String> {
    Some(match key {
        0x30..=0x39 | 0x41..=0x5a => char::from_u32(u32::from(key))?.to_string(),
        0x70..=0x87 => format!("F{}", key - 0x6f),
        0x60..=0x69 => format!("Num {}", key - 0x60),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0d => "Enter".into(),
        0x1b => "Esc".into(),
        0x20 => "Space".into(),
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x5b => "Left Win".into(),
        0x5c => "Right Win".into(),
        0xa0 => "Left Shift".into(),
        0xa1 => "Right Shift".into(),
        0xa2 => "Left Ctrl".into(),
        0xa3 => "Right Ctrl".into(),
        0xa4 => "Left Alt".into(),
        0xa5 => "Right Alt".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2d => "Insert".into(),
        0x2e => "Delete".into(),
        0x6a => "Num *".into(),
        0x6b => "Num +".into(),
        0x6d => "Num -".into(),
        0x6e => "Num .".into(),
        0x6f => "Num /".into(),
        0xba => ";".into(),
        0xbb => "=".into(),
        0xbc => ",".into(),
        0xbd => "-".into(),
        0xbe => ".".into(),
        0xbf => "/".into(),
        0xc0 => "`".into(),
        0xdb => "[".into(),
        0xdc => "\\".into(),
        0xdd => "]".into(),
        0xde => "'".into(),
        _ => return None,
    })
}
