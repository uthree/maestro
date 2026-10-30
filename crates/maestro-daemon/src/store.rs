//! What the daemon remembers between runs: the router's catalog and the record of outcomes.
//!
//! Both are JSON files in the daemon's home (`$MAESTRO_HOME`, or `~/.maestro`). `router.json`
//! is the user's to edit: which models exist, their grades, the benchmark results they trust, and
//! how strongly each signal counts. A missing one is written out with each provider's default
//! model, so there is something to edit. `outcomes.json` is the daemon's own record.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use maestro_core::{Catalog, Outcome, Router, Weights};
use serde::{Serialize, de::DeserializeOwned};

/// The router and the record, saved to `home` when there is one.
pub(crate) struct Store {
    home: Option<PathBuf>,
    pub(crate) router: Router,
    outcomes: BTreeMap<String, Outcome>,
}

impl Store {
    /// A store that forgets everything when the daemon exits.
    pub(crate) fn memory() -> Self {
        Self {
            home: None,
            router: Router { catalog: Catalog::provider_defaults(), weights: Weights::default() },
            outcomes: BTreeMap::new(),
        }
    }

    /// Loads from `home`, creating it and a template `router.json` if they are missing.
    pub(crate) fn open(home: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(home)?;
        let mut store = Self::memory();
        store.home = Some(home.to_owned());
        match read(&home.join("router.json"))? {
            Some(router) => store.router = router,
            None => write(&home.join("router.json"), &store.router)?,
        }
        if let Some(outcomes) = read(&home.join("outcomes.json"))? {
            store.outcomes = outcomes;
        }
        Ok(store)
    }

    /// Every outcome on record.
    pub(crate) fn outcomes(&self) -> Vec<Outcome> {
        self.outcomes.values().cloned().collect()
    }

    /// Records how a node went, replacing what an earlier attempt at it recorded but keeping the
    /// user's rating.
    pub(crate) fn record(&mut self, key: String, mut outcome: Outcome) {
        if let Some(earlier) = self.outcomes.get(&key) {
            outcome.rating = outcome.rating.or(earlier.rating);
        }
        self.outcomes.insert(key, outcome);
        self.save();
    }

    /// Adds the user's rating to a node's outcome. Returns whether the node has one.
    pub(crate) fn rate(&mut self, key: &str, rating: u8) -> bool {
        let Some(outcome) = self.outcomes.get_mut(key) else { return false };
        outcome.rating = Some(rating);
        self.save();
        true
    }

    fn save(&self) {
        if let Some(home) = &self.home
            && let Err(error) = write(&home.join("outcomes.json"), &self.outcomes)
        {
            eprintln!("maestro-daemon: could not save outcomes: {error}");
        }
    }
}

pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes through a temporary file, so a crash never leaves half a file behind.
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
    std::fs::rename(temporary, path)
}

#[cfg(test)]
mod tests {
    use maestro_core::{Difficulty, ModelKey, ProviderKind};

    use super::*;

    #[test]
    fn outcomes_and_ratings_survive_a_restart() {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(home.path()).unwrap();
        assert!(home.path().join("router.json").exists(), "a template is written to edit");
        let outcome = Outcome {
            model: ModelKey { provider: ProviderKind::Codex, model: String::new() },
            difficulty: Difficulty::Easy,
            succeeded: true,
            seconds: 30.0,
            revisions: 0,
            rating: None,
        };
        store.record("wf1/n1".into(), outcome.clone());
        assert!(store.rate("wf1/n1", 4));
        assert!(!store.rate("wf1/n9", 4));
        // A later attempt replaces the outcome but keeps the rating.
        store.record("wf1/n1".into(), Outcome { revisions: 1, ..outcome });

        let reopened = Store::open(home.path()).unwrap();
        let outcomes = reopened.outcomes();
        assert_eq!((outcomes[0].revisions, outcomes[0].rating), (1, Some(4)));
        assert_eq!(reopened.router.catalog.models.len(), 4);
    }
}
