use std::collections::BTreeMap;

use maestro_core::{NodeOrigin, Workflow, WorkflowError, WorkflowId};
use maestro_protocol::{Command, CommandError, Event, Reply, WorkflowSummary};

/// Every workflow the daemon knows about.
#[derive(Debug, Default)]
pub struct State {
    workflows: BTreeMap<WorkflowId, Workflow>,
    next_workflow: u64,
}

impl State {
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

    pub(crate) fn workflow(&mut self, workflow: WorkflowId) -> Result<&mut Workflow, CommandError> {
        self.workflows.get_mut(&workflow).ok_or(CommandError::UnknownWorkflow { workflow })
    }

    /// Adds a workflow built for a fresh id, such as one from a commander's plan.
    pub(crate) fn insert<E>(
        &mut self,
        build: impl FnOnce(WorkflowId) -> Result<Workflow, E>,
    ) -> Result<&Workflow, E> {
        let id = WorkflowId(self.next_workflow + 1);
        let workflow = build(id)?;
        self.next_workflow += 1;
        Ok(self.workflows.entry(id).or_insert(workflow))
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

        let (result, _) = state.apply(Command::GetWorkflow { workflow: WorkflowId(5) });
        assert_eq!(result, Err(CommandError::UnknownWorkflow { workflow: WorkflowId(5) }));
    }
}
