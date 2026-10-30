//! The settings a frontend changes on the daemon: finding the agent CLIs, updating one with its
//! documented script while every frontend follows the output, and switching a provider off so
//! the router stops choosing it. A shell script stands in for `claude`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use maestro_client::{Client, Launch, Mirror};
use maestro_core::{Difficulty, Host, ProviderKind, Router, SetupAction};
use maestro_daemon::Hub;
use maestro_protocol::{Command, Event, Reply, SetupProgress};

/// Answers `--version`, and `update` the way `claude update` does.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "9.9.9 (Claude Code)" ;;
  update) echo "Checking for updates"; echo "warning on stderr" >&2; echo "Updated" ;;
  *) exit 2 ;;
esac
"#;

fn next(events: &Receiver<Event>) -> Event {
    events.recv_timeout(Duration::from_secs(20)).expect("an event")
}

#[test]
fn providers_are_found_updated_and_switched_off() {
    let bin = tempfile::tempdir().unwrap();
    let claude = bin.path().join("claude");
    std::fs::write(&claude, FAKE_CLAUDE).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());
    // SAFETY: this test binary runs this one test, and sets PATH before starting any thread.
    unsafe { std::env::set_var("PATH", path) };

    let home = tempfile::tempdir().unwrap();
    let client = Client::connect(&Launch::InProcess(Hub::open(home.path()).unwrap())).unwrap();
    let events = client.take_events().unwrap();
    let mut mirror = Mirror::load(&client).unwrap();

    let Reply::Providers { providers } =
        client.request(Command::ListProviders { host: Host::Local }).unwrap()
    else {
        panic!("expected the providers");
    };
    assert_eq!(providers.len(), ProviderKind::ALL.len());
    let found = &providers[0];
    assert_eq!(found.provider, ProviderKind::ClaudeCode);
    assert_eq!(found.path.as_deref(), Some(claude.to_str().unwrap()));
    assert_eq!(found.version.as_deref(), Some("9.9.9 (Claude Code)"));
    mirror.set_providers(&Host::Local, providers);

    // The update answers at once; its output follows as events, both streams in order.
    let update = Command::SetupProvider {
        host: Host::Local,
        provider: ProviderKind::ClaudeCode,
        action: SetupAction::Update,
    };
    assert_eq!(client.request(update).unwrap(), Reply::Done);
    loop {
        let event = next(&events);
        mirror.apply(&event);
        if let Event::ProviderSetup { progress: SetupProgress::Finished { ok, status }, .. } = event
        {
            assert!(ok);
            assert_eq!(status.version.as_deref(), Some("9.9.9 (Claude Code)"));
            assert_eq!(status.setup, None);
            break;
        }
    }
    let setup = mirror.setup(&Host::Local, ProviderKind::ClaudeCode).unwrap();
    assert_eq!((setup.action, setup.script.as_str()), (SetupAction::Update, "claude update"));
    assert_eq!(setup.lines, ["Checking for updates", "warning on stderr", "Updated"]);
    assert_eq!(setup.finished, Some(true));

    // Switched off, it is saved for the next daemon and no longer chosen.
    let mut router: Router = mirror.router.clone();
    router.disabled.insert(ProviderKind::ClaudeCode);
    client.request(Command::SetRouter { router: router.clone() }).unwrap();
    let event = next(&events);
    mirror.apply(&event);
    assert_eq!(mirror.router, router);
    let saved: Router =
        serde_json::from_slice(&std::fs::read(home.path().join("router.json")).unwrap()).unwrap();
    assert_eq!(saved.disabled, router.disabled);

    let choose = Command::ChooseModel {
        difficulty: Difficulty::Moderate,
        tags: Vec::new(),
        host: Host::Local,
    };
    let Reply::ModelChosen { choice } = client.request(choose).unwrap() else {
        panic!("expected a choice");
    };
    assert!(choice.is_none_or(|choice| choice.model.provider != ProviderKind::ClaudeCode));
}
