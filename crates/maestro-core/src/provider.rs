//! The coding-agent CLIs a worker can run on, and how hard it is asked to think.

use serde::{Deserialize, Serialize};

/// A coding-agent CLI Maestro drives as a subprocess.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic's `claude`.
    ClaudeCode,
    /// OpenAI's `codex`.
    Codex,
    /// Google's Antigravity CLI, `agy`.
    Antigravity,
    /// The open-source `opencode`, which itself fronts many model vendors.
    OpenCode,
}

impl ProviderKind {
    /// Every provider, in the order a settings page lists them.
    pub const ALL: [Self; 4] = [Self::ClaudeCode, Self::Codex, Self::Antigravity, Self::OpenCode];

    /// The name a person reads.
    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Antigravity => "Antigravity CLI",
            Self::OpenCode => "OpenCode",
        }
    }

    /// The executable looked up on the host's `PATH`.
    pub fn executable(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode",
        }
    }
}

/// How capable (and how expensive) a model is, independent of vendor.
///
/// The commander picks a grade from a task's difficulty first and a concrete model second, so
/// routing can move between vendors without rewriting its rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelGrade {
    /// Fast and cheap; mechanical edits, lookups, summaries.
    Light,
    /// The everyday default.
    Standard,
    /// The strongest model available, for design work and hard bugs.
    Frontier,
}

/// Reasoning effort requested of a model that supports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    /// Answer quickly.
    Low,
    /// The provider's default.
    Medium,
    /// Think longer before acting.
    High,
    /// The most the provider offers.
    Max,
}

impl Effort {
    /// The name a person reads.
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low effort",
            Self::Medium => "Medium effort",
            Self::High => "High effort",
            Self::Max => "Max effort",
        }
    }
}

/// A concrete model on a concrete provider, at a chosen effort.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelChoice {
    /// The CLI that runs it.
    pub provider: ProviderKind,
    /// The provider's own model name, passed through unchanged.
    pub model: String,
    /// Grade the model was chosen as, kept so evaluation can compare like with like.
    pub grade: ModelGrade,
    /// Effort requested.
    pub effort: Effort,
}
