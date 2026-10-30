//! An organisation deeper than commander and workers: the commander delegates a step to a lead,
//! whose plan delegates a step to a lead of its own. Each lead's plan runs under its step, the
//! step succeeds with that work merged in, and the step after it builds on all of it. The limit
//! on agents at once holds throughout. One shell script stands in for `claude` in every role.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::{Client, Launch};
use maestro_core::{
    AgentRole, Effort, Host, ModelChoice, ModelGrade, NodeId, NodeRef, NodeStatus, ProviderKind,
    Resource, Workflow, WorkflowId,
};
use maestro_daemon::Hub;
use maestro_protocol::{Command, Event, Reply};

/// A reply carrying `plan`, as `claude` prints it.
fn answer(plan: serde_json::Value) -> String {
    let result = format!("Here is the plan:\n```maestro-plan\n{plan}\n```");
    serde_json::json!({"type": "result", "subtype": "success", "is_error": false, "result": result})
        .to_string()
}

/// Planners (started with `--disallowedTools`) answer by the step they were given: the lead of
/// "Outer", the lead of "Inner", or else the commander. Workers leave a file named after their
/// worktree and a list of what they found there. Every agent but the commander holds
/// `$MAESTRO_TEST_DIR/busy` during its turn, and leaves `overlap` if another already held it.
fn fake_claude() -> String {
    let commander = answer(serde_json::json!({"title": "Org", "nodes": [
        {"key": "outer", "title": "Outer", "instructions": "Plan it", "difficulty": "hard", "delegate": true},
        {"key": "after", "title": "After", "instructions": "Use it", "difficulty": "easy", "depends_on": ["outer"]}
    ]}));
    let outer = answer(serde_json::json!({"title": "Outer work", "nodes": [
        {"key": "inner", "title": "Inner", "instructions": "Plan it too", "delegate": true},
        {"key": "a", "title": "Leaf A", "instructions": "Do a"}
    ]}));
    let inner = answer(serde_json::json!({"title": "Inner work", "nodes": [
        {"key": "b", "title": "Leaf B", "instructions": "Do b"},
        {"key": "c", "title": "Leaf C", "instructions": "Do c", "depends_on": ["b"]}
    ]}));
    format!(
        r##"#!/bin/sh
hold() {{ mkdir "$MAESTRO_TEST_DIR/busy" 2>/dev/null || touch "$MAESTRO_TEST_DIR/overlap"; sleep 0.2; }}
free() {{ rmdir "$MAESTRO_TEST_DIR/busy"; }}
read prompt
case "$*" in
  *--disallowedTools*)
    case "$prompt" in
      *"# Outer"*) hold; free; cat <<'PLAN'
{outer}
PLAN
      ;;
      *"# Inner"*) hold; free; cat <<'PLAN'
{inner}
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
hold
here=$(basename "$PWD")
ls > "$here.seen"
touch "$here.txt"
free
echo '{{"type":"result","subtype":"success","is_error":false,"result":"done"}}'
while read message; do :; done
"##
    )
}

fn git(directory: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").arg("-C").arg(directory).args(args).output();
    assert!(output.unwrap().status.success(), "git {args:?}");
}

/// The first workflow change that satisfies `wanted`.
fn until(events: &Receiver<Event>, wanted: impl Fn(&Workflow) -> bool) -> Workflow {
    loop {
        match events.recv_timeout(Duration::from_secs(30)).expect("a workflow change") {
            Event::WorkflowChanged { workflow } if wanted(&workflow) => return workflow,
            Event::NodeFailed { message, .. } => panic!("a node failed: {message}"),
            Event::PlanRejected { message, .. } => panic!("a plan was rejected: {message}"),
            _ => {}
        }
    }
}

fn status(flow: &Workflow, node: u64) -> NodeStatus {
    flow.node(NodeId(node)).unwrap().status
}

#[test]
fn leads_plan_under_their_steps_to_any_depth_within_the_limit() {
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
    let events = client.take_events().unwrap();

    // One agent at a time, however deep the organisation goes.
    let Reply::Router { mut router } = client.request(Command::GetRouter).unwrap() else {
        panic!()
    };
    router.max_agents = 1;
    client.request(Command::SetRouter { router }).unwrap();

    let folder = Resource::Directory {
        host: Host::Local,
        path: repository.path().into(),
        base: "main".into(),
    };
    let created = client.request(Command::CreateProject {
        name: "Org".into(),
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

    let top = until(&events, |flow| flow.title == "Org");
    assert!(top.node(NodeId(1)).unwrap().delegate);
    let outer = NodeRef { workflow: top.id, node: NodeId(1) };

    // The lead of "Outer" plans under it, and the lead of "Inner" under that.
    let middle = until(&events, |flow| flow.title == "Outer work");
    assert_eq!(middle.parent, Some(outer));
    assert_eq!(middle.project, Some(project));
    assert_eq!(middle.target.as_ref().unwrap().base, "maestro/wf1-n1");
    let inner = NodeRef { workflow: middle.id, node: NodeId(1) };
    let bottom = until(&events, |flow| flow.title == "Inner work");
    assert_eq!(bottom.parent, Some(inner));
    assert_eq!(bottom.target.as_ref().unwrap().base, format!("maestro/{}-n1", middle.id));

    // The step after "Outer" runs once everything under it has, and builds on all of it.
    let done = until(&events, |flow| flow.id == top.id && status(flow, 2) == NodeStatus::Succeeded);
    assert_eq!(status(&done, 1), NodeStatus::Succeeded);
    let seen = std::fs::read_to_string(trees.join("maestro-wf1-n2/maestro-wf1-n2.seen")).unwrap();
    for leaf in [format!("maestro-{}-n2", middle.id), format!("maestro-{}-n1", bottom.id)] {
        assert!(seen.contains(&format!("{leaf}.txt")), "{leaf} in {seen}");
    }
    let leaf_c = format!("maestro-{}-n2", bottom.id);
    assert!(seen.contains(&format!("{leaf_c}.txt")), "{seen}");
    assert!(!scratch.path().join("overlap").exists(), "two agents worked at once");

    // The steps that were delegated went to leads.
    let Reply::Agents { agents } = client.request(Command::ListAgents).unwrap() else { panic!() };
    let lead = done.node(NodeId(1)).unwrap().agent.unwrap();
    assert_eq!(agents.iter().find(|a| a.id == lead).unwrap().spec.role, AgentRole::Lead);
    let Reply::Workflow { workflow: middle } =
        client.request(Command::GetWorkflow { workflow: WorkflowId(middle.id.0) }).unwrap()
    else {
        panic!()
    };
    assert_eq!(status(&middle, 1), NodeStatus::Succeeded);
    std::fs::remove_dir_all(&trees).ok();
}
