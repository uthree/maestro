//! A frontend's copy of the daemon's state, kept current by applying [`Event`]s.
//!
//! The daemon owns the workflows and the agents; a frontend holds a [`Mirror`] of them, loaded
//! once with [`Mirror::load`] and then moved along by [`Mirror::apply`] for every event. It also
//! folds each agent's [`AgentEvent`]s into a [`Transcript`] of entries a view can draw directly,
//! so the GUI and the TUI read a conversation the same way.

use std::collections::BTreeMap;

use maestro_core::{
    AgentEvent, AgentId, AgentRole, AgentSpec, NodeId, PermissionRequest, ToolCall, Workflow,
    WorkflowId,
};
use maestro_protocol::{AgentStatus, AgentSummary, Command, Event, Reply};

use crate::{Client, ClientError};

/// Everything a frontend shows, as of the last event applied.
#[derive(Clone, Debug, Default)]
pub struct Mirror {
    /// Every workflow, by id.
    pub workflows: BTreeMap<WorkflowId, Workflow>,
    /// Every agent the daemon has started, by id.
    pub agents: BTreeMap<AgentId, AgentView>,
    /// The workflows each commander has planned, oldest first.
    pub plans: BTreeMap<AgentId, Vec<WorkflowId>>,
    /// Things that went wrong or finished outside any one transcript, oldest first.
    pub notices: Vec<Notice>,
}

/// One agent as a frontend sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentView {
    /// How it was started.
    pub spec: AgentSpec,
    /// Where it is.
    pub status: AgentStatus,
    /// What it has said and done.
    pub transcript: Transcript,
}

/// Something worth telling the user about that belongs to no single agent's conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A commander's plan could not become a workflow.
    PlanRejected {
        /// The commander.
        agent: AgentId,
        /// Why.
        message: String,
    },
    /// A node failed.
    NodeFailed {
        /// Its workflow.
        workflow: WorkflowId,
        /// The node.
        node: NodeId,
        /// Why.
        message: String,
    },
    /// A workflow's pull request was opened.
    Published {
        /// The workflow.
        workflow: WorkflowId,
        /// The pull request's number.
        number: u64,
        /// Its page.
        url: String,
    },
}

impl Mirror {
    /// Fetches everything the daemon has. Blocks for several round trips, so call it off the UI
    /// thread. Events that arrive while it runs may repeat what it fetched; take the client's
    /// events first and apply only those received after this returns.
    pub fn load(client: &Client) -> Result<Self, ClientError> {
        let mut mirror = Self::default();
        let Reply::Workflows { workflows } = client.request(Command::ListWorkflows)? else {
            return Err(ClientError::Disconnected);
        };
        for summary in workflows {
            if let Reply::Workflow { workflow } =
                client.request(Command::GetWorkflow { workflow: summary.id })?
            {
                if let Some(agent) = workflow.planned_by {
                    mirror.plans.entry(agent).or_default().push(workflow.id);
                }
                mirror.workflows.insert(workflow.id, workflow);
            }
        }
        let Reply::Agents { agents } = client.request(Command::ListAgents)? else {
            return Err(ClientError::Disconnected);
        };
        for summary in agents {
            let Reply::Transcript { events } =
                client.request(Command::GetTranscript { agent: summary.id })?
            else {
                continue;
            };
            let mut transcript = Transcript::default();
            for event in &events {
                transcript.apply(event);
            }
            let view = AgentView { spec: summary.spec, status: summary.status, transcript };
            mirror.agents.insert(summary.id, view);
        }
        Ok(mirror)
    }

    /// Moves the copy along by one event.
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::WorkflowChanged { workflow } => {
                self.workflows.insert(workflow.id, workflow.clone());
            }
            Event::PlanRejected { agent, message } => {
                self.notices.push(Notice::PlanRejected { agent: *agent, message: message.clone() });
            }
            Event::NodeFailed { workflow, node, message } => {
                self.notices.push(Notice::NodeFailed {
                    workflow: *workflow,
                    node: *node,
                    message: message.clone(),
                })
            }
            Event::WorkflowPublished { workflow, number, url } => {
                self.notices.push(Notice::Published {
                    workflow: *workflow,
                    number: *number,
                    url: url.clone(),
                });
            }
            Event::Planned { agent, workflow } => {
                self.plans.entry(*agent).or_default().push(*workflow);
            }
            Event::AgentStarted { summary } => {
                let AgentSummary { id, spec, status } = summary.clone();
                self.agents
                    .insert(id, AgentView { spec, status, transcript: Transcript::default() });
            }
            Event::Agent { agent, event } => {
                if let Some(view) = self.agents.get_mut(agent) {
                    view.apply(event);
                }
            }
        }
    }

    /// The commanders, oldest first.
    pub fn commanders(&self) -> impl DoubleEndedIterator<Item = (AgentId, &AgentView)> {
        self.agents
            .iter()
            .filter(|(_, view)| view.spec.role == AgentRole::Commander)
            .map(|(&id, view)| (id, view))
    }
}

impl AgentView {
    fn apply(&mut self, event: &AgentEvent) {
        // The same rules the daemon's runtime follows, so the copy agrees with `ListAgents`.
        match event {
            AgentEvent::Prompted { .. } if self.status != AgentStatus::Exited => {
                self.status = AgentStatus::Working;
            }
            // The daemon refuses a commander's requests itself, and the turn carries on.
            AgentEvent::PermissionRequested { .. } if self.spec.role != AgentRole::Commander => {
                self.status = AgentStatus::AwaitingPermission;
            }
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. } => {
                self.status = AgentStatus::Idle;
            }
            AgentEvent::Exited { .. } => self.status = AgentStatus::Exited,
            _ => {}
        }
        self.transcript.apply(event);
        if self.spec.role == AgentRole::Commander {
            self.transcript.close_permissions(false);
        }
    }
}

/// A conversation, as entries a view draws in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    /// The entries, oldest first.
    pub entries: Vec<Entry>,
    /// Whether a turn is under way.
    pub busy: bool,
    /// Messages sent during a turn, oldest first, which the agent gets once the turn ends.
    pub queued: Vec<String>,
    /// Where the current turn's entries start.
    turn: usize,
}

/// One entry of a [`Transcript`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A message sent to the agent.
    User(String),
    /// The agent's reply, `done` once its turn has ended.
    Assistant {
        /// The text so far.
        text: String,
        /// Whether more may follow.
        done: bool,
    },
    /// The model's visible reasoning.
    Reasoning(String),
    /// Tool calls made one after another.
    Tools(Vec<Tool>),
    /// A request for permission; `open` until it is answered or the turn moves on.
    Permission {
        /// The request.
        request: PermissionRequest,
        /// Still waiting for an answer.
        open: bool,
        /// How it was answered, once it was.
        allowed: Option<bool>,
    },
    /// A turn ended in an error.
    Failed(String),
    /// The process ended.
    Exited(Option<String>),
}

/// One tool call in a transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    /// The call.
    pub call: ToolCall,
    /// How it went.
    pub state: ToolState,
    /// What it printed, once it finished.
    pub output: String,
}

/// Where a tool call is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolState {
    /// Still running.
    Running,
    /// Finished successfully.
    Succeeded,
    /// Finished with an error.
    Failed,
}

impl Transcript {
    /// Adds one event.
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Started => {}
            AgentEvent::Queued { text } => self.queued.push(text.clone()),
            AgentEvent::Prompted { text } => {
                if let Some(index) = self.queued.iter().position(|queued| queued == text) {
                    self.queued.remove(index);
                }
                self.close_permissions(true);
                self.busy = true;
                self.turn = self.entries.len();
                self.entries.push(Entry::User(text.clone()));
            }
            AgentEvent::Text { text } => {
                self.close_permissions(true);
                self.busy = true;
                match self.entries.last_mut() {
                    Some(Entry::Assistant { text: so_far, done: false }) => so_far.push_str(text),
                    _ => self.entries.push(Entry::Assistant { text: text.clone(), done: false }),
                }
            }
            AgentEvent::Reasoning { text } => match self.entries.last_mut() {
                Some(Entry::Reasoning(so_far)) => so_far.push_str(text),
                _ => self.entries.push(Entry::Reasoning(text.clone())),
            },
            AgentEvent::ToolStarted { call } => {
                self.close_permissions(true);
                let tool =
                    Tool { call: call.clone(), state: ToolState::Running, output: String::new() };
                match self.entries.last_mut() {
                    Some(Entry::Tools(tools)) => tools.push(tool),
                    _ => self.entries.push(Entry::Tools(vec![tool])),
                }
            }
            AgentEvent::ToolFinished { id, ok, output } => {
                let tool = self
                    .entries
                    .iter_mut()
                    .rev()
                    .filter_map(|entry| match entry {
                        Entry::Tools(tools) => Some(tools),
                        _ => None,
                    })
                    .flat_map(|tools| tools.iter_mut().rev())
                    .find(|tool| tool.call.id == *id);
                if let Some(tool) = tool {
                    tool.state = if *ok { ToolState::Succeeded } else { ToolState::Failed };
                    tool.output = output.clone();
                }
            }
            AgentEvent::PermissionRequested { request } => {
                self.entries.push(Entry::Permission {
                    request: request.clone(),
                    open: true,
                    allowed: None,
                });
            }
            AgentEvent::TurnCompleted { answer } => {
                self.close_permissions(true);
                self.busy = false;
                let replied = self.entries[self.turn.min(self.entries.len())..]
                    .iter()
                    .any(|entry| matches!(entry, Entry::Assistant { .. }));
                if !replied && !answer.is_empty() {
                    self.entries.push(Entry::Assistant { text: answer.clone(), done: true });
                }
                self.finish_turn();
            }
            AgentEvent::TurnFailed { message } => {
                self.close_permissions(false);
                self.busy = false;
                self.finish_turn();
                self.entries.push(Entry::Failed(message.clone()));
            }
            AgentEvent::Exited { reason } => {
                self.close_permissions(false);
                self.busy = false;
                self.queued.clear();
                self.finish_turn();
                self.entries.push(Entry::Exited(reason.clone()));
            }
        }
    }

    /// The request still waiting for an answer, if any.
    pub fn open_permission(&self) -> Option<&PermissionRequest> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Permission { request, open: true, .. } => Some(request),
            _ => None,
        })
    }

    /// The last reply the agent finished, if any.
    pub fn last_answer(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
    }

    /// Records the answer the user gave to a request, ahead of the agent's reaction.
    pub fn answered(&mut self, request: &str, allow: bool) {
        for entry in &mut self.entries {
            if let Entry::Permission { request: asked, open, allowed } = entry
                && asked.id == request
            {
                *open = false;
                *allowed = Some(allow);
            }
        }
    }

    /// Closes any open request: the agent went on, so it was answered, `allowed` or not.
    fn close_permissions(&mut self, allowed: bool) {
        for entry in &mut self.entries {
            if let Entry::Permission { open: open @ true, allowed: answer, .. } = entry {
                *open = false;
                answer.get_or_insert(allowed);
            }
        }
    }

    fn finish_turn(&mut self) {
        let start = self.turn.min(self.entries.len());
        for entry in &mut self.entries[start..] {
            if let Entry::Assistant { done, .. } = entry {
                *done = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use maestro_core::{
        Effort, Host, ModelChoice, ModelGrade, PermissionKind, ProviderKind, Workspace,
    };

    use super::*;

    fn view(role: AgentRole) -> AgentView {
        AgentView {
            spec: AgentSpec {
                role,
                model: ModelChoice {
                    provider: ProviderKind::ClaudeCode,
                    model: String::new(),
                    grade: ModelGrade::Standard,
                    effort: Effort::Medium,
                },
                workspace: Workspace {
                    host: Host::Local,
                    repository: "/repo".into(),
                    worktree: None,
                },
            },
            status: AgentStatus::Working,
            transcript: Transcript::default(),
        }
    }

    fn request(id: &str) -> PermissionRequest {
        PermissionRequest {
            id: id.into(),
            kind: PermissionKind::Command,
            summary: "Run ls".into(),
            detail: String::new(),
        }
    }

    #[test]
    fn a_turn_becomes_a_message_tools_and_a_reply() {
        let mut transcript = Transcript::default();
        let call = ToolCall { id: "t1".into(), name: "Bash".into(), input: "ls".into() };
        for event in [
            AgentEvent::Started,
            AgentEvent::Prompted { text: "List the files".into() },
            AgentEvent::Reasoning { text: "Use ".into() },
            AgentEvent::Reasoning { text: "ls.".into() },
            AgentEvent::ToolStarted { call: call.clone() },
            AgentEvent::ToolFinished { id: "t1".into(), ok: true, output: "a\nb".into() },
            AgentEvent::Text { text: "Two ".into() },
            AgentEvent::Text { text: "files.".into() },
        ] {
            transcript.apply(&event);
        }
        assert!(transcript.busy);
        assert_eq!(
            transcript.entries.last(),
            Some(&Entry::Assistant { text: "Two files.".into(), done: false })
        );
        transcript.apply(&AgentEvent::TurnCompleted { answer: "Two files.".into() });
        assert!(!transcript.busy);
        assert_eq!(
            transcript.entries,
            [
                Entry::User("List the files".into()),
                Entry::Reasoning("Use ls.".into()),
                Entry::Tools(vec![Tool {
                    call,
                    state: ToolState::Succeeded,
                    output: "a\nb".into()
                }]),
                Entry::Assistant { text: "Two files.".into(), done: true },
            ]
        );
    }

    #[test]
    fn a_reply_sent_only_at_the_end_still_shows() {
        let mut transcript = Transcript::default();
        transcript.apply(&AgentEvent::Prompted { text: "Hi".into() });
        transcript.apply(&AgentEvent::TurnCompleted { answer: "Hello".into() });
        assert_eq!(transcript.last_answer(), Some("Hello"));
        // A second turn with a streamed reply does not repeat it.
        transcript.apply(&AgentEvent::Prompted { text: "Again".into() });
        transcript.apply(&AgentEvent::Text { text: "Hello again".into() });
        transcript.apply(&AgentEvent::TurnCompleted { answer: "Hello again".into() });
        assert_eq!(transcript.entries.len(), 4);
    }

    #[test]
    fn a_message_sent_during_a_turn_waits_for_the_turn_to_end() {
        let mut transcript = Transcript::default();
        transcript.apply(&AgentEvent::Prompted { text: "Plan it".into() });
        transcript.apply(&AgentEvent::Text { text: "Working on".into() });
        transcript.apply(&AgentEvent::Queued { text: "Also this".into() });
        transcript.apply(&AgentEvent::Text { text: " it".into() });
        assert_eq!(transcript.queued, ["Also this"]);
        assert_eq!(transcript.last_answer(), Some("Working on it"));
        transcript.apply(&AgentEvent::TurnCompleted { answer: "Working on it".into() });
        transcript.apply(&AgentEvent::Prompted { text: "Also this".into() });
        assert!(transcript.queued.is_empty());
        assert_eq!(transcript.entries.last(), Some(&Entry::User("Also this".into())));
    }

    #[test]
    fn requests_close_when_answered_or_when_the_agent_moves_on() {
        let mut worker = view(AgentRole::Worker);
        worker.apply(&AgentEvent::PermissionRequested { request: request("p1") });
        assert_eq!(worker.status, AgentStatus::AwaitingPermission);
        assert_eq!(worker.transcript.open_permission().map(|r| r.id.as_str()), Some("p1"));
        worker.transcript.answered("p1", false);
        assert_eq!(worker.transcript.open_permission(), None);

        worker.apply(&AgentEvent::PermissionRequested { request: request("p2") });
        worker.apply(&AgentEvent::Text { text: "Done".into() });
        assert!(matches!(worker.transcript.entries.last(), Some(Entry::Assistant { .. })));
        assert_eq!(worker.transcript.open_permission(), None);

        // The daemon refuses a commander's requests at once; it never waits on the user.
        let mut commander = view(AgentRole::Commander);
        commander.apply(&AgentEvent::PermissionRequested { request: request("p3") });
        assert_eq!(commander.status, AgentStatus::Working);
        assert!(matches!(
            commander.transcript.entries[0],
            Entry::Permission { open: false, allowed: Some(false), .. }
        ));
    }
}
