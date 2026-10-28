//! A commander's plan becoming a workflow, and the workflow running to the end: workers in
//! dependency order, each in its own worktree with its dependencies' work merged in, their work
//! committed, a revision request, and a rating. One shell script stands in for `claude` in both
//! roles, so the test needs no account.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::{Client, Launch};
use maestro_core::{
    Effort, Host, ModelChoice, ModelGrade, NodeId, NodeStatus, ProviderKind, Workflow,
    WorkflowTarget, Workspace,
};
use maestro_daemon::Hub;
use maestro_protocol::{Command, Event, Reply};

/// As commander (started with `--disallowedTools`), replies with a two-step plan. As worker,
/// leaves a file named after its worktree and a list of what it found there, then on each later
/// message leaves another file.
fn fake_claude(plan: &str) -> String {
    format!(
        r#"#!/bin/sh
case "$*" in
  *--disallowedTools*)
    read prompt
    cat <<'PLAN'
{plan}
PLAN
    read next
    exit 0 ;;
esac
read prompt
here=$(basename "$PWD")
ls > "$here.seen"
touch "$here.txt"
echo '{{"type":"result","subtype":"success","is_error":false,"result":"done"}}'
while read message; do
  touch "$here.revised"
  echo '{{"type":"result","subtype":"success","is_error":false,"result":"revised"}}'
done
"#
    )
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git").arg("-C").arg(directory).args(args).output();
    let output = output.unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// The next version of `workflow` that satisfies `wanted`.
fn until(events: &Receiver<Event>, wanted: impl Fn(&Workflow) -> bool) -> Workflow {
    loop {
        match events.recv_timeout(Duration::from_secs(20)).expect("a workflow change") {
            Event::WorkflowChanged { workflow } if wanted(&workflow) => return workflow,
            Event::NodeFailed { message, .. } => panic!("a node failed: {message}"),
            _ => {}
        }
    }
}

fn status(flow: &Workflow, node: u64) -> NodeStatus {
    flow.node(NodeId(node)).unwrap().status
}

#[test]
fn a_plan_runs_to_the_end_in_dependency_order() {
    let plan = serde_json::json!({
        "type": "result", "subtype": "success", "is_error": false,
        "result": "Agreed. Here is the plan:\n```maestro-plan\n{\"title\": \"Two steps\", \"nodes\": [
            {\"key\": \"a\", \"title\": \"First\", \"instructions\": \"Do a\", \"difficulty\": \"easy\", \"depends_on\": []},
            {\"key\": \"b\", \"title\": \"Second\", \"instructions\": \"Do b\", \"difficulty\": \"hard\", \"depends_on\": [\"a\"]}
        ]}\n```"
    });
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, fake_claude(&plan.to_string())).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    // SAFETY: this test binary runs this one test, and sets PATH before starting any thread.
    unsafe { std::env::set_var("PATH", path) };

    let repository = tempfile::tempdir().unwrap();
    git(repository.path(), &["init", "--quiet", "--initial-branch=main"]);
    git(repository.path(), &["config", "user.name", "T"]);
    git(repository.path(), &["config", "user.email", "t@e"]);
    git(repository.path(), &["commit", "--quiet", "--allow-empty", "-m", "root"]);
    let trees = tempfile::tempdir().unwrap();
    let target = WorkflowTarget {
        host: Host::Local,
        repository: repository.path().into(),
        base: "main".into(),
        worktrees: trees.path().into(),
        publish: false,
    };

    let client = Client::connect(&Launch::InProcess(Hub::new())).unwrap();
    let events = client.take_events().unwrap();

    // Only Claude Code is on this PATH, so the router must choose it, at the difficulty's effort.
    let Reply::ModelChosen { choice: Some(choice) } = client
        .request(Command::ChooseModel {
            difficulty: maestro_core::Difficulty::Hard,
            host: Host::Local,
        })
        .unwrap()
    else {
        panic!("expected a model");
    };
    assert_eq!(
        (choice.model.provider, choice.model.effort),
        (ProviderKind::ClaudeCode, Effort::High)
    );

    let commander = client
        .request(Command::StartCommander {
            model: ModelChoice {
                provider: ProviderKind::ClaudeCode,
                model: String::new(),
                grade: ModelGrade::Frontier,
                effort: Effort::High,
            },
            workspace: Workspace {
                host: Host::Local,
                repository: repository.path().into(),
                worktree: None,
            },
            target: Some(target),
            prompt: "Plan two steps".into(),
        })
        .unwrap();
    assert!(matches!(commander, Reply::AgentStarted { .. }));
    let planned = until(&events, |flow| flow.nodes().count() == 2);
    assert_eq!(planned.title, "Two steps");
    assert_eq!(planned.topological_order(), [NodeId(1), NodeId(2)]);
    let workflow = planned.id;

    client.request(Command::RunWorkflow { workflow }).unwrap();
    let done = until(&events, |flow| status(flow, 2) == NodeStatus::Succeeded);
    assert_eq!(status(&done, 1), NodeStatus::Succeeded);

    // The router chose by difficulty: low effort for the easy step, high for the hard one.
    let effort = |node| done.node(NodeId(node)).unwrap().model.as_ref().unwrap().effort;
    assert_eq!((effort(1), effort(2)), (Effort::Low, Effort::High));

    // Each worker worked in its own worktree; the second saw the first's work already merged in.
    let first = trees.path().join("maestro-wf1-n1");
    let second = trees.path().join("maestro-wf1-n2");
    let seen = std::fs::read_to_string(second.join("maestro-wf1-n2.seen")).unwrap();
    assert!(seen.contains("maestro-wf1-n1.txt"), "{seen}");
    // Their work was committed on their branches.
    assert!(git(&first, &["status", "--porcelain"]).is_empty());
    assert_eq!(git(&first, &["log", "-1", "--format=%s"]).trim(), "First");
    assert_eq!(git(&second, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(), "maestro/wf1-n2");

    // A message to a node reopens it, and its next completion is committed too.
    client
        .request(Command::SendToNode { workflow, node: NodeId(1), text: "Tweak it".into() })
        .unwrap();
    until(&events, |flow| status(flow, 1) == NodeStatus::Running);
    until(&events, |flow| status(flow, 1) == NodeStatus::Succeeded);
    assert!(first.join("maestro-wf1-n1.revised").exists());
    assert!(git(&first, &["status", "--porcelain"]).is_empty());

    client.request(Command::RateNode { workflow, node: NodeId(1), rating: 5 }).unwrap();
    assert!(client.request(Command::RateNode { workflow, node: NodeId(1), rating: 9 }).is_err());
}
