//! The daemon's working state on disk, so that it can stop when nothing needs it and pick up
//! where it left off when a frontend next starts it.
//!
//! `state.json` in the daemon's home holds every project, every workflow and every agent's
//! transcript. Agent
//! processes do not survive the daemon: a restored agent has exited, and a node whose worker was
//! still busy when the daemon stopped has failed. A node that finished keeps its worktree and
//! branch, so a later message to it starts a new worker there. A delegated node whose lead had
//! planned it keeps waiting for the work under it, which carries on without the lead.

use maestro_core::{AgentEvent, AgentId, AgentSpec, NodeRef, NodeStatus, Project, Workflow};
use serde::{Deserialize, Serialize};

/// Everything in `state.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    // A state.json saved before projects has none.
    #[serde(default)]
    pub(crate) projects: Vec<Project>,
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
        // A delegated node with work planned under it waits for that work, not for its lead.
        let planned: Vec<NodeRef> = self.workflows.iter().filter_map(|flow| flow.parent).collect();
        for flow in &mut self.workflows {
            let id = flow.id;
            let busy: Vec<_> = flow
                .nodes()
                .filter(|node| {
                    matches!(node.status, NodeStatus::Running | NodeStatus::AwaitingUser)
                })
                .filter(|node| {
                    let reference = NodeRef { workflow: id, node: node.id };
                    !(node.status == NodeStatus::Running && planned.contains(&reference))
                })
                .map(|node| node.id)
                .collect();
            for node in busy {
                // A compared node keeps the work its contenders finished, for the user to pick.
                let contenders: Vec<_> = flow
                    .node(node)
                    .filter(|node| node.agent.is_none())
                    .map(|node| node.contenders.iter().map(|c| (c.agent, c.status)).collect())
                    .unwrap_or_default();
                if contenders.is_empty() {
                    flow.set_status(node, NodeStatus::Failed).ok();
                }
                for (agent, status) in contenders {
                    if matches!(status, NodeStatus::Running | NodeStatus::AwaitingUser) {
                        flow.set_contender_status(node, agent, NodeStatus::Failed).ok();
                    }
                }
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
            tags: Vec::new(),
            delegate: false,
            origin: NodeOrigin::User,
        };
        let (running, done) = (flow.add_node(draft("a")), flow.add_node(draft("b")));
        flow.set_status(running, NodeStatus::Running).unwrap();
        flow.set_status(done, NodeStatus::Succeeded).unwrap();
        // A delegated node whose lead planned it, and one whose lead had not yet.
        let (planned, planning) = (flow.add_node(draft("c")), flow.add_node(draft("d")));
        flow.set_status(planned, NodeStatus::Running).unwrap();
        flow.set_status(planning, NodeStatus::Running).unwrap();
        let mut under = Workflow::new(WorkflowId(2), "under c");
        under.parent = Some(NodeRef { workflow: WorkflowId(1), node: planned });
        let spec = AgentSpec {
            role: AgentRole::Worker,
            model: ModelChoice {
                provider: ProviderKind::ClaudeCode,
                model: String::new(),
                grade: ModelGrade::Standard,
                effort: Effort::Medium,
            },
            workspace: Workspace { host: Host::Local, repository: "/r".into(), worktree: None },
            project: None,
        };
        let agent = SavedAgent {
            id: AgentId(1),
            spec,
            transcript: vec![AgentEvent::Prompted { text: "go".into() }],
        };

        let text = serde_json::to_string(&Snapshot {
            projects: Vec::new(),
            workflows: vec![flow, under],
            agents: vec![agent],
        })
        .unwrap();
        let settled = serde_json::from_str::<Snapshot>(&text).unwrap().settle();

        let flow = &settled.workflows[0];
        assert_eq!(flow.node(running).unwrap().status, NodeStatus::Failed);
        assert_eq!(flow.node(done).unwrap().status, NodeStatus::Succeeded);
        assert_eq!(flow.node(planned).unwrap().status, NodeStatus::Running);
        assert_eq!(flow.node(planning).unwrap().status, NodeStatus::Failed);
        assert_eq!(
            settled.agents[0].transcript.last(),
            Some(&AgentEvent::Exited { reason: Some(STOPPED.into()) })
        );
    }
}
