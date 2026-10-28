# Maestro — working notes

The plan, and why the boundaries are where they are, is [docs/plan.md](docs/plan.md).

## Rules that apply to every change

* **No frontend logic outside a frontend crate.** Anything the GUI can do, the TUI must be able to
  do through the same `maestro_protocol::Command`. If a feature needs a new capability, add a
  command; never reach into daemon state from a UI.
* **The daemon owns the state; frontends hold copies.** Edits go through `Client::request`, and
  views redraw from `Event`s, which every connected frontend receives.
* **Assume the daemon is on another machine.** Requests are a network round trip; never make one
  on a UI thread. `Host::Local` means the daemon's machine, not the user's.
* **Workflows stay acyclic.** Every edit goes through `Workflow` methods, which refuse cycles and
  leave the graph unchanged on failure.
* Lints: `missing_docs` is on for every crate. Run `cargo fmt --all`, `cargo clippy --workspace
  --all-targets` and `cargo test --workspace` before pushing.
