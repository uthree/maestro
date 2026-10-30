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
cargo run                                             # daemon on this machine, this repository
cargo run -- --repo path/to/repository                # another repository on this machine
cargo run -- --ssh user@server --repo /srv/repo       # daemon, repository and agents on a server
```

`cargo run` at the workspace root builds and opens the desktop app. Without `--repo` it opens
the git checkout you started it in (only on this machine; with `--ssh`, give `--repo`).
On this machine the daemon is part of the `maestro` executable and starts by itself, so there is
nothing else to build, start or stop. With `--ssh`, the server needs `maestro-daemon` installed
(see below).

Ask the commander what to build in the conversation in the middle of the window. Its plan
appears in the conversation as a card listing the steps, with Run beside it. "Open workflow"
opens the plan as a graph beside the conversation (or in its place, on a narrow window or with
the expand button); there you can edit the steps (title, instructions, difficulty, provider,
dependencies). Select a step to see its worker, answer its permission requests, or ask it for
changes. When every step
has succeeded, "Open pull request" merges them and opens one pull request.

The gear in the title bar opens Settings:

* **General**: the display language (System, English, 日本語) and the theme. These are saved on
  this computer in `~/.maestro/preferences.json` and apply at once. `--lang`, `--appearance` and
  `--direction` override them for one run.
* **Agent providers**: which of Claude Code, Codex, Antigravity CLI and OpenCode the daemon's
  machine has, and which release. Install or update one with its project's own script (shown
  before it runs), and follow the output as it goes. A switch takes a provider out of routing
  without uninstalling it. Each CLI signs in to your account in a terminal, so Settings shows the
  command to run instead.
* **Work allocation**: when a step runs on several models to compare them, and the worker limits
  that stand for compute and usage. These are saved to the daemon's `router.json`.

`--appearance light` and `--direction studio|mono` change the theme. On Linux, building needs
the X11, Wayland and Vulkan headers (`libxkbcommon-x11-dev libwayland-dev libvulkan-dev
libxcb1-dev libx11-xcb-dev` on Debian and Ubuntu), and the window needs a window manager or
compositor to draw.

## Running the daemon on a server

Install `maestro-daemon` on the server, on its `PATH`, along with the agent CLIs (signed in) and
`gh` (signed in) if pull requests are wanted. A frontend then connects with
`Launch::ssh("user@server")`: it runs `ssh user@server maestro-daemon --stdio`, which starts the
resident daemon the first time and relays to it after that. The daemon keeps running, and its
agents keep working, when the frontend disconnects. Once no frontend has been connected and no
agent has been in the middle of a turn for ten minutes (`serve --idle SECONDS` changes this, `0`
keeps it running), it saves its state, stops its agents and exits; the next frontend starts it
again with every workflow and transcript, and a message to a finished step starts a new worker in
that step's worktree. Its socket and pid file are in
`$XDG_RUNTIME_DIR/maestro/` (or `~/.maestro/`). After an upgrade, the first `--stdio` relay that
finds the old daemon still running, speaking an older protocol, stops it and starts its own.

The daemon keeps its model catalog and the record of past work in `$MAESTRO_HOME` (default
`~/.maestro/`). `router.json` lists the models the commander may choose from, with any benchmark
scores you want it to weigh; it is written as a template the first time and is yours to edit.
`outcomes.json` records how each node went (success, time, revisions, your rating) and is what
the router learns from. `state.json` holds the workflows and transcripts, saved as they change.

To check an adapter against a CLI installed on a machine:

```sh
cargo run -p maestro-agents --example try_worker -- claude "Say hello in one word"
```

## Build

```sh
cargo test --workspace
```

Licensed under [Apache-2.0](LICENSE).
