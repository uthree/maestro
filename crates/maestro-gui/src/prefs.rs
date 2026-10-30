//! What this window remembers about the person using it: the language and the look.
//!
//! These belong to the computer the window is on, not to the daemon, which may be on another
//! machine and serve other people's windows. They are kept in `preferences.json` in
//! `$MAESTRO_HOME`, or `~/.maestro`, on this computer. What the command line gives wins for that
//! run and is not saved.

use std::path::PathBuf;

use atelier::{Appearance, Direction};
use serde::{Deserialize, Serialize};

use crate::i18n::Lang;

/// The saved choices. A missing one follows the system or the built-in default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// `en` or `ja`; none for the system's language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// `dark` or `light`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appearance: Option<String>,
    /// `glass`, `studio` or `mono`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

impl Preferences {
    /// What was saved, or nothing when there is no file or it cannot be read.
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the choices out.
    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("HOME is not set"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, bytes)
    }

    /// The saved language.
    pub fn lang(&self) -> Option<Lang> {
        self.lang.as_deref().and_then(Lang::from_tag)
    }

    /// The saved appearance.
    pub fn appearance(&self) -> Option<Appearance> {
        match self.appearance.as_deref() {
            Some("light") => Some(Appearance::Light),
            Some("dark") => Some(Appearance::Dark),
            _ => None,
        }
    }

    /// The saved direction.
    pub fn direction(&self) -> Option<Direction> {
        self.direction.as_deref().and_then(Direction::from_id)
    }

    /// Remembers an appearance.
    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = Some(if appearance.is_dark() { "dark" } else { "light" }.into());
    }

    /// Remembers a direction.
    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = Some(direction.id().into());
    }
}

fn path() -> Option<PathBuf> {
    let home = std::env::var_os("MAESTRO_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| PathBuf::from(home).join(".maestro"))
    })?;
    Some(home.join("preferences.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_round_trip_and_unknown_values_fall_back() {
        let mut preferences = Preferences { lang: Some("ja".into()), ..Preferences::default() };
        preferences.set_appearance(Appearance::Light);
        preferences.set_direction(Direction::Mono);
        let text = serde_json::to_string(&preferences).unwrap();
        let read: Preferences = serde_json::from_str(&text).unwrap();
        assert_eq!(read.lang(), Some(Lang::Ja));
        assert_eq!(read.appearance(), Some(Appearance::Light));
        assert_eq!(read.direction(), Some(Direction::Mono));

        let odd: Preferences = serde_json::from_str(r#"{"lang":"fr","extra":1}"#).unwrap();
        assert_eq!((odd.lang(), odd.appearance(), odd.direction()), (None, None, None));
    }
}
