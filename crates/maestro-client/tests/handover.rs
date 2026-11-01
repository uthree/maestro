//! A step that inherits the conversation: its worker is handed what the user and the commander
//! said before the plan, and the step after it is handed what that worker reported. One shell
//! script stands in for `claude` as commander and worker, so the test needs no account.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::{Client, Launch};
use maestro_core::{
    Effort, Host, ModelChoice, ModelGrade, NodeId, NodeStatus, ProviderKind, Resource, Workflow,
};
use maestro_daemon::Hub;
use maestro_protocol::{Command, Event, Reply};

/// As commander, asks a question, then writes `plan` once answered. As worker, keeps its first
/// message in `$MAESTRO_TEST_DIR/<worktree>.prompt` and reports which worktree it worked in.
fn fake_claude(plan: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$*" in
  *--disallowedTools*)
    read prompt
    echo '{{"type":"result","subtype":"success","is_error":false,"result":"Which colour should the logo keep?"}}'
    read answer
    cat <<'PLAN'
{plan}
PLAN
    while read more; do :; done
    exit 0 ;;
esac
read prompt
here=$(basename "$PWD")
printf '%s' "$prompt" > "$MAESTRO_TEST_DIR/$here.prompt"
touch "$here.txt"
echo "{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"Finished in $here\"}}"
while read message; do :; done
"#
    )
}

fn git(directory: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").arg("-C").arg(directory).args(args).output();
    assert!(output.unwrap().status.success(), "git {args:?}");
}

/// The next version of a workflow that satisfies `wanted`.
fn until(events: &Receiver<Event>, wanted: impl Fn(&Workflow) -> bool) -> Workflow {
    loop {
        match events.recv_timeout(Duration::from_secs(20)).expect("a workflow change") {
            Event::WorkflowChanged { workflow } if wanted(&workflow) => return workflow,
            Event::NodeFailed { message, .. } => panic!("a node failed: {message}"),
            _ => {}
        }
    }
}

#[test]
fn a_step_is_handed_the_conversation_and_the_reports_before_it() {
    let plan = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": "Blue it is.\n```maestro-plan\n{\"title\": \"Dark mode\", \"nodes\": [
            {\"key\": \"a\", \"title\": \"Add dark mode\", \"instructions\": \"Add a dark theme\", \"difficulty\": \"moderate\", \"context\": \"conversation\"},
            {\"key\": \"b\", \"title\": \"Document it\", \"instructions\": \"Write the docs\", \"difficulty\": \"easy\", \"depends_on\": [\"a\"]}
        ]}\n```"
    });
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, fake_claude(&plan.to_string())).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    let scratch = tempfile::tempdir().unwrap();
    // SAFETY: this test binary runs this one test, and sets these before starting any thread.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("MAESTRO_TEST_DIR", scratch.path());
    }

    let repository = tempfile::tempdir().unwrap();
    git(repository.path(), &["init", "--quiet", "--initial-branch=main"]);
    git(repository.path(), &["config", "user.name", "T"]);
    git(repository.path(), &["config", "user.email", "t@e"]);
    git(repository.path(), &["commit", "--quiet", "--allow-empty", "-m", "root"]);

    let client = Client::connect(&Launch::InProcess(Hub::new())).unwrap();
    let events = client.take_events().unwrap();
    let folder = Resource::Directory {
        host: Host::Local,
        path: repository.path().into(),
        base: "main".into(),
    };
    let created = client.request(Command::CreateProject {
        name: "Site".into(),
        resources: vec![folder, Resource::Provider { provider: ProviderKind::ClaudeCode }],
    });
    let Ok(Reply::ProjectCreated { project }) = created else { panic!("{created:?}") };
    let model = ModelChoice {
        provider: ProviderKind::ClaudeCode,
        model: String::new(),
        grade: ModelGrade::Frontier,
        effort: Effort::High,
    };
    let started = client.request(Command::StartCommander {
        project,
        model,
        prompt: "Add a dark mode".into(),
    });
    let Ok(Reply::AgentStarted { agent: commander }) = started else { panic!("{started:?}") };
    client
        .request(Command::SendToAgent { agent: commander, text: "Keep the logo blue".into() })
        .unwrap();

    let planned = until(&events, |flow| flow.nodes().count() == 2);
    let first = planned.node(NodeId(1)).unwrap();
    let said = first.conversation.as_deref().expect("the first step inherits the conversation");
    for part in ["Add a dark mode", "Which colour should the logo keep?", "Keep the logo blue"] {
        assert!(said.contains(part), "{said}");
    }
    // The plan itself is not repeated: the worker has its own step's instructions.
    assert!(!said.contains("maestro-plan") && said.contains("Blue it is."), "{said}");
    assert_eq!(planned.node(NodeId(2)).unwrap().conversation, None);

    let workflow = planned.id;
    until(&events, |flow| {
        flow.id == workflow && flow.nodes().all(|node| node.status == NodeStatus::Succeeded)
    });
    let prompt = |node: u64| {
        let name = format!("maestro-{workflow}-n{node}.prompt");
        std::fs::read_to_string(scratch.path().join(name)).unwrap()
    };
    let first = prompt(1);
    assert!(first.contains("The conversation this step was planned in"), "{first}");
    assert!(first.contains("Keep the logo blue"), "{first}");
    let second = prompt(2);
    assert!(!second.contains("Keep the logo blue"), "{second}");
    assert!(second.contains("What the steps before this one reported"), "{second}");
    assert!(second.contains(&format!("Finished in maestro-{workflow}-n1")), "{second}");
}
