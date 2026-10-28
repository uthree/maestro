# Maestro

A desktop application for orchestrating coding-agent CLIs (Claude Code, Codex, Antigravity CLI,
OpenCode) from one gpui interface, with a ratatui terminal frontend to follow. A commander agent
talks with you and plans the work as a graph; worker agents carry out each node in their own git
worktree, on this machine or on a server reached over SSH.

The plan is in [docs/plan.md](docs/plan.md) (Japanese).

## Layout

```
crates/maestro-core      domain model: providers, agents, workflow graphs, hosts — no I/O
crates/maestro-protocol  commands, replies and events, framed as JSON lines
crates/maestro-daemon    owns all state; `maestro-daemon --stdio` serves one frontend
crates/maestro-client    connects a frontend in-process, to a local child, or over `ssh`
```

## Build

```sh
cargo test --workspace
```

Licensed under [Apache-2.0](LICENSE).
