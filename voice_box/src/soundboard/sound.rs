//! Sound metadata shared by the library, jobs and views.

use crate::hotkeys::Hotkey;

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
