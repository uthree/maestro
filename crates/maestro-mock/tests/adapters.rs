//! Each of Maestro's four adapters, driven through a real `AgentSession` against `maestro-mock`
//! playing that CLI: the same scenario must read the same way whichever CLI plays it.

use std::sync::mpsc;
use std::time::Duration;

use maestro_agents::{AgentSession, WorkerConfig, adapter};
use maestro_core::{AgentEvent, Effort, Host, PermissionKind, ProviderKind};
use maestro_mock::{SCENARIO_FILE, Scenario};

const ALL: [ProviderKind; 4] = ProviderKind::ALL;

/// A worker on `provider` with the scenario in its working directory.
struct Worker {
    config: WorkerConfig,
    _directory: tempfile::TempDir,
}

fn worker(provider: ProviderKind, scenario: &str) -> Worker {
    let scenario = Scenario::from_json(scenario).expect("a valid scenario");
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(SCENARIO_FILE), scenario.to_json()).unwrap();
    let config = WorkerConfig {
        provider,
        model: None,
        effort: Some(Effort::Medium),
        cwd: directory.path().into(),
        instructions: String::new(),
        program: Some(env!("CARGO_BIN_EXE_maestro-mock").into()),
        read_only: false,
    };
    Worker { config, _directory: directory }
}

/// What happened in the first turn of `worker` given `prompt`. Permission requests are answered
/// from `answers` in order; `act` may act on the session at each event.
fn turn(
    worker: &Worker,
    prompt: &str,
    answers: &[bool],
    act: impl Fn(&AgentSession, &AgentEvent),
) -> Vec<AgentEvent> {
    let (sender, events) = mpsc::channel();
    let session =
        AgentSession::spawn(&Host::Local, adapter(worker.config.clone()), prompt, move |event| {
            sender.send(event).ok();
        })
        .unwrap();
    let mut answers = answers.iter();
    let mut seen = Vec::new();
    loop {
        let event = events.recv_timeout(Duration::from_secs(10)).expect("an event in time");
        if let AgentEvent::PermissionRequested { request } = &event {
            session.answer(request.id.clone(), *answers.next().expect("an answer to give"));
        }
        act(&session, &event);
        let end = matches!(
            event,
            AgentEvent::TurnCompleted { .. }
                | AgentEvent::TurnFailed { .. }
                | AgentEvent::Exited { .. }
        );
        seen.push(event);
        if end {
            return seen;
        }
    }
}

fn text(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn count(events: &[AgentEvent], wanted: impl Fn(&AgentEvent) -> bool) -> usize {
    events.iter().filter(|event| wanted(event)).count()
}

const FULL_TURN: &str = r#"{
    "stream": {"chunk": 3},
    "rules": [{"when": {"prompt_contains": "build"}, "reply": [
        {"noise": "Welcome to the CLI"},
        {"think": "Build first."},
        {"say": "Looking."},
        {"tool": {"name": "Bash", "input": {"command": "cargo build"}, "output": "Finished", "ask": true}},
        {"tool": {"name": "Edit", "input": {"file_path": "src/lib.rs"}, "ask": true}},
        {"say": "Done."}
    ]}]
}"#;

#[test]
fn a_turn_streams_reasons_calls_tools_and_asks_permission_on_every_cli() {
    for provider in ALL {
        let worker = worker(provider, FULL_TURN);
        let events = turn(&worker, "Please build it", &[true, false], |_, _| {});
        let label = provider.label();
        assert_eq!(events[0], AgentEvent::Started, "{label}: {events:#?}");
        let Some(AgentEvent::TurnCompleted { answer }) = events.last() else {
            panic!("{label} did not complete: {events:#?}")
        };
        // How two messages of one turn are joined is up to each adapter.
        let joined = match provider {
            ProviderKind::OpenCode => "Looking.Done.",
            _ => "Looking.\n\nDone.",
        };
        assert_eq!(answer, joined, "{label}");
        if provider != ProviderKind::Antigravity {
            assert_eq!(&text(&events), answer, "{label}: the streamed text is the answer");
        }

        let asked: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::PermissionRequested { request } => Some(request.kind),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 2, "{label}");
        if provider != ProviderKind::Antigravity {
            assert_eq!(asked, [PermissionKind::Command, PermissionKind::FileChange], "{label}");
        }
        let started = count(&events, |event| matches!(event, AgentEvent::ToolStarted { .. }));
        assert_eq!(started, 2, "{label}");
        let finished: Vec<bool> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolFinished { ok, .. } => Some(*ok),
                _ => None,
            })
            .collect();
        match provider {
            // `agy` never says how a step ended.
            ProviderKind::Antigravity => assert_eq!(finished, [true, true], "{label}"),
            _ => assert_eq!(finished, [true, false], "{label}: the refused call failed"),
        }
        let reasoned = count(&events, |event| matches!(event, AgentEvent::Reasoning { .. }));
        assert_eq!(reasoned > 0, provider != ProviderKind::Antigravity, "{label}");
    }
}

#[test]
fn a_failing_step_fails_the_turn_on_every_cli() {
    let scenario = r#"{"rules": [{"reply": [{"say": "Trying."}, {"fail": "rate limited"}]}]}"#;
    for provider in ALL {
        let events = turn(&worker(provider, scenario), "go", &[], |_, _| {});
        let Some(AgentEvent::TurnFailed { message }) = events.last() else {
            panic!("{}: {events:#?}", provider.label())
        };
        assert!(message.contains("rate limited"), "{}: {message}", provider.label());
    }
}

#[test]
fn an_exiting_cli_ends_the_session_with_its_status() {
    let scenario = r#"{"rules": [{"reply": [{"say": "Bye"}, {"exit": 3}]}]}"#;
    for provider in ALL {
        let events = turn(&worker(provider, scenario), "go", &[], |_, _| {});
        let Some(AgentEvent::Exited { reason: Some(reason) }) = events.last() else {
            panic!("{}: {events:#?}", provider.label())
        };
        assert!(reason.contains('3'), "{}: {reason}", provider.label());
    }
}

#[test]
fn an_interrupt_ends_a_hanging_turn() {
    let scenario = r#"{"rules": [{"reply": [{"say": "Working"}, "hang"]}]}"#;
    // `agy` has no interrupt in its protocol; Maestro kills it instead.
    for provider in [ProviderKind::ClaudeCode, ProviderKind::Codex, ProviderKind::OpenCode] {
        let events = turn(&worker(provider, scenario), "go", &[], |session, event| {
            if matches!(event, AgentEvent::Text { .. }) {
                session.interrupt();
            }
        });
        assert!(
            matches!(events.last(), Some(AgentEvent::TurnFailed { .. })),
            "{}: {events:#?}",
            provider.label()
        );
    }
}

#[test]
fn rules_see_the_turn_whether_the_cli_is_read_only_and_the_model() {
    let scenario = r#"{"rules": [
        {"when": {"read_only": true, "model": "m-1"}, "reply": [{"say": "planner on m-1"}]},
        {"when": {"turn": 1}, "reply": [{"say": "worker"}]}
    ]}"#;
    for provider in [ProviderKind::ClaudeCode, ProviderKind::Codex, ProviderKind::OpenCode] {
        let mut planner = worker(provider, scenario);
        planner.config.read_only = true;
        planner.config.model = Some("m-1".into());
        let events = turn(&planner, "plan", &[], |_, _| {});
        let label = provider.label();
        assert_eq!(
            events.last(),
            Some(&AgentEvent::TurnCompleted { answer: "planner on m-1".into() }),
            "{label}"
        );
        let events = turn(&worker(provider, scenario), "work", &[], |_, _| {});
        assert_eq!(
            events.last(),
            Some(&AgentEvent::TurnCompleted { answer: "worker".into() }),
            "{label}"
        );
    }
}

#[test]
fn later_turns_follow_the_conversation() {
    let scenario = r#"{"otherwise": [{"say": "turn {turn}: {prompt}"}]}"#;
    for provider in ALL {
        let worker = worker(provider, scenario);
        let (sender, events) = mpsc::channel();
        let session = AgentSession::spawn(
            &Host::Local,
            adapter(worker.config.clone()),
            "one",
            move |event| {
                sender.send(event).ok();
            },
        )
        .unwrap();
        let mut answers = Vec::new();
        while answers.len() < 2 {
            match events.recv_timeout(Duration::from_secs(10)).expect("an event in time") {
                AgentEvent::TurnCompleted { answer } => {
                    answers.push(answer);
                    session.send("two");
                }
                AgentEvent::TurnFailed { message } => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(answers, ["turn 1: one", "turn 2: two"], "{}", provider.label());
    }
}
