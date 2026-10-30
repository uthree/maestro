//! A hard node run on two models at once: each contender works in its own worktree, the node
//! waits for the user to pick one, the pick is what its dependents build on, and the router
//! learns from it. One shell script stands in for `claude` on both models.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::{Client, Launch};
use maestro_core::{
    Catalog, Difficulty, Host, ModelGrade, ModelProfile, NodeDraft, NodeId, NodeOrigin, NodeStatus,
    Outcome, ProviderKind, Router, Workflow, WorkflowTarget,
};
use maestro_daemon::Hub;
use maestro_protocol::{Command, Event, Reply};

/// Leaves a file named after its worktree and a list of what it found there.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
read prompt
here=$(basename "$PWD")
ls > "$here.seen"
touch "$here.txt"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done in $here\"}"
while read message; do :; done
"#;

fn git(directory: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").arg("-C").arg(directory).args(args).output();
    assert!(output.unwrap().status.success(), "git {args:?}");
}

fn until(events: &Receiver<Event>, wanted: impl Fn(&Workflow) -> bool) -> Workflow {
    loop {
        match events.recv_timeout(Duration::from_secs(20)).expect("a workflow change") {
            Event::WorkflowChanged { workflow } if wanted(&workflow) => return workflow,
            Event::NodeFailed { message, .. } => panic!("a node failed: {message}"),
            _ => {}
        }
    }
}

fn status(flow: &Workflow, node: u64) -> Option<NodeStatus> {
    flow.node(NodeId(node)).map(|node| node.status)
}

fn draft(title: &str, difficulty: Difficulty, tags: &[&str]) -> NodeDraft {
    NodeDraft {
        title: title.into(),
        instructions: format!("Do {title}"),
        model: None,
        difficulty: Some(difficulty),
        tags: tags.iter().map(|&tag| tag.to_owned()).collect(),
        origin: NodeOrigin::User,
    }
}

#[test]
fn the_users_pick_is_kept_and_learned_from() {
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    // SAFETY: this test binary runs this one test, and sets PATH before starting any thread.
    unsafe { std::env::set_var("PATH", path) };

    // Two models on the one installed CLI, with nothing to tell them apart yet.
    let home = tempfile::tempdir().unwrap();
    let profile = |model: &str| ModelProfile {
        provider: ProviderKind::ClaudeCode,
        model: model.into(),
        grade: ModelGrade::Frontier,
        benchmarks: Default::default(),
    };
    let router = Router {
        catalog: Catalog { models: vec![profile("first"), profile("second")] },
        ..Router::default()
    };
    std::fs::write(home.path().join("router.json"), serde_json::to_vec(&router).unwrap()).unwrap();

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

    let client = Client::connect(&Launch::InProcess(Hub::open(home.path()).unwrap())).unwrap();
    let events = client.take_events().unwrap();
    let Reply::WorkflowCreated { workflow } =
        client.request(Command::CreateWorkflow { title: "Compare".into(), project: None }).unwrap()
    else {
        panic!("expected a workflow");
    };
    let add = |node| match client.request(Command::AddNode { workflow, node }).unwrap() {
        Reply::NodeAdded { node } => node,
        other => panic!("{other:?}"),
    };
    let hard = add(draft("hard", Difficulty::Hard, &["ui"]));
    let easy = add(draft("easy", Difficulty::Easy, &[]));
    client.request(Command::AddEdge { workflow, from: hard, to: easy }).unwrap();
    client.request(Command::SetWorkflowTarget { workflow, target: Some(target) }).unwrap();
    client.request(Command::RunWorkflow { workflow }).unwrap();

    // The hard node ran on both models, each on its own branch, and waits for a pick.
    let compared = until(&events, |flow| status(flow, 1) == Some(NodeStatus::AwaitingChoice));
    let node = compared.node(hard).unwrap();
    let branches: Vec<&str> = node.contenders.iter().map(|c| c.branch.as_str()).collect();
    assert_eq!(branches, ["maestro/wf1-n1-c1", "maestro/wf1-n1-c2"]);
    let models: Vec<&str> = node.contenders.iter().map(|c| c.model.model.as_str()).collect();
    assert_eq!(models, ["first", "second"]);
    assert_eq!(status(&compared, 2), Some(NodeStatus::Pending), "its dependent waits for the pick");
    assert!(
        client.request(Command::RateNode { workflow, node: hard, rating: 3 }).is_err(),
        "nothing is kept yet to rate"
    );

    let picked = node.contenders[1].agent;
    let lost = node.contenders[0].agent;
    client.request(Command::ChooseContender { workflow, node: hard, agent: picked }).unwrap();
    let done = until(&events, |flow| status(flow, 2) == Some(NodeStatus::Succeeded));
    let kept = done.node(hard).unwrap();
    assert_eq!((kept.agent, kept.branch.as_deref()), (Some(picked), Some("maestro/wf1-n1-c2")));
    // The easy node is not compared, and built on the picked work alone.
    assert!(done.node(easy).unwrap().contenders.is_empty());
    let seen = std::fs::read_to_string(trees.path().join("maestro-wf1-n2/maestro-wf1-n2.seen"));
    let seen = seen.unwrap();
    assert!(seen.contains("maestro-wf1-n1-c2.txt") && !seen.contains("-c1"), "{seen}");

    // The pick went into the record, which now prefers the picked model for this kind of work.
    let outcomes: std::collections::BTreeMap<String, Outcome> =
        serde_json::from_slice(&std::fs::read(home.path().join("outcomes.json")).unwrap()).unwrap();
    let chosen = |agent| outcomes[&format!("wf1/n1/{agent}")].chosen;
    assert_eq!((chosen(picked), chosen(lost)), (Some(true), Some(false)));
    assert_eq!(outcomes[&format!("wf1/n1/{picked}")].tags, ["ui"]);
    client.request(Command::RateNode { workflow, node: hard, rating: 5 }).unwrap();
    let ask = Command::ChooseModel {
        difficulty: Difficulty::Hard,
        tags: vec!["ui".into()],
        host: Host::Local,
        project: None,
    };
    let Reply::ModelChosen { choice: Some(choice) } = client.request(ask).unwrap() else {
        panic!("expected a model");
    };
    assert_eq!(choice.model.model, "second");
}
