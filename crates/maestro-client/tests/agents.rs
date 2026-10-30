//! A worker started through the protocol, in its own worktree, asking for and receiving
//! permission. A shell script stands in for `claude` so the test needs no account.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::mirror::Entry;
use maestro_client::{Client, Launch, Mirror};
use maestro_core::{
    AgentEvent, AgentId, AgentRole, AgentSpec, Effort, Host, ModelChoice, ModelGrade, ProviderKind,
    Workspace, Worktree,
};
use maestro_daemon::Hub;
use maestro_protocol::{AgentStatus, Command, Event, Reply};

const FAKE_CLAUDE: &str = r#"#!/bin/sh
read prompt
echo '{"type":"system","subtype":"init"}'
echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch made"}}}'
read answer
case "$answer" in
  *'"allow"'*) touch made; echo '{"type":"result","subtype":"success","is_error":false,"result":"allowed"}' ;;
  *) echo '{"type":"result","subtype":"success","is_error":false,"result":"denied"}' ;;
esac
read next
echo '{"type":"result","subtype":"success","is_error":false,"result":"second"}'
"#;

fn git(directory: &Path, args: &[&str]) {
    let status = std::process::Command::new("git").arg("-C").arg(directory).args(args).status();
    assert!(status.unwrap().success(), "git {args:?}");
}

fn next_agent_event(events: &Receiver<Event>, agent: AgentId) -> AgentEvent {
    loop {
        match events.recv_timeout(Duration::from_secs(10)).expect("an agent event") {
            Event::Agent { agent: from, event } if from == agent => return event,
            _ => {}
        }
    }
}

fn until(
    events: &Receiver<Event>,
    agent: AgentId,
    wanted: impl Fn(&AgentEvent) -> bool,
) -> AgentEvent {
    loop {
        let event = next_agent_event(events, agent);
        if wanted(&event) {
            return event;
        }
    }
}

#[test]
fn a_worker_runs_in_its_worktree_and_waits_for_permission() {
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    // SAFETY: this test binary runs this one test, and sets PATH before starting any thread.
    unsafe { std::env::set_var("PATH", path) };

    let repository = tempfile::tempdir().unwrap();
    git(repository.path(), &["init", "--quiet", "--initial-branch=main"]);
    git(
        repository.path(),
        &[
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@e",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "root",
        ],
    );
    let trees = tempfile::tempdir().unwrap();
    let worktree = Worktree {
        branch: "maestro/n1".into(),
        base: "main".into(),
        path: trees.path().join("n1"),
    };

    let client = Client::connect(&Launch::InProcess(Hub::new())).unwrap();
    let events = client.take_events().unwrap();
    let spec = AgentSpec {
        role: AgentRole::Worker,
        model: ModelChoice {
            provider: ProviderKind::ClaudeCode,
            model: String::new(),
            grade: ModelGrade::Standard,
            effort: Effort::Medium,
        },
        workspace: Workspace {
            host: Host::Local,
            repository: repository.path().into(),
            worktree: Some(worktree.clone()),
        },
        project: None,
    };
    let Reply::AgentStarted { agent } = client
        .request(Command::StartAgent {
            spec,
            instructions: "Be brief.".into(),
            prompt: "Make a file".into(),
        })
        .unwrap()
    else {
        panic!("expected an agent");
    };

    let AgentEvent::PermissionRequested { request } =
        until(&events, agent, |event| matches!(event, AgentEvent::PermissionRequested { .. }))
    else {
        unreachable!()
    };
    assert_eq!(request.summary, "Bash: touch made");
    let Reply::Agents { agents } = client.request(Command::ListAgents).unwrap() else { panic!() };
    assert_eq!(agents[0].status, AgentStatus::AwaitingPermission);

    client.request(Command::AnswerPermission { agent, request: request.id, allow: true }).unwrap();
    let done = until(&events, agent, |event| matches!(event, AgentEvent::TurnCompleted { .. }));
    assert_eq!(done, AgentEvent::TurnCompleted { answer: "allowed".into() });
    assert!(worktree.path.join("made").exists(), "the worker ran inside its own worktree");

    client.request(Command::SendToAgent { agent, text: "Again".into() }).unwrap();
    let done = until(&events, agent, |event| matches!(event, AgentEvent::TurnCompleted { .. }));
    assert_eq!(done, AgentEvent::TurnCompleted { answer: "second".into() });
    assert_eq!(next_agent_event(&events, agent), AgentEvent::Exited { reason: None });

    // A frontend that connects now sees both sides of the conversation.
    let Reply::Transcript { events } = client.request(Command::GetTranscript { agent }).unwrap()
    else {
        panic!("expected a transcript");
    };
    let prompts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Prompted { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(prompts, ["Make a file", "Again"]);
    let mirror = Mirror::load(&client).unwrap();
    let users = mirror.agents[&agent]
        .transcript
        .entries
        .iter()
        .filter(|entry| matches!(entry, Entry::User(_)))
        .count();
    assert_eq!(users, 2);
}
