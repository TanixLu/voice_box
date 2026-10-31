mod message;
mod text;

use std::sync::atomic::{AtomicU8, Ordering};

use num_enum::{IntoPrimitive, TryFromPrimitive};
use strum::EnumIter;

pub use crate::i18n::{
    message::{Details, Message, UserContext},
    text::Text,
};
use crate::utils::win_preferred_ui_languages;

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

#[derive(Clone, Copy, PartialEq, Eq, EnumIter)]
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
                for tag in win_preferred_ui_languages() {
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
