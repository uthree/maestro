//! Choosing which model a worker runs on, and how hard it thinks.
//!
//! The commander rates each node's [`Difficulty`]; the difficulty decides the [`ModelGrade`] to
//! look in and the [`Effort`] to ask for. Within the grade, each model in the [`Catalog`] is scored
//! from two sources:
//!
//! * **Public benchmarks**, as entered in the catalog. These are the prior: they are all there is
//!   to go on for a model that has not worked here yet.
//! * **Its record here** ([`Outcome`]s): whether its nodes succeeded, how long they took, how many
//!   times the user had to ask for changes, and the user's rating. As outcomes accumulate they
//!   outweigh the benchmarks, so routing learns which model actually does well on this user's
//!   work.
//!
//! [`Router::choose`] is a pure function of the catalog, the record and the difficulty, so a
//! choice can be explained and tested.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::provider::{Effort, ModelChoice, ModelGrade, ProviderKind};

/// How hard a node is, as the commander judges it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    /// A mechanical change: a rename, a version bump, a lookup.
    Trivial,
    /// Small and well specified.
    Easy,
    /// Ordinary feature or bug work.
    Moderate,
    /// Touches design, many files, or subtle behaviour.
    Hard,
    /// Open-ended design or a bug nobody understands yet.
    Expert,
}

impl Difficulty {
    /// The grade of model a node of this difficulty runs on.
    pub fn grade(self) -> ModelGrade {
        match self {
            Self::Trivial | Self::Easy => ModelGrade::Light,
            Self::Moderate => ModelGrade::Standard,
            Self::Hard | Self::Expert => ModelGrade::Frontier,
        }
    }

    /// The effort a node of this difficulty asks for.
    pub fn effort(self) -> Effort {
        match self {
            Self::Trivial | Self::Easy => Effort::Low,
            Self::Moderate => Effort::Medium,
            Self::Hard => Effort::High,
            Self::Expert => Effort::Max,
        }
    }
}

/// One model Maestro may route work to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// The CLI that runs it.
    pub provider: ProviderKind,
    /// The provider's model name; empty for the CLI's own default.
    pub model: String,
    /// Its grade.
    pub grade: ModelGrade,
    /// Published benchmark results, each scaled to 0–1 (a pass rate as a fraction), by benchmark
    /// name. Only benchmarks every compared model reports make a fair comparison, so the score
    /// uses the mean over whatever each model has.
    #[serde(default)]
    pub benchmarks: BTreeMap<String, f64>,
}

impl ModelProfile {
    fn key(&self) -> ModelKey {
        ModelKey { provider: self.provider, model: self.model.clone() }
    }
}

/// Every model Maestro may route work to. It lives in the daemon's configuration, so the user
/// decides which models exist and enters the benchmark results they trust.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// The models.
    pub models: Vec<ModelProfile>,
}

impl Catalog {
    /// Each provider's own default model at the standard grade, with no benchmarks: routing then
    /// starts from a level field and learns only from outcomes.
    pub fn provider_defaults() -> Self {
        let models = ProviderKind::ALL
            .into_iter()
            .map(|provider| ModelProfile {
                provider,
                model: String::new(),
                grade: ModelGrade::Standard,
                benchmarks: BTreeMap::new(),
            })
            .collect();
        Self { models }
    }
}

/// Names a model across catalog edits.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ModelKey {
    /// The CLI.
    pub provider: ProviderKind,
    /// The model name.
    pub model: String,
}

/// How one node went, recorded when it finishes and amended when the user rates it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// The model that did the work.
    pub model: ModelKey,
    /// How hard the node was.
    pub difficulty: Difficulty,
    /// Whether it succeeded.
    pub succeeded: bool,
    /// Wall-clock seconds from start to finish.
    pub seconds: f64,
    /// How many times the user asked the worker for changes.
    pub revisions: u32,
    /// The user's rating, 1–5, if they gave one.
    pub rating: Option<u8>,
}

/// How strongly each signal counts. The defaults are a starting point, not a measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    /// How many outcomes the benchmark prior is worth: with this many outcomes on record, the
    /// record and the benchmarks count equally.
    pub prior_strength: f64,
    /// Score lost per revision request, averaged over outcomes.
    pub per_revision: f64,
    /// Score lost per doubling of the average time past one minute.
    pub per_doubling: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self { prior_strength: 3.0, per_revision: 0.1, per_doubling: 0.02 }
    }
}

/// Why a model was chosen, for the commander's log and the user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// The model, effort included.
    pub model: ModelChoice,
    /// Its score.
    pub score: f64,
    /// How many outcomes on record informed it.
    pub evidence: usize,
}

/// Picks models.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Router {
    /// The models to choose from.
    pub catalog: Catalog,
    /// How strongly each signal counts.
    pub weights: Weights,
}

impl Router {
    /// The best model for a node of `difficulty`, among providers `available` on the host.
    ///
    /// Looks in the difficulty's grade first; with nothing there, in the next grade up, then in
    /// the grades below. Returns `None` only when no catalog model is available at all.
    pub fn choose(
        &self,
        difficulty: Difficulty,
        outcomes: &[Outcome],
        available: &[ProviderKind],
    ) -> Option<Choice> {
        let wanted = difficulty.grade();
        let grades = match wanted {
            ModelGrade::Light => [ModelGrade::Light, ModelGrade::Standard, ModelGrade::Frontier],
            ModelGrade::Standard => [ModelGrade::Standard, ModelGrade::Frontier, ModelGrade::Light],
            ModelGrade::Frontier => [ModelGrade::Frontier, ModelGrade::Standard, ModelGrade::Light],
        };
        grades.into_iter().find_map(|grade| {
            self.catalog
                .models
                .iter()
                .filter(|profile| profile.grade == grade && available.contains(&profile.provider))
                .map(|profile| self.score(profile, difficulty, outcomes))
                .max_by(|a, b| a.score.total_cmp(&b.score))
        })
    }

    fn score(
        &self,
        profile: &ModelProfile,
        difficulty: Difficulty,
        outcomes: &[Outcome],
    ) -> Choice {
        let key = profile.key();
        // Outcomes on harder or easier work say less about this node than outcomes on the same
        // difficulty, but they still say something.
        let record: Vec<(f64, &Outcome)> = outcomes
            .iter()
            .filter(|outcome| outcome.model == key)
            .map(|outcome| {
                let distance = (outcome.difficulty as i32 - difficulty as i32).unsigned_abs();
                (1.0 / f64::from(1 + distance), outcome)
            })
            .collect();
        let weight: f64 = record.iter().map(|(weight, _)| weight).sum();

        let prior = if profile.benchmarks.is_empty() {
            0.5
        } else {
            profile.benchmarks.values().sum::<f64>() / profile.benchmarks.len() as f64
        };
        let quality = |outcome: &Outcome| {
            let success = if outcome.succeeded { 1.0 } else { 0.0 };
            match outcome.rating {
                // A rating is the user's own judgement of the result, so it counts as much as
                // whether the node technically succeeded.
                Some(rating) => (success + f64::from(rating.clamp(1, 5) - 1) / 4.0) / 2.0,
                None => success,
            }
        };
        let observed: f64 = record.iter().map(|(weight, outcome)| weight * quality(outcome)).sum();
        let strength = self.weights.prior_strength;
        let mut score = (prior * strength + observed) / (strength + weight);

        if weight > 0.0 {
            let mean = |value: &dyn Fn(&Outcome) -> f64| {
                record.iter().map(|(weight, outcome)| weight * value(outcome)).sum::<f64>() / weight
            };
            score -= self.weights.per_revision * mean(&|outcome| f64::from(outcome.revisions));
            let minutes = mean(&|outcome| outcome.seconds / 60.0);
            score -= self.weights.per_doubling * minutes.max(1.0).log2();
        }

        Choice {
            model: ModelChoice {
                provider: profile.provider,
                model: profile.model.clone(),
                grade: profile.grade,
                effort: difficulty.effort(),
            },
            score,
            evidence: record.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(provider: ProviderKind, model: &str, grade: ModelGrade, bench: f64) -> ModelProfile {
        ModelProfile {
            provider,
            model: model.into(),
            grade,
            benchmarks: BTreeMap::from([("swe".to_owned(), bench)]),
        }
    }

    fn outcome(model: &str, provider: ProviderKind, succeeded: bool, revisions: u32) -> Outcome {
        Outcome {
            model: ModelKey { provider, model: model.into() },
            difficulty: Difficulty::Hard,
            succeeded,
            seconds: 120.0,
            revisions,
            rating: None,
        }
    }

    fn router() -> Router {
        Router {
            catalog: Catalog {
                models: vec![
                    profile(ProviderKind::ClaudeCode, "big-a", ModelGrade::Frontier, 0.75),
                    profile(ProviderKind::Codex, "big-b", ModelGrade::Frontier, 0.70),
                    profile(ProviderKind::ClaudeCode, "small-a", ModelGrade::Light, 0.40),
                ],
            },
            weights: Weights::default(),
        }
    }

    const ALL: &[ProviderKind] = &ProviderKind::ALL;

    #[test]
    fn difficulty_picks_the_grade_and_the_effort() {
        let hard = router().choose(Difficulty::Hard, &[], ALL).unwrap();
        assert_eq!((hard.model.model.as_str(), hard.model.effort), ("big-a", Effort::High));
        let easy = router().choose(Difficulty::Easy, &[], ALL).unwrap();
        assert_eq!((easy.model.model.as_str(), easy.model.effort), ("small-a", Effort::Low));
        // No standard model: moderate work goes up to frontier rather than down to light.
        let moderate = router().choose(Difficulty::Moderate, &[], ALL).unwrap();
        assert_eq!(moderate.model.grade, ModelGrade::Frontier);
    }

    #[test]
    fn the_record_here_overtakes_the_benchmarks() {
        let mut record = Vec::new();
        for _ in 0..4 {
            record.push(outcome("big-a", ProviderKind::ClaudeCode, false, 0));
            record.push(outcome("big-b", ProviderKind::Codex, true, 0));
        }
        let choice = router().choose(Difficulty::Hard, &record, ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");
        assert_eq!(choice.evidence, 4);
    }

    #[test]
    fn revision_requests_and_low_ratings_count_against_a_model() {
        let fussy = vec![outcome("big-a", ProviderKind::ClaudeCode, true, 3); 2];
        let clean = vec![outcome("big-b", ProviderKind::Codex, true, 0); 2];
        let choice = router().choose(Difficulty::Hard, &[fussy, clean].concat(), ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");

        let mut disliked = outcome("big-a", ProviderKind::ClaudeCode, true, 0);
        disliked.rating = Some(1);
        let choice = router().choose(Difficulty::Hard, &vec![disliked; 3], ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");
    }

    #[test]
    fn unavailable_providers_are_skipped() {
        let choice = router().choose(Difficulty::Hard, &[], &[ProviderKind::Codex]).unwrap();
        assert_eq!(choice.model.model, "big-b");
        assert!(router().choose(Difficulty::Hard, &[], &[ProviderKind::OpenCode]).is_none());
    }

    #[test]
    fn provider_defaults_are_a_level_field() {
        let router = Router { catalog: Catalog::provider_defaults(), weights: Weights::default() };
        let choice = router.choose(Difficulty::Trivial, &[], ALL).unwrap();
        assert_eq!((choice.model.grade, choice.model.effort), (ModelGrade::Standard, Effort::Low));
        assert_eq!(choice.score, 0.5);
    }
}
