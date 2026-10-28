//! Runs one turn of a real CLI through its adapter and prints the events, to check an adapter
//! against the CLI installed on this machine:
//!
//! ```sh
//! cargo run -p maestro-agents --example try_worker -- claude "Say hello in one word"
//! ```
//!
//! The first argument is `claude`, `codex`, `antigravity` or `opencode`. Permission requests are
//! refused, so the turn cannot change anything.

use std::sync::mpsc;
use std::time::Duration;

use maestro_agents::{AgentSession, WorkerConfig, adapter};
use maestro_core::{AgentEvent, Host, ProviderKind};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [provider, prompt] = &args[..] else {
        eprintln!("usage: try_worker (claude|codex|antigravity|opencode) PROMPT");
        std::process::exit(2);
    };
    let provider = match provider.as_str() {
        "claude" => ProviderKind::ClaudeCode,
        "codex" => ProviderKind::Codex,
        "antigravity" => ProviderKind::Antigravity,
        "opencode" => ProviderKind::OpenCode,
        other => panic!("unknown provider {other}"),
    };
    let config = WorkerConfig {
        provider,
        model: None,
        effort: None,
        cwd: std::env::current_dir().unwrap(),
        instructions: "Answer briefly.".into(),
        program: None,
        read_only: true,
    };
    let (events, received) = mpsc::channel();
    let session = AgentSession::spawn(&Host::Local, adapter(config), prompt, move |event| {
        events.send(event).ok();
    })
    .unwrap();
    while let Ok(event) = received.recv_timeout(Duration::from_secs(120)) {
        println!("{event:?}");
        match event {
            AgentEvent::PermissionRequested { request } => session.answer(request.id, false),
            AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. } => session.stop(),
            AgentEvent::Exited { .. } => return,
            _ => {}
        }
    }
    eprintln!("no event for two minutes");
}
