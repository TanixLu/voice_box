//! Translated text: `Text` for fixed strings, `Message` for strings with arguments.
//!
//! Keys say what kind of text they hold. A label names its control (`DeleteSound`); an error
//! names the failed operation with a `Failed` suffix (`OpenDatabaseFailed`), or the state at
//! fault (`InvalidKey`, `LibraryBusy`).

mod message;
mod text;

use std::sync::atomic::{AtomicU8, Ordering};

use num_enum::{IntoPrimitive, TryFromPrimitive};
use serde::{Deserialize, Serialize};
use strum::EnumIter;
use windows::{
    Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME},
    core::PWSTR,
};

pub use crate::i18n::{
    message::{Details, Message, UserContext},
    text::Text,
};
use crate::model::Hotkey;

#[derive(Clone, Copy, TryFromPrimitive, IntoPrimitive, EnumIter)]
#[repr(u8)]
pub enum Language {
    English,
    SimplifiedChinese,
}

static LANGUAGE: AtomicU8 = AtomicU8::new(0);

pub fn current_language() -> Language {
    Language::try_from(LANGUAGE.load(Ordering::Relaxed)).unwrap_or(Language::English)
}

pub fn set_language(language: Language) {
    LANGUAGE.store(language.into(), Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Eq, EnumIter, Deserialize, Serialize)]
pub enum LanguagePreference {
    System,
    English,
    SimplifiedChinese,
}

impl LanguagePreference {
    pub fn text(&self) -> &'static str {
        match self {
            Self::System => Text::FollowSystem.text(),
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }

    pub fn resolve(self) -> Language {
        match self {
            Self::System => {
                for tag in preferred_ui_languages() {
                    let tag = tag.replace('_', "-").to_ascii_lowercase();
                    match tag.as_str() {
                        "zh-cn" | "zh-sg" | "zh-hans" | "zh-hans-cn" | "zh-hans-sg"
                        | "zh-hans-hk" | "zh-hans-mo" => return Language::SimplifiedChinese,
                        "en" => return Language::English,
                        _ if tag.starts_with("en-") => return Language::English,
                        _ => continue,
                    }
                }
                Language::English
            }
            Self::English => Language::English,
            Self::SimplifiedChinese => Language::SimplifiedChinese,
        }
    }
}

fn preferred_ui_languages() -> Vec<String> {
    let mut count = 0;
    let mut size = 0;
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut size).is_err()
            || size == 0
            || size > 65_536
        {
            return Vec::new();
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut size,
        )
        .is_err()
        {
            return Vec::new();
        }
        buffer
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}

pub fn hotkey_label(key: Hotkey) -> String {
    key.label_with(Text::InvalidKey.text())
}
