//! Maestro's domain model, with no knowledge of a UI, a transport or a running process.
//!
//! Everything a frontend shows and everything the daemon decides is described by the types here:
//! which coding-agent CLI a worker runs on ([`provider`]), whether an agent is the commander the
//! user talks to or a worker it delegates to ([`agent`]), where a worker's files live
//! ([`target`]), and the graph of work the commander plans ([`workflow`]).

pub mod agent;
pub mod id;
pub mod provider;
pub mod target;
pub mod workflow;

pub use agent::{AgentRole, AgentSpec};
pub use id::{NodeId, WorkflowId};
pub use provider::{Effort, ModelChoice, ModelGrade, ProviderKind};
pub use target::{Host, Workspace, Worktree};
pub use workflow::{Edge, Node, NodeDraft, NodeOrigin, NodeStatus, Workflow, WorkflowError};
