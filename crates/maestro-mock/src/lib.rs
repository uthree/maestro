//! Scripted stand-ins for the AI providers Maestro drives, for tests that need no account.
//!
//! Maestro never calls a model's HTTP API: it drives each provider's CLI over stdio, and the
//! adapters in `maestro-agents` read that CLI's JSON. So the main stand-in is a fake CLI.
//! [`cli::run`] plays any of the four (Claude Code, Codex, Antigravity CLI, OpenCode) in its own
//! wire format, and the `maestro-mock` executable runs it as a process, so a test exercises the
//! real adapter, [`AgentSession`]'s pipes, and the daemon above them.
//!
//! What it answers comes from a [`Scenario`]: rules matched against each prompt, whose replies
//! stream text and reasoning, call tools, ask permission, pause, fail, crash, or print noise.
//! The same scenario drives [`server::Server`], an OpenAI-compatible HTTP server, for pointing a
//! real CLI at when the question is whether the CLI itself still writes what the adapters read.
//!
//! The executable:
//!
//! ```sh
//! maestro-mock install DIR        # links claude, codex, agy and opencode in DIR to itself
//! PATH=DIR:$PATH MAESTRO_MOCK_SCENARIO=scenario.json cargo run   # the app, on fake providers
//! maestro-mock serve [--listen 127.0.0.1:8080]                   # the OpenAI-compatible server
//! ```
//!
//! Run under any other name, it is the CLI its name or arguments say it is (see
//! [`cli::Cli::detect`]). The scenario comes from [`scenario::SCENARIO_VAR`], or else from
//! [`scenario::SCENARIO_FILE`] in the working directory.
//!
//! [`AgentSession`]: https://docs.rs/maestro-agents

pub mod cli;
pub mod scenario;
pub mod server;

pub use cli::Cli;
pub use scenario::{Rule, SCENARIO_FILE, SCENARIO_VAR, Scenario, Step, Stream, Tool, When};
pub use server::Server;
