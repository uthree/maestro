//! Choosing which model a worker runs on, and how hard it thinks.
//!
//! The commander rates each node's [`Difficulty`]; the difficulty decides the [`ModelGrade`] to
//! look in and the [`Effort`] to ask for. Within the grade, each model in the [`Catalog`] is scored
//! from two sources:
//!
//! * **Public benchmarks**, as entered in the catalog. These are the prior: they are all there is
//!   to go on for a model that has not worked here yet.
//! * **Its record here** ([`Outcome`]s): whether its nodes succeeded, how long they took, how many
//!   times the user had to ask for changes, the user's rating, and whether the user preferred its
//!   work when it ran against other models. As outcomes accumulate they outweigh the benchmarks,
//!   so routing learns which model actually does well on this user's work. Outcomes on work with
//!   the same tags (the kind of work: `"ui"`, `"tests"`) count for more, so the router learns
//!   what each model is good at, not only how good it is.
//!
//! When there is room, [`Router::lineup`] picks several models for one node instead of one, so
//! their work can be compared ([`Comparison`] says when there is room). The user picks the
//! better result, and the pick goes into the record.
//!
//! [`Router::choose`] and [`Router::lineup`] are pure functions of the catalog, the record and
//! the task, so a choice can be explained and tested.

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
    /// Every difficulty, easiest first.
    pub const ALL: [Self; 5] =
        [Self::Trivial, Self::Easy, Self::Moderate, Self::Hard, Self::Expert];

    /// The name a person reads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Trivial => "Trivial",
            Self::Easy => "Easy",
            Self::Moderate => "Moderate",
            Self::Hard => "Hard",
            Self::Expert => "Expert",
        }
    }

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
    /// The kind of work it was, as the node was tagged.
    #[serde(default)]
    pub tags: Vec<String>,
    /// When the node ran on several models at once: whether the user chose this model's work.
    /// `None` when there was no comparison, or no choice yet.
    #[serde(default)]
    pub chosen: Option<bool>,
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

/// When a node runs on several models at once, so the user can pick the better work.
///
/// Running a node twice costs twice the quota and compute, so it happens only when there is
/// room: the node is hard enough to be worth it, the host is not already running
/// [`Comparison::max_workers`] workers, and each extra model's provider is under its limit in
/// [`Comparison::per_provider`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comparison {
    /// How many models a node runs on when there is room; 1 turns comparison off.
    pub contenders: usize,
    /// Easier nodes always run on one model.
    pub min_difficulty: Difficulty,
    /// The most workers a host runs at once before nodes stop being compared. Work that is not
    /// compared still starts past it.
    pub max_workers: usize,
    /// The most workers each provider runs at once before it stops being added as an extra
    /// model: the user's usage limits. A provider not listed has no limit.
    pub per_provider: BTreeMap<ProviderKind, usize>,
}

impl Default for Comparison {
    fn default() -> Self {
        Self {
            contenders: 2,
            min_difficulty: Difficulty::Moderate,
            max_workers: 4,
            per_provider: BTreeMap::new(),
        }
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
    /// When a node runs on several models at once.
    #[serde(default)]
    pub comparison: Comparison,
}

impl Router {
    /// The best model for a node of `difficulty` and `tags`, among providers `available` on the
    /// host.
    ///
    /// Looks in the difficulty's grade first; with nothing there, in the next grade up, then in
    /// the grades below. Returns `None` only when no catalog model is available at all.
    pub fn choose(
        &self,
        difficulty: Difficulty,
        tags: &[String],
        outcomes: &[Outcome],
        available: &[ProviderKind],
    ) -> Option<Choice> {
        self.ranked(difficulty, tags, outcomes, available).into_iter().next()
    }

    /// The models a node runs on: the best one, and when there is room, the next best to
    /// compare it with, other providers first. `busy` names the provider of every worker the
    /// host is running now, once per worker.
    ///
    /// Empty only when no catalog model is available at all.
    pub fn lineup(
        &self,
        difficulty: Difficulty,
        tags: &[String],
        outcomes: &[Outcome],
        available: &[ProviderKind],
        busy: &[ProviderKind],
    ) -> Vec<Choice> {
        let mut ranked = self.ranked(difficulty, tags, outcomes, available).into_iter();
        let Some(best) = ranked.next() else { return Vec::new() };
        let policy = &self.comparison;
        let room = policy.max_workers.saturating_sub(busy.len());
        let wanted = policy.contenders.min(room);
        if difficulty < policy.min_difficulty || wanted < 2 {
            return vec![best];
        }
        let mut running: BTreeMap<ProviderKind, usize> = BTreeMap::new();
        for provider in busy.iter().chain([&best.model.provider]) {
            *running.entry(*provider).or_default() += 1;
        }
        let rest: Vec<Choice> = ranked.collect();
        let mut lineup = vec![best];
        // Another provider's model tells more about what each is good at than a sibling does.
        for distinct in [true, false] {
            for choice in &rest {
                if lineup.len() == wanted {
                    return lineup;
                }
                let provider = choice.model.provider;
                let taken = lineup.iter().any(|picked| picked.model == choice.model);
                let seen = lineup.iter().any(|picked| picked.model.provider == provider);
                let limit = policy.per_provider.get(&provider).copied().unwrap_or(usize::MAX);
                let used = running.get(&provider).copied().unwrap_or(0);
                if taken || (distinct && seen) || used >= limit {
                    continue;
                }
                *running.entry(provider).or_default() += 1;
                lineup.push(choice.clone());
            }
        }
        lineup
    }

    /// Every available model of the first grade that has any, best first.
    fn ranked(
        &self,
        difficulty: Difficulty,
        tags: &[String],
        outcomes: &[Outcome],
        available: &[ProviderKind],
    ) -> Vec<Choice> {
        let wanted = difficulty.grade();
        let grades = match wanted {
            ModelGrade::Light => [ModelGrade::Light, ModelGrade::Standard, ModelGrade::Frontier],
            ModelGrade::Standard => [ModelGrade::Standard, ModelGrade::Frontier, ModelGrade::Light],
            ModelGrade::Frontier => [ModelGrade::Frontier, ModelGrade::Standard, ModelGrade::Light],
        };
        for grade in grades {
            let mut choices: Vec<Choice> = self
                .catalog
                .models
                .iter()
                .filter(|profile| profile.grade == grade && available.contains(&profile.provider))
                .map(|profile| self.score(profile, difficulty, tags, outcomes))
                .collect();
            if !choices.is_empty() {
                // Stable, so models that tie keep the catalog's order.
                choices.sort_by(|a, b| b.score.total_cmp(&a.score));
                return choices;
            }
        }
        Vec::new()
    }

    fn score(
        &self,
        profile: &ModelProfile,
        difficulty: Difficulty,
        tags: &[String],
        outcomes: &[Outcome],
    ) -> Choice {
        let key = profile.key();
        // Outcomes on harder or easier work, or on another kind of work, say less about this
        // node than outcomes on the same, but they still say something.
        let record: Vec<(f64, &Outcome)> = outcomes
            .iter()
            .filter(|outcome| outcome.model == key)
            .map(|outcome| {
                let distance = (outcome.difficulty as i32 - difficulty as i32).unsigned_abs();
                let shared = tags.iter().filter(|tag| outcome.tags.contains(tag)).count();
                let kind = (1 + shared) as f64 / (1 + tags.len()) as f64;
                (kind / f64::from(1 + distance), outcome)
            })
            .collect();
        let weight: f64 = record.iter().map(|(weight, _)| weight).sum();

        let prior = if profile.benchmarks.is_empty() {
            0.5
        } else {
            profile.benchmarks.values().sum::<f64>() / profile.benchmarks.len() as f64
        };
        let quality = |outcome: &Outcome| {
            // A rating, or picking one model's work over another's, is the user's own judgement
            // of the result, so each counts as much as whether the node technically succeeded.
            let signals = [
                Some(if outcome.succeeded { 1.0 } else { 0.0 }),
                outcome.rating.map(|rating| f64::from(rating.clamp(1, 5) - 1) / 4.0),
                outcome.chosen.map(|chosen| if chosen { 1.0 } else { 0.0 }),
            ];
            let given: Vec<f64> = signals.into_iter().flatten().collect();
            given.iter().sum::<f64>() / given.len() as f64
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
            tags: Vec::new(),
            chosen: None,
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
            comparison: Comparison::default(),
        }
    }

    const ALL: &[ProviderKind] = &ProviderKind::ALL;
    const NONE: &[String] = &[];

    fn names(lineup: &[Choice]) -> Vec<&str> {
        lineup.iter().map(|choice| choice.model.model.as_str()).collect()
    }

    #[test]
    fn difficulty_picks_the_grade_and_the_effort() {
        let hard = router().choose(Difficulty::Hard, NONE, &[], ALL).unwrap();
        assert_eq!((hard.model.model.as_str(), hard.model.effort), ("big-a", Effort::High));
        let easy = router().choose(Difficulty::Easy, NONE, &[], ALL).unwrap();
        assert_eq!((easy.model.model.as_str(), easy.model.effort), ("small-a", Effort::Low));
        // No standard model: moderate work goes up to frontier rather than down to light.
        let moderate = router().choose(Difficulty::Moderate, NONE, &[], ALL).unwrap();
        assert_eq!(moderate.model.grade, ModelGrade::Frontier);
    }

    #[test]
    fn the_record_here_overtakes_the_benchmarks() {
        let mut record = Vec::new();
        for _ in 0..4 {
            record.push(outcome("big-a", ProviderKind::ClaudeCode, false, 0));
            record.push(outcome("big-b", ProviderKind::Codex, true, 0));
        }
        let choice = router().choose(Difficulty::Hard, NONE, &record, ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");
        assert_eq!(choice.evidence, 4);
    }

    #[test]
    fn revision_requests_and_low_ratings_count_against_a_model() {
        let fussy = vec![outcome("big-a", ProviderKind::ClaudeCode, true, 3); 2];
        let clean = vec![outcome("big-b", ProviderKind::Codex, true, 0); 2];
        let choice =
            router().choose(Difficulty::Hard, NONE, &[fussy, clean].concat(), ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");

        let mut disliked = outcome("big-a", ProviderKind::ClaudeCode, true, 0);
        disliked.rating = Some(1);
        let choice = router().choose(Difficulty::Hard, NONE, &vec![disliked; 3], ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");
    }

    #[test]
    fn unavailable_providers_are_skipped() {
        let choice = router().choose(Difficulty::Hard, NONE, &[], &[ProviderKind::Codex]).unwrap();
        assert_eq!(choice.model.model, "big-b");
        assert!(router().choose(Difficulty::Hard, NONE, &[], &[ProviderKind::OpenCode]).is_none());
    }

    #[test]
    fn provider_defaults_are_a_level_field() {
        let router = Router { catalog: Catalog::provider_defaults(), ..Router::default() };
        let choice = router.choose(Difficulty::Trivial, NONE, &[], ALL).unwrap();
        assert_eq!((choice.model.grade, choice.model.effort), (ModelGrade::Standard, Effort::Low));
        assert_eq!(choice.score, 0.5);
    }

    #[test]
    fn hard_work_is_compared_across_providers_when_there_is_room() {
        let mut router = router();
        router.catalog.models.push(profile(
            ProviderKind::ClaudeCode,
            "big-c",
            ModelGrade::Frontier,
            0.72,
        ));
        let lineup = router.lineup(Difficulty::Hard, NONE, &[], ALL, &[]);
        // big-c outscores big-b, but another provider's model is the better comparison.
        assert_eq!(names(&lineup), ["big-a", "big-b"]);
        router.comparison.contenders = 3;
        assert_eq!(
            names(&router.lineup(Difficulty::Hard, NONE, &[], ALL, &[])),
            ["big-a", "big-b", "big-c"]
        );
        // Easy work, or work on one model only, is not compared.
        assert_eq!(names(&router.lineup(Difficulty::Easy, NONE, &[], ALL, &[])), ["small-a"]);
        router.comparison.contenders = 1;
        assert_eq!(names(&router.lineup(Difficulty::Hard, NONE, &[], ALL, &[])), ["big-a"]);
    }

    #[test]
    fn a_busy_host_or_a_provider_at_its_limit_runs_one_model() {
        let mut router = router();
        let busy = [ProviderKind::Codex; 3];
        // Four workers at most: three running leaves room for one more, not two.
        assert_eq!(names(&router.lineup(Difficulty::Hard, NONE, &[], ALL, &busy)), ["big-a"]);
        assert_eq!(
            names(&router.lineup(Difficulty::Hard, NONE, &[], ALL, &busy[..2])),
            ["big-a", "big-b"]
        );
        router.comparison.per_provider.insert(ProviderKind::Codex, 2);
        assert_eq!(names(&router.lineup(Difficulty::Hard, NONE, &[], ALL, &busy[..2])), ["big-a"]);
        // The best model always runs, whatever the limits.
        router.comparison.per_provider.insert(ProviderKind::ClaudeCode, 0);
        assert_eq!(names(&router.lineup(Difficulty::Hard, NONE, &[], ALL, &busy)), ["big-a"]);
        assert!(router.lineup(Difficulty::Hard, NONE, &[], &[], &[]).is_empty());
    }

    #[test]
    fn losing_comparisons_counts_against_a_model() {
        let mut record = Vec::new();
        for _ in 0..3 {
            record.push(Outcome {
                chosen: Some(false),
                ..outcome("big-a", ProviderKind::ClaudeCode, true, 0)
            });
            record.push(Outcome {
                chosen: Some(true),
                ..outcome("big-b", ProviderKind::Codex, true, 0)
            });
        }
        let choice = router().choose(Difficulty::Hard, NONE, &record, ALL).unwrap();
        assert_eq!(choice.model.model, "big-b");
    }

    #[test]
    fn the_router_learns_what_each_model_is_good_at() {
        let tagged = |model: &str, provider, tag: &str, chosen: bool| Outcome {
            tags: vec![tag.to_owned()],
            chosen: Some(chosen),
            ..outcome(model, provider, true, 0)
        };
        let mut record = Vec::new();
        for _ in 0..3 {
            record.push(tagged("big-a", ProviderKind::ClaudeCode, "ui", true));
            record.push(tagged("big-b", ProviderKind::Codex, "ui", false));
            record.push(tagged("big-a", ProviderKind::ClaudeCode, "tests", false));
            record.push(tagged("big-b", ProviderKind::Codex, "tests", true));
        }
        let pick = |tag: &str| {
            let tags = [tag.to_owned()];
            router().choose(Difficulty::Hard, &tags, &record, ALL).unwrap().model.model
        };
        assert_eq!((pick("ui").as_str(), pick("tests").as_str()), ("big-a", "big-b"));
    }
}
