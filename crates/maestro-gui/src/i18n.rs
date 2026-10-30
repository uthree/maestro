//! The window's words, in English or Japanese.
//!
//! The language is chosen before the window opens: `--lang en|ja`, then the one picked in
//! Settings ([`crate::prefs`]), then `MAESTRO_LANG`, then the system's language. Settings can
//! change it while the window is open. Each string is written in place as an English and a
//! Japanese pair, with [`tr`] for fixed text and [`trf!`](crate::trf) for formatted text, and is
//! looked up as the view draws, so nothing holds on to a translated copy and a change shows at
//! the next frame.
//!
//! What the daemon and the agents say is shown as it comes; only the window's own chrome is
//! translated here.

use std::sync::atomic::{AtomicU8, Ordering};

use maestro_core::{Difficulty, Effort};

/// A language the window can speak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    /// English.
    En,
    /// Japanese.
    Ja,
}

impl Lang {
    /// The language a locale or language tag names (`ja`, `ja_JP.UTF-8`, `en-US`), if it is
    /// one the window speaks.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.trim().to_ascii_lowercase();
        let language = tag.split(['_', '-', '.', '@']).next().unwrap_or_default();
        match language {
            "ja" | "jp" => Some(Self::Ja),
            "en" | "c" | "posix" => Some(Self::En),
            _ => None,
        }
    }

    /// The tag [`Lang::from_tag`] reads back.
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
        }
    }
}

/// The language in use: 0 before it is first looked up, else 1 for English and 2 for Japanese.
static LANG: AtomicU8 = AtomicU8::new(0);

/// Switches the language. What is drawn after this speaks it.
pub fn set(lang: Lang) {
    LANG.store(lang as u8 + 1, Ordering::Relaxed);
}

/// The language the window speaks: the one [`set`] chose, or else the environment's.
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::En,
        2 => Lang::Ja,
        _ => {
            let lang = detect();
            set(lang);
            lang
        }
    }
}

/// The language from `MAESTRO_LANG`, then from the system; English when neither is Japanese.
pub fn detect() -> Lang {
    std::env::var("MAESTRO_LANG")
        .ok()
        .and_then(|tag| Lang::from_tag(&tag))
        .or_else(|| sys_locale::get_locale().and_then(|tag| Lang::from_tag(&tag)))
        .unwrap_or(Lang::En)
}

/// The text for the current language.
pub fn tr(en: &'static str, ja: &'static str) -> &'static str {
    match lang() {
        Lang::En => en,
        Lang::Ja => ja,
    }
}

/// [`format!`] in the current language: `trf!("{n} steps", "{n} ステップ")`.
#[macro_export]
macro_rules! trf {
    ($en:literal, $ja:literal $(, $arg:expr)* $(,)?) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::En => format!($en $(, $arg)*),
            $crate::i18n::Lang::Ja => format!($ja $(, $arg)*),
        }
    };
}

/// A difficulty's name.
pub fn difficulty(difficulty: Difficulty) -> &'static str {
    match difficulty {
        Difficulty::Trivial => tr("Trivial", "ごく簡単"),
        Difficulty::Easy => tr("Easy", "簡単"),
        Difficulty::Moderate => tr("Moderate", "普通"),
        Difficulty::Hard => tr("Hard", "難しい"),
        Difficulty::Expert => tr("Expert", "高難度"),
    }
}

/// An effort's name.
pub fn effort(effort: Effort) -> &'static str {
    match effort {
        Effort::Low => tr("Low effort", "低エフォート"),
        Effort::Medium => tr("Medium effort", "中エフォート"),
        Effort::High => tr("High effort", "高エフォート"),
        Effort::Max => tr("Max effort", "最大エフォート"),
    }
}

#[cfg(test)]
mod tests {
    use super::Lang;

    #[test]
    fn reads_locale_tags() {
        assert_eq!(Lang::from_tag("ja_JP.UTF-8"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag("ja-JP"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag("JA"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag("en_US.UTF-8"), Some(Lang::En));
        assert_eq!(Lang::from_tag("C"), Some(Lang::En));
        assert_eq!(Lang::from_tag("fr_FR"), None);
    }
}
