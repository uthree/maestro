//! The daemon's working state on disk, so that it can stop when nothing needs it and pick up
//! where it left off when a frontend next starts it.
//!
//! `state.json` in the daemon's home holds every workflow and every agent's transcript. Agent
//! processes do not survive the daemon: a restored agent has exited, and a node whose worker was
//! still busy when the daemon stopped has failed. A node that finished keeps its worktree and
//! branch, so a later message to it starts a new worker there.

use maestro_core::{AgentEvent, AgentId, AgentSpec, NodeStatus, Workflow};
use serde::{Deserialize, Serialize};

/// Everything in `state.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) workflows: Vec<Workflow>,
    pub(crate) agents: Vec<SavedAgent>,
}

/// An agent as it is remembered: what it was, and what it said.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SavedAgent {
    pub(crate) id: AgentId,
    pub(crate) spec: AgentSpec,
    pub(crate) transcript: Vec<AgentEvent>,
}

/// Why a restored agent is no longer running, unless its transcript already says.
const STOPPED: &str = "the daemon stopped";

impl Snapshot {
    /// Settles what was in flight when the snapshot was taken: busy nodes fail, and every agent
    /// has exited.
    pub(crate) fn settle(mut self) -> Self {
        for flow in &mut self.workflows {
            let busy: Vec<_> = flow
                .nodes()
                .filter(|node| {
                    matches!(node.status, NodeStatus::Running | NodeStatus::AwaitingUser)
                })
                .map(|node| node.id)
                .collect();
            for node in busy {
                flow.set_status(node, NodeStatus::Failed).ok();
            }
        }
        for agent in &mut self.agents {
            if !matches!(agent.transcript.last(), Some(AgentEvent::Exited { .. })) {
                agent.transcript.push(AgentEvent::Exited { reason: Some(STOPPED.into()) });
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use maestro_core::{
        AgentRole, Effort, Host, ModelChoice, ModelGrade, NodeDraft, NodeOrigin, ProviderKind,
        WorkflowId, Workspace,
    };

    use super::*;

    #[test]
    fn settling_fails_busy_nodes_and_ends_every_transcript() {
        let mut flow = Workflow::new(WorkflowId(1), "t");
        let draft = |title: &str| NodeDraft {
            title: title.into(),
            instructions: String::new(),
            model: None,
            difficulty: None,
            origin: NodeOrigin::User,
        };
        let (running, done) = (flow.add_node(draft("a")), flow.add_node(draft("b")));
        flow.set_status(running, NodeStatus::Running).unwrap();
        flow.set_status(done, NodeStatus::Succeeded).unwrap();
        let spec = AgentSpec {
            role: AgentRole::Worker,
            model: ModelChoice {
                provider: ProviderKind::ClaudeCode,
                model: String::new(),
                grade: ModelGrade::Standard,
                effort: Effort::Medium,
            },
            workspace: Workspace { host: Host::Local, repository: "/r".into(), worktree: None },
        };
        let agent = SavedAgent {
            id: AgentId(1),
            spec,
            transcript: vec![AgentEvent::Prompted { text: "go".into() }],
        };

        let text = serde_json::to_string(&Snapshot { workflows: vec![flow], agents: vec![agent] })
            .unwrap();
        let settled = serde_json::from_str::<Snapshot>(&text).unwrap().settle();

        let flow = &settled.workflows[0];
        assert_eq!(flow.node(running).unwrap().status, NodeStatus::Failed);
        assert_eq!(flow.node(done).unwrap().status, NodeStatus::Succeeded);
        assert_eq!(
            settled.agents[0].transcript.last(),
            Some(&AgentEvent::Exited { reason: Some(STOPPED.into()) })
        );
    }
}
