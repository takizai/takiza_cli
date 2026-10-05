//! Interface language. Conversation content and tool output are never translated.
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Language {
    #[default]
    #[serde(rename = "en", alias = "english")]
    English,
    #[serde(rename = "ru", alias = "russian")]
    Russian,
    #[serde(rename = "zh", alias = "zh-CN", alias = "chinese")]
    Chinese,
}

impl Language {
    pub const ALL: [Self; 3] = [Self::English, Self::Russian, Self::Chinese];
    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Russian => "Русский",
            Self::Chinese => "简体中文",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);
pub fn set_current(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}
pub fn current() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Language::Russian,
        2 => Language::Chinese,
        _ => Language::English,
    }
}

/// Look up interface literals; unknown strings retain their original text.
pub fn tr(text: &str) -> &str {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static CATALOG: OnceLock<HashMap<String, [String; 2]>> = OnceLock::new();
    let index = match current() {
        Language::English => return text,
        Language::Russian => 0,
        Language::Chinese => 1,
    };
    CATALOG
        .get_or_init(|| {
            serde_json::from_str(include_str!("locales/ui.json"))
                .expect("valid interface translations")
        })
        .get(text)
        .map(|translations| translations[index].as_str())
        .unwrap_or(text)
}

// Generated from locales/ui.json; named format arguments remain explicit at call sites.
include!(concat!(env!("OUT_DIR"), "/ui_formats.rs"));
