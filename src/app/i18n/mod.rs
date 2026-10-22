//! Application language and presentation. Audio code never reads this state.
mod message;
mod shared_error;
mod text;

pub(in crate::app) use message::{Details, Message, UserContext};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};
pub(in crate::app) use text::Text;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::app) enum Language {
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::app) enum LanguagePreference {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

static LANGUAGE: AtomicU8 = AtomicU8::new(0);

pub(in crate::app) fn current_language() -> Language {
    match LANGUAGE.load(Ordering::Relaxed) {
        1 => Language::SimplifiedChinese,
        _ => Language::English,
    }
}

pub(in crate::app) fn set_language(language: Language) {
    LANGUAGE.store(
        match language {
            Language::English => 0,
            Language::SimplifiedChinese => 1,
        },
        Ordering::Relaxed,
    );
}

impl LanguagePreference {
    pub(in crate::app) fn resolve(self) -> Language {
        match self {
            Self::System => system_language(),
            Self::English => Language::English,
            Self::SimplifiedChinese => Language::SimplifiedChinese,
        }
    }

    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::System => Text::FollowSystem.text(),
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }
}

fn match_language(name: &str) -> Option<Language> {
    let name = name.replace('_', "-").to_ascii_lowercase();
    let parts: Vec<_> = name.split('-').collect();
    match parts.first().copied() {
        Some("en") => Some(Language::English),
        Some("zh")
            if !parts.contains(&"hant")
                && (parts.contains(&"hans") || parts.contains(&"cn") || parts.contains(&"sg")) =>
        {
            Some(Language::SimplifiedChinese)
        }
        _ => None,
    }
}

fn choose_language<'a>(names: impl IntoIterator<Item = &'a str>) -> Language {
    names
        .into_iter()
        .find_map(match_language)
        .unwrap_or(Language::English)
}

fn system_language() -> Language {
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let mut count = 0;
    let mut size = 0;
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut size).is_err()
            || size == 0
            || size > 65_536
        {
            return Language::English;
        }
        let mut buffer = vec![0u16; size as usize];
        if GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            &mut size,
        )
        .is_err()
        {
            return Language::English;
        }
        let names: Vec<_> = buffer
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(String::from_utf16_lossy)
            .collect();
        choose_language(names.iter().map(String::as_str))
    }
}
