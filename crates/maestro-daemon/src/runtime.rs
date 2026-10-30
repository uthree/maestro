//! The commands with side effects: agent processes, worktrees, commits and pull requests.
//!
//! These run on their own thread per request, because over SSH a `git push` or a CLI start takes
//! seconds, and the connection that asked must stay free to send other requests meanwhile. No
//! lock is held while a process runs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::snapshot::SavedAgent;
use maestro_agents::{AgentSession, WorkerConfig};
use maestro_core::{AgentEvent, AgentId, AgentRole, AgentSpec, Workspace};
use maestro_host::github::{self, PullRequestDraft};
use maestro_host::{HostError, git};
use maestro_protocol::{
    AgentStatus, AgentSummary, Command, CommandError, Event, MergeMethod, Reply,
};

pub(crate) type Broadcast = Arc<dyn Fn(Event) + Send + Sync>;
pub(crate) type Listener = Box<dyn Fn(AgentId, AgentEvent) + Send + Sync>;

struct Agent {
    spec: AgentSpec,
    status: AgentStatus,
    session: Option<AgentSession>,
    /// Every event so far, for frontends that connect later.
    transcript: Vec<AgentEvent>,
}

pub(crate) struct Runtime {
    agents: Mutex<BTreeMap<AgentId, Agent>>,
    next_agent: AtomicU64,
    broadcast: Broadcast,
    /// Also told every event: the workflow engine, which moves nodes along as workers finish.
    listener: OnceLock<Listener>,
    this: Weak<Runtime>,
}

impl Runtime {
    pub(crate) fn new(broadcast: Broadcast) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            agents: Mutex::default(),
            next_agent: AtomicU64::new(1),
            broadcast,
            listener: OnceLock::new(),
            this: this.clone(),
        })
    }

    /// Takes back the agents a snapshot remembers. They have exited; new agents get later ids.
    pub(crate) fn restore(&self, saved: Vec<SavedAgent>) {
        let mut agents = self.agents.lock().expect("agents lock");
        for SavedAgent { id, spec, transcript } in saved {
            self.next_agent.fetch_max(id.0 + 1, Ordering::Relaxed);
            let agent = Agent { spec, status: AgentStatus::Exited, session: None, transcript };
            agents.insert(id, agent);
        }
    }

    /// Every agent, for a snapshot.
    pub(crate) fn saved(&self) -> Vec<SavedAgent> {
        let agents = self.agents.lock().expect("agents lock");
        agents
            .iter()
            .map(|(&id, agent)| SavedAgent {
                id,
                spec: agent.spec.clone(),
                transcript: agent.transcript.clone(),
            })
            .collect()
    }

    /// Whether any agent is in the middle of a turn or waiting for the user. An agent between
    /// turns may be stopped and started again later without losing anything.
    pub(crate) fn busy(&self) -> bool {
        let agents = self.agents.lock().expect("agents lock");
        agents.values().any(|agent| {
            matches!(agent.status, AgentStatus::Working | AgentStatus::AwaitingPermission)
        })
    }

    /// Stops every agent process.
    pub(crate) fn stop_all(&self) {
        for agent in self.agents.lock().expect("agents lock").values() {
            if let Some(session) = &agent.session {
                session.stop();
            }
        }
    }

    /// Sends every later agent event to `listener` too.
    pub(crate) fn listen(&self, listener: Listener) {
        self.listener.set(listener).ok();
    }

    /// Whether `command` belongs here rather than to the workflow state.
    pub(crate) fn handles(command: &Command) -> bool {
        matches!(
            command,
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
        )
    }

    pub(crate) fn execute(&self, command: Command) -> Result<Reply, CommandError> {
        match command {
            Command::StartAgent { spec, instructions, prompt } => {
                self.start(spec, instructions, &prompt).map(|agent| Reply::AgentStarted { agent })
            }
            Command::ListAgents => {
                let agents = self.agents.lock().expect("agents lock");
                let agents = agents
                    .iter()
                    .map(|(&id, agent)| AgentSummary {
                        id,
                        spec: agent.spec.clone(),
                        status: agent.status,
                    })
                    .collect();
                Ok(Reply::Agents { agents })
            }
            Command::GetTranscript { agent } => {
                let agents = self.agents.lock().expect("agents lock");
                let entry = agents.get(&agent).ok_or(CommandError::UnknownAgent { agent })?;
                Ok(Reply::Transcript { events: entry.transcript.clone() })
            }
            Command::SendToAgent { agent, text } => {
                if !self.agents.lock().expect("agents lock").contains_key(&agent) {
                    return Err(CommandError::UnknownAgent { agent });
                }
                // Reported before it is sent, so that it precedes the reply in every transcript.
                self.observe(agent, AgentEvent::Prompted { text: text.clone() });
                self.with_session(agent, Some(AgentStatus::Working), |session| session.send(text))
            }
            Command::AnswerPermission { agent, request, allow } => {
                self.with_session(agent, Some(AgentStatus::Working), |session| {
                    session.answer(request, allow)
                })
            }
            Command::InterruptAgent { agent } => {
                self.with_session(agent, None, AgentSession::interrupt)
            }
            Command::StopAgent { agent } => self.with_session(agent, None, AgentSession::stop),
            Command::PublishWorkspace { workspace, message, title, body, base, draft } => {
                let worktree = workspace.worktree.as_ref().ok_or(CommandError::NoWorktree)?;
                let host = &workspace.host;
                let commit = git::commit_all(host, &worktree.path, &message).map_err(failed)?;
                git::push(host, &worktree.path, &worktree.branch).map_err(failed)?;
                let draft =
                    PullRequestDraft { title, body, base, head: worktree.branch.clone(), draft };
                let opened = github::create(host, &worktree.path, &draft).map_err(failed)?;
                Ok(Reply::PullRequestOpened { number: opened.number, url: opened.url, commit })
            }
            Command::MergePullRequest { workspace, number, method } => {
                let method = match method {
                    MergeMethod::Squash => github::MergeMethod::Squash,
                    MergeMethod::Merge => github::MergeMethod::Merge,
                    MergeMethod::Rebase => github::MergeMethod::Rebase,
                };
                let directory = workspace.working_directory();
                github::merge(&workspace.host, directory, number, method).map_err(failed)?;
                Ok(Reply::Done)
            }
            Command::RemoveWorktree { workspace, force } => {
                let worktree = workspace.worktree.as_ref().ok_or(CommandError::NoWorktree)?;
                git::remove_worktree(&workspace.host, &workspace.repository, &worktree.path, force)
                    .map_err(failed)?;
                Ok(Reply::Done)
            }
            other => unreachable!("not a runtime command: {other:?}"),
        }
    }

    /// Starts an agent, creating its worktree first. A commander is started read-only, and any
    /// permission it asks for is refused.
    pub(crate) fn start(
        &self,
        spec: AgentSpec,
        instructions: String,
        prompt: &str,
    ) -> Result<AgentId, CommandError> {
        prepare_worktree(&spec.workspace).map_err(failed)?;
        let agent = AgentId(self.next_agent.fetch_add(1, Ordering::Relaxed));
        self.agents.lock().expect("agents lock").insert(
            agent,
            Agent {
                spec: spec.clone(),
                status: AgentStatus::Working,
                session: None,
                transcript: Vec::new(),
            },
        );
        let summary = AgentSummary { id: agent, spec: spec.clone(), status: AgentStatus::Working };
        (self.broadcast)(Event::AgentStarted { summary });
        if !prompt.is_empty() {
            self.observe(agent, AgentEvent::Prompted { text: prompt.into() });
        }
        let config = WorkerConfig {
            provider: spec.model.provider,
            model: Some(spec.model.model.clone()).filter(|model| !model.is_empty()),
            effort: Some(spec.model.effort),
            cwd: spec.workspace.working_directory().clone(),
            instructions,
            program: None,
            read_only: spec.role == AgentRole::Commander,
        };
        let this = self.this.clone();
        let spawned = AgentSession::spawn(
            &spec.workspace.host,
            maestro_agents::adapter(config),
            prompt,
            move |event| {
                if let Some(runtime) = this.upgrade() {
                    runtime.observe(agent, event);
                }
            },
        );
        let mut agents = self.agents.lock().expect("agents lock");
        match spawned {
            Ok(session) => {
                if let Some(entry) = agents.get_mut(&agent) {
                    entry.session = Some(session);
                }
                Ok(agent)
            }
            Err(error) => {
                agents.remove(&agent);
                Err(failed(error))
            }
        }
    }

    fn observe(&self, agent: AgentId, event: AgentEvent) {
        let status = match &event {
            AgentEvent::PermissionRequested { .. } => Some(AgentStatus::AwaitingPermission),
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. } => {
                Some(AgentStatus::Idle)
            }
            AgentEvent::Exited { .. } => Some(AgentStatus::Exited),
            _ => None,
        };
        if let Some(entry) = self.agents.lock().expect("agents lock").get_mut(&agent) {
            if let Some(status) = status {
                entry.status = status;
            }
            remember(&mut entry.transcript, &event);
            // The commander plans; it never changes anything, whatever it asks.
            if let AgentEvent::PermissionRequested { request } = &event
                && entry.spec.role == AgentRole::Commander
                && let Some(session) = &entry.session
            {
                session.answer(request.id.clone(), false);
                entry.status = AgentStatus::Working;
            }
        }
        if let Some(listener) = self.listener.get() {
            listener(agent, event.clone());
        }
        (self.broadcast)(Event::Agent { agent, event });
    }

    /// Acts on a running agent's session, first recording the status the action leads to.
    fn with_session(
        &self,
        agent: AgentId,
        status: Option<AgentStatus>,
        act: impl FnOnce(&AgentSession),
    ) -> Result<Reply, CommandError> {
        let mut agents = self.agents.lock().expect("agents lock");
        let entry = agents.get_mut(&agent).ok_or(CommandError::UnknownAgent { agent })?;
        if let Some(status) = status.filter(|_| entry.status != AgentStatus::Exited) {
            entry.status = status;
        }
        if let Some(session) = &entry.session {
            act(session);
        }
        Ok(Reply::Done)
    }
}

/// Adds an event to a transcript, joining a piece of text to the piece before it.
fn remember(transcript: &mut Vec<AgentEvent>, event: &AgentEvent) {
    match (transcript.last_mut(), event) {
        (Some(AgentEvent::Text { text }), AgentEvent::Text { text: more })
        | (Some(AgentEvent::Reasoning { text }), AgentEvent::Reasoning { text: more }) => {
            text.push_str(more);
        }
        _ => transcript.push(event.clone()),
    }
}

/// Creates the workspace's worktree unless it is already registered.
pub(crate) fn prepare_worktree(workspace: &Workspace) -> Result<(), HostError> {
    let Some(worktree) = &workspace.worktree else { return Ok(()) };
    let existing = git::list_worktrees(&workspace.host, &workspace.repository)?;
    if existing.iter().any(|entry| entry.path == worktree.path) {
        return Ok(());
    }
    git::add_worktree(&workspace.host, &workspace.repository, worktree)
}

pub(crate) fn failed(error: HostError) -> CommandError {
    CommandError::Host { message: error.to_string() }
}
