//! Maestro's domain model, with no knowledge of a UI, a transport or a running process.
//!
//! Everything a frontend shows and everything the daemon decides is described by the types here:
//! which coding-agent CLI a worker runs on ([`provider`]), whether an agent is the commander the
//! user talks to or a worker it delegates to ([`agent`]), where a worker's files live
//! ([`target`]), the graph of work the commander plans ([`workflow`]), and what a running agent
//! reports, whichever CLI it runs on ([`event`]), and how a worker's model is chosen
//! ([`routing`]). [`plan`] is how the commander's plans become workflows.

pub mod agent;
pub mod event;
pub mod id;
pub mod plan;
pub mod provider;
pub mod routing;
pub mod target;
pub mod workflow;

pub use agent::{AgentRole, AgentSpec};
pub use event::{AgentEvent, PermissionKind, PermissionRequest, ToolCall};
pub use id::{AgentId, NodeId, WorkflowId};
pub use plan::{
    COMMANDER_INSTRUCTIONS, Plan, PlanError, PlanNode, WORKER_INSTRUCTIONS, status_report,
};
pub use provider::{Effort, ModelChoice, ModelGrade, ProviderKind};
pub use routing::{
    Catalog, Choice, Comparison, Difficulty, ModelKey, ModelProfile, Outcome, Router, Weights,
};
pub use target::{Host, Workspace, Worktree};
pub use workflow::{
    Contender, Edge, Node, NodeDraft, NodeEdit, NodeOrigin, NodeRef, NodeStatus, Workflow,
    WorkflowError, WorkflowTarget,
};
