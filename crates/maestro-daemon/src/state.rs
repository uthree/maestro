use std::collections::BTreeMap;

use maestro_core::{NodeOrigin, NodeRef, NodeStatus, Workflow, WorkflowError, WorkflowId};
use maestro_protocol::{Command, CommandError, Event, Reply, WorkflowSummary};

/// Every workflow the daemon knows about.
#[derive(Debug, Default)]
pub struct State {
    workflows: BTreeMap<WorkflowId, Workflow>,
    next_workflow: u64,
}

impl State {
    /// The state holding `workflows`, as a snapshot saved them; new workflows get later ids.
    pub(crate) fn restore(workflows: Vec<Workflow>) -> Self {
        let next_workflow = workflows.iter().map(|flow| flow.id.0).max().unwrap_or(0);
        let workflows = workflows.into_iter().map(|flow| (flow.id, flow)).collect();
        Self { workflows, next_workflow }
    }

    /// Every workflow, for a snapshot.
    pub(crate) fn workflows(&self) -> Vec<Workflow> {
        self.workflows.values().cloned().collect()
    }

    /// Applies one command. On failure nothing has changed and no events are returned.
    pub fn apply(&mut self, command: Command) -> (Result<Reply, CommandError>, Vec<Event>) {
        match self.dispatch(command) {
            Ok((reply, changed)) => {
                let events = changed
                    .into_iter()
                    .map(|id| Event::WorkflowChanged { workflow: self.workflows[&id].clone() })
                    .collect();
                (Ok(reply), events)
            }
            Err(error) => (Err(error), Vec::new()),
        }
    }

    /// Runs a command and names the workflow it changed, if any.
    fn dispatch(&mut self, command: Command) -> Result<(Reply, Option<WorkflowId>), CommandError> {
        Ok(match command {
            Command::Ping => (Reply::Pong, None),
            Command::ListWorkflows => {
                let workflows = self
                    .workflows
                    .values()
                    .map(|flow| WorkflowSummary {
                        id: flow.id,
                        title: flow.title.clone(),
                        nodes: flow.nodes().count(),
                    })
                    .collect();
                (Reply::Workflows { workflows }, None)
            }
            Command::GetWorkflow { workflow } => {
                (Reply::Workflow { workflow: self.workflow(workflow)?.clone() }, None)
            }
            Command::CreateWorkflow { title } => {
                self.next_workflow += 1;
                let id = WorkflowId(self.next_workflow);
                self.workflows.insert(id, Workflow::new(id, title));
                (Reply::WorkflowCreated { workflow: id }, Some(id))
            }
            Command::AddNode { workflow, node } => {
                let node = self.workflow(workflow)?.add_node(node);
                (Reply::NodeAdded { node }, Some(workflow))
            }
            Command::RemoveNode { workflow, node } => {
                self.workflow(workflow)?.remove_node(node).map_err(refused)?;
                (Reply::Done, Some(workflow))
            }
            Command::AddEdge { workflow, from, to } => {
                self.workflow(workflow)?.add_edge(from, to).map_err(refused)?;
                (Reply::Done, Some(workflow))
            }
            Command::RemoveEdge { workflow, from, to } => {
                let removed = self.workflow(workflow)?.remove_edge(from, to);
                (Reply::Done, removed.then_some(workflow))
            }
            Command::WaitFor { workflow, node, on } => {
                if !self.contains(on) {
                    return Err(CommandError::Workflow { message: format!("{on} does not exist") });
                }
                self.workflow(workflow)?.wait_for(node, on).map_err(refused)?;
                (Reply::Done, Some(workflow))
            }
            Command::StopWaiting { workflow, node, on } => {
                let stopped = self.workflow(workflow)?.stop_waiting(node, on);
                (Reply::Done, stopped.then_some(workflow))
            }
            Command::SetWorkflowTarget { workflow, target } => {
                self.workflow(workflow)?.target = target;
                (Reply::Done, Some(workflow))
            }
            Command::EditNode { workflow, node, edit } => {
                self.workflow(workflow)?
                    .edit_node(node, edit, NodeOrigin::User)
                    .map_err(refused)?;
                (Reply::Done, Some(workflow))
            }
            Command::StartAgent { .. }
            | Command::ListAgents
            | Command::GetTranscript { .. }
            | Command::SendToAgent { .. }
            | Command::AnswerPermission { .. }
            | Command::InterruptAgent { .. }
            | Command::StopAgent { .. }
            | Command::PublishWorkspace { .. }
            | Command::MergePullRequest { .. }
            | Command::RemoveWorktree { .. }
            | Command::RunWorkflow { .. }
            | Command::SendToNode { .. }
            | Command::RateNode { .. }
            | Command::StartCommander { .. }
            | Command::ChooseModel { .. }
            | Command::PublishWorkflow { .. } => {
                unreachable!("the hub sends these to the runtime or the engine")
            }
        })
    }

    /// Whether `node` names a node that exists.
    pub(crate) fn contains(&self, node: NodeRef) -> bool {
        self.workflows.get(&node.workflow).is_some_and(|flow| flow.node(node.node).is_some())
    }

    /// The status of the node `node` names, if it exists.
    pub(crate) fn status(&self, node: NodeRef) -> Option<NodeStatus> {
        Some(self.workflows.get(&node.workflow)?.node(node.node)?.status)
    }

    pub(crate) fn workflow(&mut self, workflow: WorkflowId) -> Result<&mut Workflow, CommandError> {
        self.workflows.get_mut(&workflow).ok_or(CommandError::UnknownWorkflow { workflow })
    }

    /// The id the next workflow gets.
    pub(crate) fn next_id(&self) -> WorkflowId {
        WorkflowId(self.next_workflow + 1)
    }

    /// Adds a workflow built for [`State::next_id`], such as one from a commander's plan.
    pub(crate) fn insert(&mut self, workflow: Workflow) -> &Workflow {
        debug_assert_eq!(workflow.id, self.next_id());
        self.next_workflow += 1;
        self.workflows.entry(workflow.id).or_insert(workflow)
    }
}

fn refused(error: WorkflowError) -> CommandError {
    CommandError::Workflow { message: error.to_string() }
}

#[cfg(test)]
mod tests {
    use maestro_core::{NodeDraft, NodeId, NodeOrigin};

    use super::*;

    fn draft(title: &str) -> NodeDraft {
        NodeDraft {
            title: title.into(),
            instructions: String::new(),
            model: None,
            difficulty: None,
            origin: NodeOrigin::User,
        }
    }

    #[test]
    fn edits_emit_the_new_graph_and_failures_emit_nothing() {
        let mut state = State::default();
        let (created, events) = state.apply(Command::CreateWorkflow { title: "t".into() });
        assert_eq!(created, Ok(Reply::WorkflowCreated { workflow: WorkflowId(1) }));
        assert_eq!(events.len(), 1);

        let workflow = WorkflowId(1);
        state.apply(Command::AddNode { workflow, node: draft("a") }).0.unwrap();
        state.apply(Command::AddNode { workflow, node: draft("b") }).0.unwrap();
        let (_, events) =
            state.apply(Command::AddEdge { workflow, from: NodeId(1), to: NodeId(2) });
        let [Event::WorkflowChanged { workflow: graph }] = &events[..] else {
            panic!("{events:?}")
        };
        assert_eq!(graph.edges().count(), 1);

        let (result, events) =
            state.apply(Command::AddEdge { workflow, from: NodeId(2), to: NodeId(1) });
        assert!(matches!(result, Err(CommandError::Workflow { .. })));
        assert!(events.is_empty());

        // A node waits only on a node of an earlier workflow that exists.
        state.apply(Command::CreateWorkflow { title: "later".into() }).0.unwrap();
        let on = NodeRef { workflow, node: NodeId(2) };
        let later = WorkflowId(2);
        state.apply(Command::AddNode { workflow: later, node: draft("c") }).0.unwrap();
        let wait = |on| Command::WaitFor { workflow: later, node: NodeId(1), on };
        assert!(state.apply(wait(NodeRef { workflow, node: NodeId(9) })).0.is_err());
        assert!(state.apply(Command::WaitFor { workflow, node: NodeId(1), on }).0.is_err());
        let (result, events) = state.apply(wait(on));
        assert_eq!((result, events.len()), (Ok(Reply::Done), 1));
        assert!(state.contains(on) && state.status(on) == Some(NodeStatus::Pending));

        let (result, _) = state.apply(Command::GetWorkflow { workflow: WorkflowId(5) });
        assert_eq!(result, Err(CommandError::UnknownWorkflow { workflow: WorkflowId(5) }));
    }
}
