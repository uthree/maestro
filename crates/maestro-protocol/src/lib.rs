//! What a frontend and the daemon say to each other.
//!
//! The daemon usually runs on a different machine from the frontend, reached by
//! `ssh host maestro-daemon --stdio`, so everything that crosses between them is a serialisable
//! message on a byte stream. A frontend sends [`ClientMessage`]s: each carries a [`Command`] and an
//! id. The daemon answers every one with a [`ServerMessage::Response`] carrying the same id, and
//! separately broadcasts [`Event`]s so that a second frontend watching the same daemon (the TUI
//! on a phone over SSH, say) sees changes the first one made.
//!
//! The framing is one JSON object per line ([`write_message`], [`read_message`]): readable in a
//! terminal when debugging, and safe through SSH, which passes bytes but not file descriptors.

use std::io::{self, BufRead, Write};

use maestro_core::{
    AgentEvent, AgentId, AgentSpec, Choice, Difficulty, Host, ModelChoice, NodeDraft, NodeEdit,
    NodeId, Workflow, WorkflowId, WorkflowTarget, Workspace,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Bumped whenever a message changes shape. The daemon announces it first; a frontend that
/// disagrees disconnects instead of misreading.
pub const PROTOCOL_VERSION: u32 = 3;

/// Pairs a response with the request it answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

/// Everything a frontend can ask of the daemon. Both the GUI and the TUI speak only this.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command")]
pub enum Command {
    /// Checks the connection; answered with [`Reply::Pong`].
    Ping,
    /// Lists every workflow.
    ListWorkflows,
    /// Fetches one workflow in full.
    GetWorkflow {
        /// Which one.
        workflow: WorkflowId,
    },
    /// Creates an empty workflow.
    CreateWorkflow {
        /// Its name.
        title: String,
    },
    /// Adds a node to a workflow.
    AddNode {
        /// Which workflow.
        workflow: WorkflowId,
        /// The node.
        node: NodeDraft,
    },
    /// Removes a node and its edges.
    RemoveNode {
        /// Which workflow.
        workflow: WorkflowId,
        /// Which node.
        node: NodeId,
    },
    /// Makes `to` wait for `from`.
    AddEdge {
        /// Which workflow.
        workflow: WorkflowId,
        /// The dependency.
        from: NodeId,
        /// The dependent.
        to: NodeId,
    },
    /// Removes a dependency.
    RemoveEdge {
        /// Which workflow.
        workflow: WorkflowId,
        /// The dependency.
        from: NodeId,
        /// The dependent.
        to: NodeId,
    },
    /// Sets where a workflow's workers run. Needed before [`Command::RunWorkflow`].
    SetWorkflowTarget {
        /// Which workflow.
        workflow: WorkflowId,
        /// The repository and branches, or `None` to make it a sketch again.
        target: Option<WorkflowTarget>,
    },
    /// Edits a node's title, instructions, model or difficulty.
    EditNode {
        /// Which workflow.
        workflow: WorkflowId,
        /// Which node.
        node: NodeId,
        /// The changes.
        edit: NodeEdit,
    },
    /// Starts every node whose dependencies have succeeded, and keeps starting nodes as others
    /// succeed. Running a running workflow again restarts only what is ready and not running.
    RunWorkflow {
        /// Which workflow.
        workflow: WorkflowId,
    },
    /// Writes to the worker on a node, asking it for changes. Reopens a node that succeeded,
    /// and counts as a revision request in the model's record.
    SendToNode {
        /// Which workflow.
        workflow: WorkflowId,
        /// Which node.
        node: NodeId,
        /// The message.
        text: String,
    },
    /// Records the user's rating of a finished node, 1 (poor) to 5 (excellent).
    RateNode {
        /// Which workflow.
        workflow: WorkflowId,
        /// Which node.
        node: NodeId,
        /// The rating.
        rating: u8,
    },
    /// Starts a commander: an agent that converses with the user and writes plans, which become
    /// new workflows. The user talks to it with [`Command::SendToAgent`].
    StartCommander {
        /// The model it runs on.
        model: ModelChoice,
        /// Where it runs and what it may read. It is given no worktree and may not write.
        workspace: Workspace,
        /// Where the workflows it plans will run.
        target: Option<WorkflowTarget>,
        /// The user's first message.
        prompt: String,
    },
    /// Asks the router which model it would give a node, without starting anything.
    ChooseModel {
        /// How hard the node is.
        difficulty: Difficulty,
        /// The host the worker would run on; only providers installed there count.
        host: Host,
    },
    /// Merges every node's branch into one branch, pushes it, and opens a pull request.
    PublishWorkflow {
        /// Which workflow; every node must have succeeded.
        workflow: WorkflowId,
    },
    /// Starts an agent process, creating its worktree first when it has one.
    StartAgent {
        /// Which CLI, model and workspace.
        spec: AgentSpec,
        /// Standing instructions: the agent's role and limits.
        instructions: String,
        /// The first message.
        prompt: String,
    },
    /// Lists every agent the daemon has started and not yet forgotten.
    ListAgents,
    /// Sends the next message to an agent between turns. This is also how a user writes to a
    /// workflow node directly.
    SendToAgent {
        /// Which agent.
        agent: AgentId,
        /// The message.
        text: String,
    },
    /// Answers an agent's [`AgentEvent::PermissionRequested`].
    AnswerPermission {
        /// Which agent.
        agent: AgentId,
        /// The request's id.
        request: String,
        /// Allow, or refuse.
        allow: bool,
    },
    /// Asks an agent to abandon its current turn.
    InterruptAgent {
        /// Which agent.
        agent: AgentId,
    },
    /// Kills an agent's process.
    StopAgent {
        /// Which agent.
        agent: AgentId,
    },
    /// Commits everything in a workspace, pushes its branch and opens a pull request for it.
    PublishWorkspace {
        /// The workspace; it must have a worktree, whose branch is pushed.
        workspace: Workspace,
        /// The commit message, used only when there is something to commit.
        message: String,
        /// The pull request's title.
        title: String,
        /// The pull request's description.
        body: String,
        /// The branch to merge into.
        base: String,
        /// Open it as a draft.
        draft: bool,
    },
    /// Merges a pull request opened from a workspace.
    MergePullRequest {
        /// Any checkout of the repository.
        workspace: Workspace,
        /// The pull request's number.
        number: u64,
        /// How to merge it.
        method: MergeMethod,
    },
    /// Removes a workspace's worktree, keeping its branch.
    RemoveWorktree {
        /// The workspace.
        workspace: Workspace,
        /// Discard uncommitted changes rather than refuse.
        force: bool,
    },
}

/// How a pull request is merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    /// One commit on the base branch.
    Squash,
    /// A merge commit.
    Merge,
    /// Each commit replayed onto the base branch.
    Rebase,
}

/// Where an agent is in its life, as far as the daemon can tell from its events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Working on a turn.
    Working,
    /// Blocked on a permission request.
    AwaitingPermission,
    /// Between turns, waiting for a message.
    Idle,
    /// The process has exited.
    Exited,
}

/// One agent, for lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    /// Its id.
    pub id: AgentId,
    /// How it was started.
    pub spec: AgentSpec,
    /// Where it is.
    pub status: AgentStatus,
}

/// A one-line summary of a workflow, for lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSummary {
    /// Its id.
    pub id: WorkflowId,
    /// Its name.
    pub title: String,
    /// How many nodes it has.
    pub nodes: usize,
}

/// A successful answer to a [`Command`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reply")]
pub enum Reply {
    /// Answer to [`Command::Ping`].
    Pong,
    /// The command succeeded and has nothing to return.
    Done,
    /// Every workflow.
    Workflows {
        /// One line each.
        workflows: Vec<WorkflowSummary>,
    },
    /// One workflow in full.
    Workflow {
        /// The graph.
        workflow: Workflow,
    },
    /// A workflow was created.
    WorkflowCreated {
        /// Its id.
        workflow: WorkflowId,
    },
    /// A node was added.
    NodeAdded {
        /// Its id.
        node: NodeId,
    },
    /// An agent was started.
    AgentStarted {
        /// Its id; its events carry it.
        agent: AgentId,
    },
    /// Every agent.
    Agents {
        /// One entry each.
        agents: Vec<AgentSummary>,
    },
    /// The router's choice, or `None` when no catalog model is installed on the host.
    ModelChosen {
        /// The choice, with its score and how much record informed it.
        choice: Option<Choice>,
    },
    /// A pull request was opened.
    PullRequestOpened {
        /// Its number.
        number: u64,
        /// Its page.
        url: String,
        /// The commit made first, if there was anything to commit.
        commit: Option<String>,
    },
}

/// Why a command failed. The daemon's state is unchanged after any failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum CommandError {
    /// No workflow has that id.
    #[error("workflow {workflow} does not exist")]
    UnknownWorkflow {
        /// The id asked for.
        workflow: WorkflowId,
    },
    /// The workflow refused the edit.
    #[error("{message}")]
    Workflow {
        /// What the workflow said, in words.
        message: String,
    },
    /// No agent has that id.
    #[error("agent {agent} does not exist")]
    UnknownAgent {
        /// The id asked for.
        agent: AgentId,
    },
    /// The command needs a workspace with a worktree.
    #[error("the workspace has no worktree")]
    NoWorktree,
    /// A process on the host failed: the CLI, `git` or `gh`.
    #[error("{message}")]
    Host {
        /// What failed, with the program's own error text.
        message: String,
    },
}

/// Something that changed in the daemon, sent to every connected frontend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Event {
    /// A workflow was created or edited; the whole graph is sent so a view can redraw from it.
    WorkflowChanged {
        /// The graph as it now is.
        workflow: Workflow,
    },
    /// A commander wrote a plan that could not become a workflow.
    PlanRejected {
        /// The commander.
        agent: AgentId,
        /// Why, in words; the commander is told the same.
        message: String,
    },
    /// A node could not start, or its worker's work could not be kept; the node is now failed.
    NodeFailed {
        /// Which workflow.
        workflow: WorkflowId,
        /// Which node.
        node: NodeId,
        /// What went wrong, in words.
        message: String,
    },
    /// A workflow's pull request was opened, by request or because every node succeeded.
    WorkflowPublished {
        /// Which workflow.
        workflow: WorkflowId,
        /// The pull request's number.
        number: u64,
        /// Its page.
        url: String,
    },
    /// A commander's plan became a workflow, sent as [`Event::WorkflowChanged`] as well.
    Planned {
        /// The commander.
        agent: AgentId,
        /// The new workflow.
        workflow: WorkflowId,
    },
    /// An agent did something.
    Agent {
        /// Which agent.
        agent: AgentId,
        /// What it did.
        event: AgentEvent,
    },
}

/// What a frontend sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientMessage {
    /// Echoed back in the response.
    pub id: RequestId,
    /// What to do.
    pub command: Command,
}

/// What the daemon sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServerMessage {
    /// The first message on every connection.
    Hello {
        /// The daemon's [`PROTOCOL_VERSION`].
        protocol: u32,
    },
    /// The answer to one [`ClientMessage`].
    Response {
        /// The request's id.
        id: RequestId,
        /// What happened.
        result: Result<Reply, CommandError>,
    },
    /// A broadcast change.
    Event {
        /// The change.
        event: Event,
    },
}

/// A message could not be read or written.
#[derive(Debug, Error)]
pub enum FrameError {
    /// The stream failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A line was not a valid message.
    #[error("malformed message: {0}")]
    Malformed(#[from] serde_json::Error),
}

/// Writes one message as a line of JSON and flushes it.
pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<(), FrameError> {
    // One write per message: a socket or pipe then never carries half a line.
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()?;
    Ok(())
}

/// Reads one message, or `None` at the end of the stream. Blank lines are skipped.
pub fn read_message<T: DeserializeOwned>(
    reader: &mut impl BufRead,
) -> Result<Option<T>, FrameError> {
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            return Ok(Some(serde_json::from_str(&line)?));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_survive_framing() {
        let sent = ClientMessage {
            id: RequestId(7),
            command: Command::AddEdge { workflow: WorkflowId(1), from: NodeId(1), to: NodeId(2) },
        };
        let mut buffer = Vec::new();
        write_message(&mut buffer, &sent).unwrap();
        write_message(&mut buffer, &sent).unwrap();
        assert_eq!(buffer.iter().filter(|&&b| b == b'\n').count(), 2);

        let mut reader = &buffer[..];
        for _ in 0..2 {
            assert_eq!(read_message::<ClientMessage>(&mut reader).unwrap(), Some(sent.clone()));
        }
        assert_eq!(read_message::<ClientMessage>(&mut reader).unwrap(), None);
    }

    #[test]
    fn a_workflow_with_nodes_round_trips_inside_an_event() {
        let mut workflow = Workflow::new(WorkflowId(1), "t");
        let a = workflow.add_node(NodeDraft {
            title: "a".into(),
            instructions: String::new(),
            model: None,
            difficulty: None,
            origin: maestro_core::NodeOrigin::User,
        });
        let b = workflow.add_node(NodeDraft {
            title: "b".into(),
            instructions: String::new(),
            model: None,
            difficulty: None,
            origin: maestro_core::NodeOrigin::User,
        });
        workflow.add_edge(a, b).unwrap();
        let sent = ServerMessage::Event { event: Event::WorkflowChanged { workflow } };
        let text = serde_json::to_string(&sent).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&text).unwrap(), sent);
    }

    #[test]
    fn errors_round_trip_inside_a_response() {
        let sent = ServerMessage::Response {
            id: RequestId(3),
            result: Err(CommandError::UnknownWorkflow { workflow: WorkflowId(9) }),
        };
        let text = serde_json::to_string(&sent).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&text).unwrap(), sent);
    }
}
