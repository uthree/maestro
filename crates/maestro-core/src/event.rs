//! What a running agent reports, in one vocabulary for every provider.
//!
//! Claude Code, Codex, Antigravity and OpenCode each stream their own JSON. The adapters in
//! `maestro-agents` translate it into [`AgentEvent`]s at the edge, so the daemon, the protocol and
//! every frontend see one shape and never learn which CLI produced it.

use serde::{Deserialize, Serialize};

/// A tool the agent started, as shown in a transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call, used to pair it with its result.
    pub id: String,
    /// The tool's name as the provider reports it (`Bash`, `commandExecution`, `execute`, ...).
    pub name: String,
    /// The tool's input, as JSON text, for display.
    pub input: String,
}

/// The broad kind of an action awaiting approval, for policy and for the approval card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    /// Running a shell command.
    Command,
    /// Creating, editing or deleting files.
    FileChange,
    /// Anything the adapter could not classify.
    Other,
}

/// An action the agent will take only once it is allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Opaque to everyone but the adapter that issued it; sent back with the answer.
    pub id: String,
    /// What sort of action it is.
    pub kind: PermissionKind,
    /// A one-line description for the user.
    pub summary: String,
    /// The provider's full request, as JSON text, for anyone who wants detail.
    pub detail: String,
}

/// One thing a running agent did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AgentEvent {
    /// The process is up and has accepted its first instructions.
    Started,
    /// Part of the reply the user reads.
    Text {
        /// The new text.
        text: String,
    },
    /// Part of the model's visible reasoning.
    Reasoning {
        /// The new text.
        text: String,
    },
    /// A tool call began.
    ToolStarted {
        /// The call.
        call: ToolCall,
    },
    /// A tool call ended.
    ToolFinished {
        /// The call's id.
        id: String,
        /// Whether it succeeded.
        ok: bool,
        /// Its output, as text.
        output: String,
    },
    /// The agent is blocked until this is allowed or refused.
    PermissionRequested {
        /// The request.
        request: PermissionRequest,
    },
    /// A turn ended normally; the agent waits for the next message.
    TurnCompleted {
        /// The whole reply of the turn.
        answer: String,
    },
    /// A turn ended in an error; the agent may still accept another message.
    TurnFailed {
        /// What went wrong, in words.
        message: String,
    },
    /// The process is gone and accepts nothing more.
    Exited {
        /// Why, when known.
        reason: Option<String>,
    },
}
