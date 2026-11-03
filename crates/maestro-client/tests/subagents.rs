//! Sub-agents are the planner's call. A worker is started with its CLI's sub-agent tools refused,
//! and stopped, its step failed, if it calls one anyway. A step whose worker may use them is
//! started without the refusal. A lead whose step may not delegate has a plan that delegates sent
//! back, and its plan without delegation then runs. One shell script stands in for `claude`.

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

/// A reply carrying `plan`, as `claude` prints it.
fn answer(plan: serde_json::Value) -> String {
    let result = format!("Here is the plan:\n```maestro-plan\n{plan}\n```");
    serde_json::json!({"type": "result", "subtype": "success", "is_error": false, "result": result})
        .to_string()
}

/// Planners (started read-only) answer by the step they were given: the lead of "Outer" first
/// delegates, then plans without delegating once told to; anyone else is the commander. The
/// worker of "Helper" calls `Task`; other workers just finish. Every worker leaves the tools it
/// was refused in `$MAESTRO_TEST_DIR/<worktree>.refused`.
fn fake_claude() -> String {
    let commander = answer(serde_json::json!({"title": "Job", "nodes": [
        {"key": "outer", "title": "Outer", "instructions": "Plan it", "delegate": true, "subagents": false},
        {"key": "helper", "title": "Helper", "instructions": "Do it"},
        {"key": "searcher", "title": "Searcher", "instructions": "Search", "subagents": true}
    ]}));
    let delegating = answer(serde_json::json!({"title": "Outer work", "nodes": [
        {"key": "deeper", "title": "Deeper", "instructions": "Plan more", "delegate": true}
    ]}));
    let flat = answer(serde_json::json!({"title": "Outer work", "nodes": [
        {"key": "leaf", "title": "Leaf", "instructions": "Do the leaf"}
    ]}));
    let task = serde_json::json!({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": "t1", "name": "Task", "input": {"prompt": "do it for me"}}
    ]}});
    format!(
        r##"#!/bin/sh
read prompt
case "$*" in
  *--disallowedTools\ Write*)
    case "$prompt" in
      *"# Outer"*) cat <<'PLAN'
{delegating}
PLAN
        read again
        cat <<'PLAN'
{flat}
PLAN
      ;;
      *) cat <<'PLAN'
{commander}
PLAN
      ;;
    esac
    while read next; do :; done
    exit 0 ;;
esac
here=$(basename "$PWD")
refused=""
take=0
for arg in "$@"; do
  if [ "$take" = 1 ]; then refused="$arg"; take=0; fi
  if [ "$arg" = "--disallowedTools" ]; then take=1; fi
done
printf '%s' "$refused" > "$MAESTRO_TEST_DIR/$here.refused"
case "$prompt" in
  *"# Helper"*) cat <<'TASK'
{task}
TASK
    while read message; do :; done
    exit 0 ;;
esac
touch "$here.txt"
echo '{{"type":"result","subtype":"success","is_error":false,"result":"done"}}'
while read message; do :; done
"##
    )
}

fn git(directory: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").arg("-C").arg(directory).args(args).output();
    assert!(output.unwrap().status.success(), "git {args:?}");
}

fn status(flow: &Workflow, node: u64) -> NodeStatus {
    flow.node(NodeId(node)).unwrap().status
}

#[test]
fn workers_and_leads_keep_to_what_their_step_allows() {
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, fake_claude()).unwrap();
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
    let trees = std::path::PathBuf::from(format!("{}-worktrees", repository.path().display()));

    let client = Client::connect(&Launch::InProcess(Hub::new())).unwrap();
    let events: Receiver<Event> = client.take_events().unwrap();
    let folder = Resource::Directory {
        host: Host::Local,
        path: repository.path().into(),
        base: "main".into(),
    };
    let created = client.request(Command::CreateProject {
        name: "Subagents".into(),
        resources: vec![folder, Resource::Provider { provider: ProviderKind::ClaudeCode }],
    });
    let Ok(Reply::ProjectCreated { project }) = created else { panic!("{created:?}") };
    let model = ModelChoice {
        provider: ProviderKind::ClaudeCode,
        model: String::new(),
        grade: ModelGrade::Frontier,
        effort: Effort::High,
    };
    client.request(Command::StartCommander { project, model, prompt: "Plan it".into() }).unwrap();

    // Until the top workflow's helper has failed, the lead's flat plan has run, and the
    // searcher has finished: what failed and what was sent back along the way.
    let (mut failures, mut rejections) = (Vec::new(), Vec::new());
    let mut under: Vec<Workflow> = Vec::new();
    let top = loop {
        match events.recv_timeout(Duration::from_secs(30)).expect("an event") {
            Event::WorkflowChanged { workflow } if workflow.title == "Job" => {
                let done = status(&workflow, 1) == NodeStatus::Succeeded
                    && status(&workflow, 2) == NodeStatus::Failed
                    && status(&workflow, 3) == NodeStatus::Succeeded;
                if done {
                    break workflow;
                }
            }
            Event::WorkflowChanged { workflow } if workflow.title == "Outer work" => {
                under.retain(|flow| flow.id != workflow.id);
                under.push(workflow);
            }
            Event::NodeFailed { node, message, .. } => failures.push((node, message)),
            Event::PlanRejected { message, .. } => rejections.push(message),
            _ => {}
        }
    };

    // The helper called `Task` and was stopped for it; nothing else failed.
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].0, NodeId(2));
    assert!(failures[0].1.contains("sub-agent (Task)"), "{}", failures[0].1);

    // The lead's delegating plan was sent back once, naming the step.
    assert_eq!(rejections.len(), 1, "{rejections:?}");
    assert!(rejections[0].contains("\"deeper\""), "{}", rejections[0]);
    assert_eq!(under.len(), 1);
    assert!(!under[0].nodes().any(|node| node.delegate));

    // Workers were refused the sub-agent tools unless their step allowed them.
    let refused = |node: u64| {
        let file = scratch.path().join(format!("maestro-{}-n{node}.refused", top.id));
        std::fs::read_to_string(file).unwrap()
    };
    assert_eq!(refused(2), "Task,Agent");
    assert_eq!(refused(3), "");
    let leaf = scratch.path().join(format!("maestro-{}-n1.refused", under[0].id));
    assert_eq!(std::fs::read_to_string(leaf).unwrap(), "Task,Agent");
    std::fs::remove_dir_all(&trees).ok();
}
