//! The kinds of agent, kept apart on purpose.
//!
//! The commander talks with the user and plans; it never edits files. Workers carry out one node
//! of a plan each and never talk to the user unless the user writes to their node. Keeping the
//! roles separate is what lets a cheap model do mechanical work while a strong one plans, and
//! keeps the user's conversation free of tool noise.
//!
//! A node too big for one worker goes to a lead instead: an agent that plans like the commander,
//! but only that node, as a workflow of its own under the node ([`Workflow::parent`]). A lead's
//! nodes may go to leads in turn, so the organisation is as deep as the work needs; the only
//! bound is how many agents run at once ([`Router::max_agents`]).
//!
//! [`Workflow::parent`]: crate::Workflow::parent
//! [`Router::max_agents`]: crate::Router::max_agents

use serde::{Deserialize, Serialize};

use crate::id::ProjectId;
use crate::provider::ModelChoice;
use crate::target::Workspace;

/// Which side of the split an agent is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Converses with the user and writes workflows. Has no write access to workspaces.
    Commander,
    /// Executes one workflow node inside its own workspace.
    Worker,
    /// Plans one workflow node as a workflow of its own. Like the commander, it has no write
    /// access to workspaces.
    Lead,
}

impl AgentRole {
    /// Whether it plans rather than works: started read-only, with every permission it asks
    /// for refused.
    pub fn plans(self) -> bool {
        matches!(self, Self::Commander | Self::Lead)
    }
}

/// Everything needed to start an agent process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Commander or worker.
    pub role: AgentRole,
    /// Provider, model and effort.
    pub model: ModelChoice,
    /// Where it runs and which files it may touch.
    pub workspace: Workspace,
    /// The project it works for: the one a commander leads, or the one a worker's node is in.
    #[serde(default)]
    pub project: Option<ProjectId>,
}
