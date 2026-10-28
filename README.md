# Maestro

A desktop application for orchestrating coding-agent CLIs (Claude Code, Codex, Antigravity CLI,
OpenCode) from one gpui interface, with a ratatui terminal frontend to follow. A commander agent
talks with you and plans the work as a graph; worker agents carry out each node in their own git
worktree, on this machine or on a server reached over SSH.

The plan is in [docs/plan.md](docs/plan.md) (Japanese).

## Layout

```
crates/maestro-core      domain model: providers, agents, workflow graphs, hosts, agent events — no I/O
crates/maestro-protocol  commands, replies and events, framed as JSON lines
crates/maestro-host      processes on a host (local or over `ssh`), git worktrees, `gh` pull requests
crates/maestro-agents    adapters for Claude Code, Codex, Antigravity CLI and OpenCode, and the
                         session that runs one as a worker
crates/maestro-daemon    owns all state; `serve` is the resident daemon, `--stdio` relays to it
crates/maestro-client    connects a frontend in-process, to a local child, or over `ssh`, and
                         keeps the frontend's copy of the daemon's state
crates/maestro-gui       the desktop app (`maestro`), built with gpui and Atelier
```

## The desktop app

```sh
cargo build --workspace
target/debug/maestro --repo path/to/repository            # daemon on this machine
target/debug/maestro --ssh user@server --repo /srv/repo   # daemon, repository and agents on a server
```

Ask the commander what to build in the panel on the right. Its plan appears as a graph of steps;
edit them (title, instructions, difficulty, provider, dependencies), then press Run. Select a
step to see its worker, answer its permission requests, or ask it for changes. When every step
has succeeded, "Open pull request" merges them and opens one pull request.

`--appearance light` and `--direction studio|mono` change the theme. On Linux, building needs
the X11, Wayland and Vulkan headers (`libxkbcommon-x11-dev libwayland-dev libvulkan-dev
libxcb1-dev libx11-xcb-dev` on Debian and Ubuntu), and the window needs a window manager or
compositor to draw.

## Running the daemon on a server

Install `maestro-daemon` on the server, on its `PATH`, along with the agent CLIs (signed in) and
`gh` (signed in) if pull requests are wanted. A frontend then connects with
`Launch::ssh("user@server")`: it runs `ssh user@server maestro-daemon --stdio`, which starts the
resident daemon the first time and relays to it after that. The daemon keeps running, and its
agents keep working, when the frontend disconnects. Its socket and pid file are in
`$XDG_RUNTIME_DIR/maestro/` (or `~/.maestro/`). After an upgrade, the first `--stdio` relay that
finds the old daemon still running, speaking an older protocol, stops it and starts its own.

The daemon keeps its model catalog and the record of past work in `$MAESTRO_HOME` (default
`~/.maestro/`). `router.json` lists the models the commander may choose from, with any benchmark
scores you want it to weigh; it is written as a template the first time and is yours to edit.
`outcomes.json` records how each node went (success, time, revisions, your rating) and is what
the router learns from.

To check an adapter against a CLI installed on a machine:

```sh
cargo run -p maestro-agents --example try_worker -- claude "Say hello in one word"
```

## Build

```sh
cargo test --workspace
```

Licensed under [Apache-2.0](LICENSE).
