//! Claude Code, Codex, Antigravity CLI and OpenCode as worker processes, through Atelier's
//! `atelier-agents`.
//!
//! The adapters that read each CLI's JSON live in `atelier-agents`, shared with Auris Studio;
//! this crate is Maestro's side of that boundary. It turns a [`WorkerConfig`] into the adapter's
//! configuration, runs the CLI on the worker's [`Host`] through [`HostCommand`] (so an agent
//! over `ssh`, in WSL or in a container starts the way every other Maestro process does), and
//! hands the daemon Maestro's own [`AgentEvent`]s. Nothing past this crate learns that the
//! adapters are not Maestro's.

use std::path::PathBuf;

use atelier_agents as agents;
use maestro_core::{
    AgentEvent, Effort, Host, PermissionKind, PermissionRequest, ProviderKind, ToolCall,
};
use maestro_host::{HostCommand, HostError};

pub use atelier_agents::Adapter;

/// How to start one worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerConfig {
    /// The CLI.
    pub provider: ProviderKind,
    /// The provider's model name, or `None` for the CLI's default.
    pub model: Option<String>,
    /// Reasoning effort, or `None` for the CLI's default.
    pub effort: Option<Effort>,
    /// The directory it works in, on its host.
    pub cwd: PathBuf,
    /// Standing instructions put in front of the conversation: the step, its limits.
    pub instructions: String,
    /// Overrides the executable's name or path.
    pub program: Option<String>,
    /// Refuse every change to files, where the CLI can be told so up front. Commanders and leads
    /// plan; the daemon also refuses any permission they ask for.
    pub read_only: bool,
    /// Whether it may start sub-agents of its own.
    pub subagents: bool,
}

/// The adapter for `config`'s CLI.
pub fn adapter(config: WorkerConfig) -> Box<dyn Adapter> {
    let mut atelier = agents::AgentConfig::new(provider(config.provider), config.cwd);
    atelier.model = config.model;
    atelier.effort = config.effort.map(effort);
    atelier.instructions = config.instructions;
    atelier.program = config.program;
    atelier.read_only = config.read_only;
    atelier.subagents = config.subagents;
    atelier.client = agents::ClientInfo {
        name: "maestro".into(),
        title: "Maestro".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    };
    agents::adapter(atelier)
}

/// Whether `tool` is how `provider` starts a sub-agent, for the CLIs that cannot be told up
/// front not to: the daemon fails a step whose worker does so when it may not.
pub fn starts_subagent(provider: ProviderKind, tool: &str) -> bool {
    agents::starts_subagent(self::provider(provider), tool)
}

/// A running worker. Dropping it stops the worker.
pub struct AgentSession(agents::AgentSession);

impl AgentSession {
    /// Starts the adapter's CLI on `host`, sends `prompt`, and reports every event to `on_event`
    /// from the pump thread. The last event is always [`AgentEvent::Exited`].
    pub fn spawn(
        host: &Host,
        adapter: Box<dyn Adapter>,
        prompt: &str,
        on_event: impl Fn(AgentEvent) + Send + 'static,
    ) -> Result<Self, HostError> {
        let forward = move |event| {
            if let Some(event) = self::event(event) {
                on_event(event);
            }
        };
        let session = match host {
            // On this machine the adapter writes the files it needs, owner-only, itself.
            Host::Local => agents::AgentSession::spawn(adapter, prompt, forward),
            _ => {
                let process = host_command(adapter.command()).process(host);
                agents::AgentSession::spawn_in(process, adapter, prompt, forward)
            }
        };
        session
            .map(Self)
            .map_err(|error| HostError::Spawn { program: error.program, source: error.source })
    }

    /// Sends the next user message. Call it between turns.
    pub fn send(&self, text: impl Into<String>) {
        self.0.send(text);
    }

    /// Answers a permission request.
    pub fn answer(&self, request: impl Into<String>, allow: bool) {
        self.0.answer(request, allow);
    }

    /// Asks the CLI to abandon the current turn; the agent stays up for the next message.
    pub fn interrupt(&self) {
        self.0.interrupt();
    }

    /// Kills the process. [`AgentEvent::Exited`] follows.
    pub fn stop(&self) {
        self.0.stop();
    }
}

/// The adapter's command as Maestro runs a process on a host.
fn host_command(command: agents::CommandSpec) -> HostCommand {
    HostCommand {
        program: command.program,
        args: command.args,
        cwd: command.cwd,
        env: command.env,
        env_remove: command.env_remove,
    }
}

fn provider(provider: ProviderKind) -> agents::ProviderKind {
    match provider {
        ProviderKind::ClaudeCode => agents::ProviderKind::ClaudeCode,
        ProviderKind::Codex => agents::ProviderKind::Codex,
        ProviderKind::Antigravity => agents::ProviderKind::Antigravity,
        ProviderKind::OpenCode => agents::ProviderKind::OpenCode,
    }
}

fn effort(effort: Effort) -> agents::Effort {
    match effort {
        Effort::Low => agents::Effort::Low,
        Effort::Medium => agents::Effort::Medium,
        Effort::High => agents::Effort::High,
        Effort::Max => agents::Effort::Max,
    }
}

/// An adapter's event as Maestro's. The ones Maestro has no use for yet (the CLI's session id,
/// its models, the plan's limits) are dropped; `Prompted` and `Queued` are Maestro's own to
/// report, and an adapter never produces them.
fn event(event: agents::AgentEvent) -> Option<AgentEvent> {
    use agents::AgentEvent as From;
    Some(match event {
        From::Started => AgentEvent::Started,
        From::Prompted { text } => AgentEvent::Prompted { text },
        From::Queued { text } => AgentEvent::Queued { text },
        From::Text { text } => AgentEvent::Text { text },
        From::Reasoning { text } => AgentEvent::Reasoning { text },
        From::ToolStarted { call } => AgentEvent::ToolStarted {
            call: ToolCall { id: call.id, name: call.name, input: call.input },
        },
        From::ToolFinished { id, ok, output } => AgentEvent::ToolFinished { id, ok, output },
        From::PermissionRequested { request } => AgentEvent::PermissionRequested {
            request: PermissionRequest {
                id: request.id,
                kind: match request.kind {
                    agents::PermissionKind::Command => PermissionKind::Command,
                    agents::PermissionKind::FileChange => PermissionKind::FileChange,
                    agents::PermissionKind::Other => PermissionKind::Other,
                },
                summary: request.summary,
                detail: request.detail,
            },
        },
        From::TurnCompleted { answer } => AgentEvent::TurnCompleted { answer },
        From::TurnFailed { message } => AgentEvent::TurnFailed { message },
        From::Exited { reason } => AgentEvent::Exited { reason },
        From::SessionStarted { .. }
        | From::ResumeFailed { .. }
        | From::Models { .. }
        | From::RateLimits { .. } => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_provider_names_its_own_sub_agent_tools() {
        assert!(starts_subagent(ProviderKind::ClaudeCode, "Task"));
        assert!(starts_subagent(ProviderKind::ClaudeCode, "Agent"));
        assert!(!starts_subagent(ProviderKind::ClaudeCode, "Bash"));
        assert!(starts_subagent(ProviderKind::Antigravity, "invoke_subagent"));
        assert!(!starts_subagent(ProviderKind::Antigravity, "run_command"));
        assert!(starts_subagent(ProviderKind::OpenCode, "task"));
        assert!(starts_subagent(ProviderKind::Codex, "collabAgentToolCall"));
        assert!(!starts_subagent(ProviderKind::Codex, "commandExecution"));
    }

    #[test]
    fn a_remote_worker_runs_the_adapters_command_through_the_host() {
        let config = WorkerConfig {
            provider: ProviderKind::ClaudeCode,
            model: None,
            effort: Some(Effort::High),
            cwd: "/work/n1".into(),
            instructions: String::new(),
            program: None,
            read_only: true,
            subagents: false,
        };
        let command = host_command(adapter(config).command());
        assert_eq!(command.program, "claude");
        assert_eq!(command.cwd.as_deref(), Some(std::path::Path::new("/work/n1")));
        let ssh = command.process(&Host::Ssh { destination: "box".into() });
        assert_eq!(ssh.get_program(), "ssh");
    }

    #[test]
    fn events_maestro_does_not_use_are_dropped() {
        assert_eq!(event(agents::AgentEvent::Started), Some(AgentEvent::Started));
        assert_eq!(event(agents::AgentEvent::SessionStarted { id: "s".into() }), None);
    }
}
