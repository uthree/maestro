//! The four coding-agent CLIs, behind one interface.
//!
//! Each provider module holds an [`Adapter`]: a state machine with no I/O that knows which
//! command starts the CLI, what to write to it, and how to read what it writes back into
//! [`AgentEvent`]s. Keeping I/O out means an adapter is tested by feeding it recorded JSON, and the
//! same adapter works whether the CLI's stdio is a local pipe or an `ssh` session.
//! [`session::AgentSession`] supplies the I/O: it starts the process on a [`Host`], pumps lines
//! through the adapter and hands events to a callback.
//!
//! The wire formats follow what Auris Studio's backends speak to the same CLIs:
//! Claude Code's `stream-json`, Codex's `app-server` JSON-RPC, OpenCode's Agent Client Protocol
//! and Antigravity's `stream-json`.

mod antigravity;
mod claude;
mod codex;
mod opencode;
mod rpc;
pub mod session;

use std::path::PathBuf;

use maestro_core::{AgentEvent, Effort, ProviderKind};
use maestro_host::HostCommand;
use serde_json::Value;

pub use session::AgentSession;

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
    /// Standing instructions from the commander: the node's role and its limits.
    pub instructions: String,
    /// Overrides the executable's name or path.
    pub program: Option<String>,
    /// Refuse every change to files, where the CLI can be told so up front. The commander runs
    /// this way; the daemon also refuses any permission it asks for.
    pub read_only: bool,
}

/// What an adapter produced from one message.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    /// Events for the daemon.
    pub events: Vec<AgentEvent>,
    /// Messages to write back to the CLI.
    pub replies: Vec<Value>,
}

impl Output {
    fn event(event: AgentEvent) -> Self {
        Self { events: vec![event], replies: Vec::new() }
    }

    fn reply(reply: Value) -> Self {
        Self { events: Vec::new(), replies: vec![reply] }
    }
}

/// One provider's protocol, without I/O.
///
/// The session calls [`Adapter::start`] once after spawning [`Adapter::command`], then
/// [`Adapter::receive`] for every line the CLI writes. [`Adapter::send`] is called only between
/// turns, after a [`AgentEvent::TurnCompleted`] or [`AgentEvent::TurnFailed`].
pub trait Adapter: Send {
    /// The process to start.
    fn command(&self) -> HostCommand;
    /// The first messages, carrying the first prompt. An adapter whose CLI needs a handshake
    /// sends the handshake here and the prompt once the CLI is ready.
    fn start(&mut self, prompt: &str) -> Vec<Value>;
    /// The next user message.
    fn send(&mut self, text: &str) -> Vec<Value>;
    /// One message from the CLI.
    fn receive(&mut self, message: Value) -> Output;
    /// Answers a [`AgentEvent::PermissionRequested`].
    fn answer(&mut self, request: &str, allow: bool) -> Vec<Value>;
    /// Messages that ask the CLI to stop the current turn before it is killed.
    fn interrupt(&mut self) -> Vec<Value> {
        Vec::new()
    }
}

/// The adapter for a worker's provider.
pub fn adapter(config: WorkerConfig) -> Box<dyn Adapter> {
    match config.provider {
        ProviderKind::ClaudeCode => Box::new(claude::Claude::new(config)),
        ProviderKind::Codex => Box::new(codex::Codex::new(config)),
        ProviderKind::Antigravity => Box::new(antigravity::Antigravity::new(config)),
        ProviderKind::OpenCode => Box::new(opencode::OpenCode::new(config)),
    }
}

/// What separates two messages of one turn, in the answer and in the streamed text.
const PARAGRAPH: &str = "\n\n";

/// Puts the commander's instructions in front of the first prompt, for CLIs that have no
/// separate channel for them that leaves their own system prompt intact.
fn with_instructions(instructions: &str, prompt: &str) -> String {
    if instructions.is_empty() {
        prompt.to_owned()
    } else {
        format!("{instructions}\n\n---\n\n{prompt}")
    }
}

/// The text of a tool result that may be a string or a list of content parts.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
fn config(provider: ProviderKind) -> WorkerConfig {
    WorkerConfig {
        provider,
        model: Some("model-x".into()),
        effort: Some(Effort::High),
        cwd: "/work/n1".into(),
        instructions: "Only touch src/.".into(),
        program: None,
        read_only: false,
    }
}
