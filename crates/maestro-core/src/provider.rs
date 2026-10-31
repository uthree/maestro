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

    /// The shell script that installs the CLI on a machine that lacks it, as each project
    /// documents it.
    pub fn install_script(self) -> &'static str {
        match self {
            Self::ClaudeCode => "curl -fsSL https://claude.ai/install.sh | bash",
            Self::Codex => "npm install -g @openai/codex",
            Self::Antigravity => "curl -fsSL https://antigravity.google/install.sh | bash",
            Self::OpenCode => "curl -fsSL https://opencode.ai/install | bash",
        }
    }

    /// The shell script that brings an installed CLI up to its latest release.
    pub fn update_script(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude update",
            Self::Codex => "npm install -g @openai/codex@latest",
            // Its installer replaces an existing install with the latest release.
            Self::Antigravity => self.install_script(),
            Self::OpenCode => "opencode upgrade",
        }
    }

    /// The script for `action`.
    pub fn setup_script(self, action: SetupAction) -> &'static str {
        match action {
            SetupAction::Install => self.install_script(),
            SetupAction::Update => self.update_script(),
        }
    }

    /// What a person runs in a terminal on the host to sign the CLI in to their account. Each
    /// CLI asks for it interactively, so Maestro shows it rather than running it.
    pub fn sign_in(self) -> &'static str {
        match self {
            // Its first run asks to sign in.
            Self::ClaudeCode => "claude",
            Self::Codex => "codex login",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode auth login",
        }
    }

    /// Models a person can pick for this CLI without listing them in the router's catalog:
    /// names the CLI itself documents as stable, with their grade. Only Claude Code publishes
    /// aliases that follow its newest models; for the others, the catalog is the list.
    pub fn known_models(self) -> &'static [(&'static str, ModelGrade)] {
        match self {
            Self::ClaudeCode => &[
                ("opus", ModelGrade::Frontier),
                ("sonnet", ModelGrade::Standard),
                ("haiku", ModelGrade::Light),
            ],
            Self::Codex | Self::Antigravity | Self::OpenCode => &[],
        }
    }
}

/// What to do to a provider's CLI on a host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupAction {
    /// Install it.
    Install,
    /// Update it to the latest release.
    Update,
}

/// Whether a provider's CLI is installed on a host, and which release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    /// The provider.
    pub provider: ProviderKind,
    /// Where its executable is, when it is on the host's `PATH`.
    pub path: Option<String>,
    /// What `--version` printed, first line only.
    pub version: Option<String>,
    /// An install or update running now.
    pub setup: Option<SetupAction>,
}

impl ProviderStatus {
    /// Whether the CLI was found.
    pub fn installed(&self) -> bool {
        self.path.is_some()
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
