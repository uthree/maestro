//! Messages sent while a worker is in its turn wait for it to end, and the person can take one
//! back, pause them, or send one now. A shell script stands in for `claude`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::{Client, Launch, Mirror};
use maestro_core::{
    AgentEvent, AgentId, AgentRole, AgentSpec, Effort, Host, ModelChoice, ModelGrade, ProviderKind,
    Workspace,
};
use maestro_daemon::Hub;
use maestro_protocol::{AgentStatus, Command, CommandError, Event, QueueEdit, Reply};

/// Holds its first turn open on a permission request, then answers every message at once.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
read prompt
echo '{"type":"system","subtype":"init"}'
echo '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"true"}}}'
read answer
echo '{"type":"result","subtype":"success","is_error":false,"result":"first"}'
while read next; do
  echo '{"type":"result","subtype":"success","is_error":false,"result":"again"}'
done
"#;

fn until(
    events: &Receiver<Event>,
    agent: AgentId,
    wanted: impl Fn(&AgentEvent) -> bool,
) -> AgentEvent {
    loop {
        match events.recv_timeout(Duration::from_secs(10)).expect("an agent event") {
            Event::Agent { agent: from, event } if from == agent && wanted(&event) => {
                return event;
            }
            _ => {}
        }
    }
}

fn edit(client: &Client, agent: AgentId, edit: QueueEdit) {
    client.request(Command::EditQueue { agent, edit }).unwrap();
}

#[test]
fn the_waiting_messages_can_be_taken_back_paused_and_sent_now() {
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    // SAFETY: this test binary runs this one test, and sets PATH before starting any thread.
    unsafe { std::env::set_var("PATH", path) };
    let directory = tempfile::tempdir().unwrap();

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
            repository: directory.path().into(),
            worktree: None,
        },
        project: None,
        subagents: true,
    };
    let Reply::AgentStarted { agent } = client
        .request(Command::StartAgent {
            spec,
            instructions: "Be brief.".into(),
            prompt: "Start".into(),
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

    for text in ["A", "B", "C"] {
        client.request(Command::SendToAgent { agent, text: text.into() }).unwrap();
    }
    edit(&client, agent, QueueEdit::Remove { index: 1 });
    let changed = until(&events, agent, |event| matches!(event, AgentEvent::QueueChanged { .. }));
    assert_eq!(
        changed,
        AgentEvent::QueueChanged { queued: vec!["A".into(), "C".into()], paused: false }
    );
    let missing =
        client.request(Command::EditQueue { agent, edit: QueueEdit::Remove { index: 5 } });
    assert!(matches!(
        missing,
        Err(maestro_client::ClientError::Refused(CommandError::NotQueued { index: 5 }))
    ));

    // Paused, the turn ends and nothing is sent.
    edit(&client, agent, QueueEdit::Pause { paused: true });
    client.request(Command::AnswerPermission { agent, request: request.id, allow: true }).unwrap();
    until(&events, agent, |event| matches!(event, AgentEvent::TurnCompleted { .. }));
    let Reply::Agents { agents } = client.request(Command::ListAgents).unwrap() else { panic!() };
    assert_eq!(agents[0].status, AgentStatus::Idle);
    let mirror = Mirror::load(&client).unwrap();
    assert_eq!(mirror.agents[&agent].transcript.queued, ["A", "C"]);
    assert!(mirror.agents[&agent].transcript.paused);

    // "Send now" puts C first and resumes; between turns it goes at once, then A after it.
    edit(&client, agent, QueueEdit::SendNow { index: 1 });
    let sent = until(&events, agent, |event| matches!(event, AgentEvent::Prompted { .. }));
    assert_eq!(sent, AgentEvent::Prompted { text: "C".into() });
    let sent = until(&events, agent, |event| matches!(event, AgentEvent::Prompted { .. }));
    assert_eq!(sent, AgentEvent::Prompted { text: "A".into() });
    until(&events, agent, |event| matches!(event, AgentEvent::TurnCompleted { .. }));
    let mirror = Mirror::load(&client).unwrap();
    assert!(mirror.agents[&agent].transcript.queued.is_empty());
    assert!(!mirror.agents[&agent].transcript.paused);
}
