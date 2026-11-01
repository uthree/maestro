//! Stand-ins for the four coding-agent CLIs, each speaking its CLI's stdio protocol the way
//! `maestro-agents` reads it.
//!
//! [`run`] is one CLI process: it reads the client's JSON lines, plays the scenario's reply to
//! each prompt, and writes the CLI's messages. A [`Dialect`] knows one CLI's message shapes; the
//! turn itself (streaming, pauses, permission waits, interrupts) is played the same for all four.
//! While a turn waits, the client's messages keep being read, so an interrupt or a permission
//! answer arrives the way it would from a real CLI.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::scenario::{Player, Prompt, Step, Tool};

/// Which CLI to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cli {
    /// `claude --print --input-format stream-json ...`
    Claude,
    /// `codex app-server`
    Codex,
    /// `agy --input-format stream-json ...`
    Antigravity,
    /// `opencode acp`
    OpenCode,
}

impl Cli {
    /// Every CLI.
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Antigravity, Self::OpenCode];

    /// The executable's name, which is also the provider's name in a scenario.
    pub fn executable(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
            Self::OpenCode => "opencode",
        }
    }

    /// The CLI an executable's name or its arguments say it is: the name when it is one of
    /// the four (as when installed under that name), else the arguments Maestro starts each
    /// with (as when a worker's `program` points at `maestro-mock` itself).
    pub fn detect(program: &str, args: &[String]) -> Option<Self> {
        let name = std::path::Path::new(program).file_stem()?.to_string_lossy();
        if let Some(cli) = Self::ALL.into_iter().find(|cli| cli.executable() == name) {
            return Some(cli);
        }
        let has = |flag: &str| args.iter().any(|arg| arg == flag);
        match args.first().map(String::as_str) {
            Some("app-server") => Some(Self::Codex),
            Some("acp") => Some(Self::OpenCode),
            _ if has("--print") => Some(Self::Claude),
            _ if has("--input-format") => Some(Self::Antigravity),
            _ => None,
        }
    }
}

/// How the process was started.
#[derive(Clone, Debug, Default)]
pub struct Invocation {
    /// Its arguments, without the program.
    pub args: Vec<String>,
    /// `OPENCODE_PERMISSION`, which tells OpenCode what it may do.
    pub opencode_permission: Option<String>,
}

impl Invocation {
    /// This process's own.
    pub fn current() -> Self {
        Self {
            args: std::env::args().skip(1).collect(),
            opencode_permission: std::env::var("OPENCODE_PERMISSION").ok(),
        }
    }

    /// The value of `--name=value` or `--name value`.
    fn flag(&self, name: &str) -> Option<String> {
        let mut args = self.args.iter();
        while let Some(arg) = args.next() {
            if arg == name {
                return args.next().cloned();
            }
            if let Some(value) = arg.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')) {
                return Some(value.to_owned());
            }
        }
        None
    }
}

/// Plays `player`'s scenario as `cli`, reading the client from `input` and writing to `output`,
/// until input closes or a step exits. Returns the exit code.
pub fn run(
    cli: Cli,
    invocation: &Invocation,
    player: Player,
    input: impl BufRead + Send + 'static,
    output: impl Write,
) -> i32 {
    let (lines, inbox) = mpsc::channel();
    thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { break };
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && lines.send(value).is_err()
            {
                break;
            }
        }
    });
    let dialect: Box<dyn Dialect> = match cli {
        Cli::Claude => Box::new(Claude::new(invocation)),
        Cli::Codex => Box::new(Codex::default()),
        Cli::Antigravity => Box::new(Antigravity::default()),
        Cli::OpenCode => Box::new(OpenCode::new(invocation)),
    };
    let mut driver = Driver {
        cli,
        dialect,
        player,
        inbox,
        wire: Wire(Box::new(output)),
        prompts: VecDeque::new(),
        turn: 0,
    };
    driver.serve()
}

/// The client's side of the pipe, one JSON value per line.
struct Wire<'a>(Box<dyn Write + 'a>);

impl Wire<'_> {
    fn send(&mut self, message: Value) {
        self.line(&message.to_string());
    }

    fn line(&mut self, line: &str) {
        // The client hanging up shows as input closing; nothing to do here.
        writeln!(self.0, "{line}").and_then(|()| self.0.flush()).ok();
    }
}

/// What a client's message means to the turn being played.
enum Incoming {
    /// A user message: its text.
    Prompt(String),
    /// A permission answer, by the key [`Dialect::ask`] returned.
    Answer { key: String, allow: bool },
    /// Stop the current turn.
    Interrupt,
    /// Handled by the dialect, or ignored.
    Nothing,
}

/// One CLI's message shapes.
trait Dialect {
    /// Reads one client message, answering protocol requests itself.
    fn read(&mut self, message: Value, wire: &mut Wire) -> Incoming;
    /// Whether the CLI was told to change nothing, and the model it was given, as the
    /// client has said so far.
    fn context(&self) -> (bool, Option<String>);
    /// A turn begins.
    fn begin(&mut self, _wire: &mut Wire) {}
    /// A chunk of text; `first` when it starts a message.
    fn text(&mut self, wire: &mut Wire, chunk: &str, first: bool);
    /// A message's text is complete.
    fn said(&mut self, _wire: &mut Wire, _text: &str) {}
    /// A chunk of reasoning.
    fn think(&mut self, wire: &mut Wire, chunk: &str);
    /// A tool call starts.
    fn tool(&mut self, wire: &mut Wire, id: &str, tool: &Tool);
    /// Asks permission for the call; returns the key its answer will carry.
    fn ask(&mut self, wire: &mut Wire, id: &str, tool: &Tool) -> String;
    /// The call is over.
    fn done(&mut self, wire: &mut Wire, id: &str, tool: &Tool, ok: bool, output: &str);
    /// The turn ended well; `answer` is all its text.
    fn finish(&mut self, wire: &mut Wire, answer: &str);
    /// The turn failed.
    fn fail(&mut self, wire: &mut Wire, message: &str);
    /// The turn was interrupted.
    fn interrupted(&mut self, wire: &mut Wire);
}

/// Why a turn stopped early.
enum Stop {
    Interrupted,
    Closed,
    Exit(i32),
}

struct Driver<'a> {
    cli: Cli,
    dialect: Box<dyn Dialect>,
    player: Player,
    inbox: Receiver<Value>,
    wire: Wire<'a>,
    /// Prompts that arrived during a turn, played after it.
    prompts: VecDeque<String>,
    turn: u32,
}

impl Driver<'_> {
    fn serve(&mut self) -> i32 {
        loop {
            let prompt = match self.prompts.pop_front() {
                Some(prompt) => prompt,
                None => match self.inbox.recv() {
                    Ok(message) => match self.dialect.read(message, &mut self.wire) {
                        Incoming::Prompt(prompt) => prompt,
                        _ => continue,
                    },
                    Err(_) => return 0,
                },
            };
            match self.play(&prompt) {
                Ok(()) => {}
                Err(Stop::Interrupted) => self.dialect.interrupted(&mut self.wire),
                Err(Stop::Closed) => return 0,
                Err(Stop::Exit(code)) => return code,
            }
        }
    }

    fn play(&mut self, text: &str) -> Result<(), Stop> {
        self.turn += 1;
        let (read_only, model) = self.dialect.context();
        let steps = self.player.reply(&Prompt {
            provider: self.cli.executable(),
            text,
            turn: self.turn,
            read_only,
            model: model.as_deref(),
        });
        let stream = self.player.stream();
        self.dialect.begin(&mut self.wire);
        let mut answer = String::new();
        let mut tools = 0;
        for step in steps {
            match step {
                Step::Say(text) => {
                    for (index, chunk) in stream.chunks(&text).iter().enumerate() {
                        if index > 0 {
                            self.wait(stream.delay())?;
                        }
                        self.dialect.text(&mut self.wire, chunk, index == 0);
                    }
                    self.dialect.said(&mut self.wire, &text);
                    if !answer.is_empty() {
                        answer.push_str("\n\n");
                    }
                    answer.push_str(&text);
                }
                Step::Think(text) => {
                    for (index, chunk) in stream.chunks(&text).iter().enumerate() {
                        if index > 0 {
                            self.wait(stream.delay())?;
                        }
                        self.dialect.think(&mut self.wire, chunk);
                    }
                }
                Step::Tool(tool) => {
                    tools += 1;
                    let id = format!("call_{}_{tools}", self.turn);
                    self.dialect.tool(&mut self.wire, &id, &tool);
                    let allowed = if tool.ask {
                        let key = self.dialect.ask(&mut self.wire, &id, &tool);
                        self.permission(&key)?
                    } else {
                        true
                    };
                    let (ok, output) = if allowed {
                        (tool.ok, tool.output.as_str())
                    } else {
                        (false, "The user refused this action.")
                    };
                    self.dialect.done(&mut self.wire, &id, &tool, ok, output);
                }
                Step::SleepMs(millis) => self.wait(Duration::from_millis(millis))?,
                Step::Hang => self.wait(Duration::MAX)?,
                Step::Fail(message) | Step::HttpError { message, .. } => {
                    self.dialect.fail(&mut self.wire, &message);
                    return Ok(());
                }
                Step::Exit(code) => return Err(Stop::Exit(code)),
                Step::Noise(line) => self.wire.line(&line),
            }
        }
        self.dialect.finish(&mut self.wire, &answer);
        Ok(())
    }

    /// Handles one message that arrived mid-turn; `Some` when it answers `key`.
    fn handle(&mut self, message: Value, key: Option<&str>) -> Result<Option<bool>, Stop> {
        match self.dialect.read(message, &mut self.wire) {
            Incoming::Prompt(prompt) => self.prompts.push_back(prompt),
            Incoming::Interrupt => return Err(Stop::Interrupted),
            Incoming::Answer { key: answered, allow } if Some(answered.as_str()) == key => {
                return Ok(Some(allow));
            }
            Incoming::Answer { .. } | Incoming::Nothing => {}
        }
        Ok(None)
    }

    /// Waits `duration`, reading the client meanwhile.
    fn wait(&mut self, duration: Duration) -> Result<(), Stop> {
        let deadline = Instant::now().checked_add(duration);
        loop {
            let message = match deadline {
                Some(deadline) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(());
                    }
                    self.inbox.recv_timeout(left)
                }
                None => self.inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match message {
                Ok(message) => {
                    self.handle(message, None)?;
                }
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => return Err(Stop::Closed),
            }
        }
    }

    /// Waits for the answer carrying `key`.
    fn permission(&mut self, key: &str) -> Result<bool, Stop> {
        loop {
            let message = self.inbox.recv().map_err(|_| Stop::Closed)?;
            if let Some(allow) = self.handle(message, Some(key))? {
                return Ok(allow);
            }
        }
    }
}

/// The text of `content`: a string, or a list of parts with `text`.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

/// Whether a tool call is a shell command, and the command.
fn command_of(tool: &Tool) -> Option<&str> {
    tool.input["command"].as_str()
}

/// Whether a tool call changes a file.
fn edits(tool: &Tool) -> bool {
    tool.input.get("file_path").is_some()
        || matches!(tool.name.as_str(), "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
}

/// Claude Code's `stream-json`, with partial messages and stdio permission prompts.
struct Claude {
    read_only: bool,
    model: Option<String>,
    started: bool,
    asked: u64,
}

impl Claude {
    fn new(invocation: &Invocation) -> Self {
        Self {
            read_only: invocation.args.iter().any(|arg| arg == "--disallowedTools"),
            model: invocation.flag("--model"),
            started: false,
            asked: 0,
        }
    }

    fn result(wire: &mut Wire, ok: bool, result: &str) {
        let subtype = if ok { "success" } else { "error_during_execution" };
        wire.send(json!({"type": "result", "subtype": subtype, "is_error": !ok, "result": result}));
    }
}

impl Dialect for Claude {
    fn read(&mut self, message: Value, wire: &mut Wire) -> Incoming {
        match message["type"].as_str() {
            Some("user") => Incoming::Prompt(text_of(&message["message"]["content"])),
            Some("control_request") => {
                let id = &message["request_id"];
                let ok = message["request"]["subtype"] == "interrupt";
                let response = if ok {
                    json!({"subtype": "success", "request_id": id})
                } else {
                    json!({"subtype": "error", "request_id": id, "error": "not supported"})
                };
                wire.send(json!({"type": "control_response", "response": response}));
                if ok { Incoming::Interrupt } else { Incoming::Nothing }
            }
            Some("control_response") => {
                let response = &message["response"];
                Incoming::Answer {
                    key: response["request_id"].as_str().unwrap_or_default().to_owned(),
                    allow: response["response"]["behavior"] == "allow",
                }
            }
            _ => Incoming::Nothing,
        }
    }

    fn context(&self) -> (bool, Option<String>) {
        (self.read_only, self.model.clone())
    }

    fn begin(&mut self, wire: &mut Wire) {
        if !std::mem::replace(&mut self.started, true) {
            wire.send(json!({"type": "system", "subtype": "init", "model": self.model}));
        }
    }

    fn text(&mut self, wire: &mut Wire, chunk: &str, _first: bool) {
        wire.send(json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"type": "text_delta", "text": chunk}}, "parent_tool_use_id": null}));
    }

    fn said(&mut self, wire: &mut Wire, text: &str) {
        wire.send(json!({"type": "assistant", "parent_tool_use_id": null,
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}));
    }

    fn think(&mut self, wire: &mut Wire, chunk: &str) {
        wire.send(json!({"type": "stream_event", "event": {"type": "content_block_delta",
            "delta": {"type": "thinking_delta", "thinking": chunk}}, "parent_tool_use_id": null}));
    }

    fn tool(&mut self, wire: &mut Wire, id: &str, tool: &Tool) {
        wire.send(json!({"type": "assistant", "parent_tool_use_id": null,
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": tool.name, "input": tool.input}]}}));
    }

    fn ask(&mut self, wire: &mut Wire, id: &str, tool: &Tool) -> String {
        self.asked += 1;
        let key = format!("perm_{}", self.asked);
        wire.send(json!({"type": "control_request", "request_id": key, "request": {
            "subtype": "can_use_tool", "tool_name": tool.name, "input": tool.input,
            "tool_use_id": id}}));
        key
    }

    fn done(&mut self, wire: &mut Wire, id: &str, _tool: &Tool, ok: bool, output: &str) {
        wire.send(json!({"type": "user", "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id,
                "content": [{"type": "text", "text": output}], "is_error": !ok}]}}));
    }

    fn finish(&mut self, wire: &mut Wire, answer: &str) {
        Self::result(wire, true, answer);
    }

    fn fail(&mut self, wire: &mut Wire, message: &str) {
        Self::result(wire, false, message);
    }

    fn interrupted(&mut self, wire: &mut Wire) {
        Self::result(wire, false, "Interrupted by the user");
    }
}

/// Codex's `app-server` JSON-RPC (no `jsonrpc` member).
#[derive(Default)]
struct Codex {
    read_only: bool,
    model: Option<String>,
    turns: u64,
    turn: String,
    /// The id of the agent message being streamed.
    message: Option<String>,
    items: u64,
    next_request: u64,
}

impl Codex {
    fn notify(&self, wire: &mut Wire, method: &str, mut params: Value) {
        params["threadId"] = json!("thr_mock");
        params["turnId"] = json!(self.turn);
        wire.send(json!({"method": method, "params": params}));
    }

    fn item(tool: &Tool, id: &str) -> Value {
        match command_of(tool) {
            Some(command) => json!({"type": "commandExecution", "id": id, "command": command,
                "status": "inProgress"}),
            None if edits(tool) => json!({"type": "fileChange", "id": id, "status": "inProgress",
                "changes": [{"path": tool.input["file_path"], "kind": "update"}]}),
            None => json!({"type": "mcpToolCall", "id": id, "server": "mock", "tool": tool.name,
                "arguments": tool.input, "status": "inProgress"}),
        }
    }

    fn completed(&mut self, wire: &mut Wire, status: &str, error: Option<&str>) {
        let error = error.map(|message| json!({"message": message}));
        let turn = json!({"id": self.turn, "status": status, "error": error, "items": []});
        self.notify(wire, "turn/completed", json!({"turn": turn}));
        self.message = None;
    }
}

impl Dialect for Codex {
    fn read(&mut self, message: Value, wire: &mut Wire) -> Incoming {
        let id = message["id"].clone();
        let params = &message["params"];
        let result = match message["method"].as_str() {
            // A response: the answer to an approval request.
            None => {
                return Incoming::Answer {
                    key: id.to_string(),
                    allow: matches!(
                        message["result"]["decision"].as_str(),
                        Some("accept" | "acceptForSession")
                    ),
                };
            }
            Some("initialize") => json!({"userAgent": "maestro-mock"}),
            Some("thread/start") => {
                self.read_only = params["sandbox"] == "read-only";
                self.model = params["model"].as_str().map(str::to_owned);
                json!({"thread": {"id": "thr_mock"}, "model": params["model"]})
            }
            Some("turn/start") => {
                self.turns += 1;
                self.turn = format!("turn_{}", self.turns);
                wire.send(json!({"id": id, "result": {"turn": {"id": self.turn,
                    "status": "inProgress", "items": []}}}));
                let text = params["input"].as_array().into_iter().flatten();
                let text = text.filter_map(|part| part["text"].as_str()).collect::<Vec<_>>();
                return Incoming::Prompt(text.join("\n"));
            }
            Some("turn/interrupt") => {
                wire.send(json!({"id": id, "result": {}}));
                return Incoming::Interrupt;
            }
            Some(_) if id.is_null() => return Incoming::Nothing,
            Some(method) => {
                wire.send(json!({"id": id, "error": {"code": -32601,
                    "message": format!("{method} is not mocked")}}));
                return Incoming::Nothing;
            }
        };
        wire.send(json!({"id": id, "result": result}));
        Incoming::Nothing
    }

    fn context(&self) -> (bool, Option<String>) {
        (self.read_only, self.model.clone())
    }

    fn begin(&mut self, wire: &mut Wire) {
        self.notify(
            wire,
            "turn/started",
            json!({"turn": {"id": self.turn, "status": "inProgress"}}),
        );
    }

    fn text(&mut self, wire: &mut Wire, chunk: &str, first: bool) {
        if first {
            self.items += 1;
            let id = format!("msg_{}", self.items);
            self.notify(
                wire,
                "item/started",
                json!({"item": {"type": "agentMessage", "id": id,
                "text": ""}}),
            );
            self.message = Some(id);
        }
        let item = self.message.clone();
        self.notify(wire, "item/agentMessage/delta", json!({"itemId": item, "delta": chunk}));
    }

    fn said(&mut self, wire: &mut Wire, text: &str) {
        let id = self.message.take();
        self.notify(
            wire,
            "item/completed",
            json!({"item": {"type": "agentMessage", "id": id,
            "text": text}}),
        );
    }

    fn think(&mut self, wire: &mut Wire, chunk: &str) {
        self.notify(
            wire,
            "item/reasoning/textDelta",
            json!({"itemId": "reasoning",
            "delta": chunk}),
        );
    }

    fn tool(&mut self, wire: &mut Wire, id: &str, tool: &Tool) {
        self.notify(wire, "item/started", json!({"item": Self::item(tool, id)}));
    }

    fn ask(&mut self, wire: &mut Wire, id: &str, tool: &Tool) -> String {
        self.next_request += 1;
        let request = json!(self.next_request);
        let (method, mut params) = match command_of(tool) {
            Some(command) => ("item/commandExecution/requestApproval", json!({"command": command})),
            None => ("item/fileChange/requestApproval", json!({})),
        };
        params["threadId"] = json!("thr_mock");
        params["turnId"] = json!(self.turn);
        params["itemId"] = json!(id);
        wire.send(json!({"id": request, "method": method, "params": params}));
        request.to_string()
    }

    fn done(&mut self, wire: &mut Wire, id: &str, tool: &Tool, ok: bool, output: &str) {
        let mut item = Self::item(tool, id);
        item["status"] = json!(if ok { "completed" } else { "failed" });
        match item["type"].as_str() {
            Some("commandExecution") => {
                item["exitCode"] = json!(if ok { 0 } else { 1 });
                item["aggregatedOutput"] = json!(output);
            }
            Some("mcpToolCall") => {
                item["result"] = json!({"content": [{"type": "text", "text": output}]});
                if !ok {
                    item["error"] = json!({"message": output});
                }
            }
            _ => {}
        }
        self.notify(wire, "item/completed", json!({"item": item}));
    }

    fn finish(&mut self, wire: &mut Wire, _answer: &str) {
        self.completed(wire, "completed", None);
    }

    fn fail(&mut self, wire: &mut Wire, message: &str) {
        self.completed(wire, "failed", Some(message));
    }

    fn interrupted(&mut self, wire: &mut Wire) {
        self.completed(wire, "interrupted", None);
    }
}

/// Antigravity's `stream-json`: `step_update`s and a `result`.
#[derive(Default)]
struct Antigravity {
    started: bool,
    asked: u64,
}

impl Dialect for Antigravity {
    fn read(&mut self, message: Value, _wire: &mut Wire) -> Incoming {
        match message["event"].as_str().or_else(|| message["type"].as_str()) {
            Some("user") => Incoming::Prompt(text_of(&message["message"]["content"])),
            Some("control_response") => {
                let response = &message["response"];
                Incoming::Answer {
                    key: response["request_id"].as_str().unwrap_or_default().to_owned(),
                    allow: response["response"]["behavior"] == "allow",
                }
            }
            _ => Incoming::Nothing,
        }
    }

    fn context(&self) -> (bool, Option<String>) {
        (false, None)
    }

    fn begin(&mut self, wire: &mut Wire) {
        if !std::mem::replace(&mut self.started, true) {
            wire.send(json!({"event": "init", "init": {"agent": "default"}}));
        }
    }

    fn text(&mut self, wire: &mut Wire, chunk: &str, _first: bool) {
        wire.send(json!({"event": "step_update", "step_update": {"step_type": "model",
            "text": chunk}}));
    }

    fn think(&mut self, _wire: &mut Wire, _chunk: &str) {}

    fn tool(&mut self, wire: &mut Wire, id: &str, tool: &Tool) {
        wire.send(json!({"event": "step_update", "step_update": {"step_type": "tool",
            "step_id": id, "tool_name": tool.name, "tool_input": tool.input}}));
    }

    fn ask(&mut self, wire: &mut Wire, _id: &str, tool: &Tool) -> String {
        self.asked += 1;
        let key = format!("perm_{}", self.asked);
        wire.send(json!({"event": "control_request", "request_id": key, "request": {
            "subtype": "can_use_tool", "tool_name": tool.name, "input": tool.input}}));
        key
    }

    // `agy` reports no end to a step; the next step or the result closes it.
    fn done(&mut self, _wire: &mut Wire, _id: &str, _tool: &Tool, _ok: bool, _output: &str) {}

    fn finish(&mut self, wire: &mut Wire, answer: &str) {
        wire.send(json!({"event": "result", "result": {"status": "SUCCESS", "response": answer}}));
    }

    fn fail(&mut self, wire: &mut Wire, message: &str) {
        wire.send(json!({"event": "result", "result": {"status": "ERROR", "error": message}}));
    }

    fn interrupted(&mut self, wire: &mut Wire) {
        self.fail(wire, "Interrupted by the user");
    }
}

/// OpenCode's Agent Client Protocol: JSON-RPC 2.0.
struct OpenCode {
    read_only: bool,
    model: Option<String>,
    /// The ids of `session/prompt` requests not yet answered, oldest first.
    prompts: VecDeque<Value>,
    /// The one being played.
    current: Value,
    next_request: u64,
}

impl OpenCode {
    fn new(invocation: &Invocation) -> Self {
        let permission = invocation.opencode_permission.as_deref().unwrap_or_default();
        Self {
            read_only: permission.contains("deny"),
            model: None,
            prompts: VecDeque::new(),
            current: Value::Null,
            next_request: 0,
        }
    }

    fn update(wire: &mut Wire, update: Value) {
        wire.send(json!({"jsonrpc": "2.0", "method": "session/update",
            "params": {"sessionId": "ses_mock", "update": update}}));
    }

    fn kind(tool: &Tool) -> &'static str {
        if command_of(tool).is_some() {
            "execute"
        } else if edits(tool) {
            "edit"
        } else {
            "other"
        }
    }

    fn respond(&mut self, wire: &mut Wire, result: Value) {
        let id = std::mem::take(&mut self.current);
        wire.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
}

impl Dialect for OpenCode {
    fn read(&mut self, message: Value, wire: &mut Wire) -> Incoming {
        let id = message["id"].clone();
        let params = &message["params"];
        let result = match message["method"].as_str() {
            None => {
                let outcome = &message["result"]["outcome"];
                return Incoming::Answer {
                    key: id.to_string(),
                    allow: outcome["outcome"] == "selected" && outcome["optionId"] == "allow",
                };
            }
            Some("initialize") => json!({"protocolVersion": 1,
                "agentCapabilities": {"loadSession": false}, "authMethods": []}),
            Some("session/new") => json!({"sessionId": "ses_mock"}),
            Some("session/set_model") => {
                self.model = params["modelId"].as_str().map(str::to_owned);
                json!({})
            }
            Some("session/prompt") => {
                self.prompts.push_back(id);
                let text = params["prompt"].as_array().into_iter().flatten();
                let text = text.filter_map(|part| part["text"].as_str()).collect::<Vec<_>>();
                return Incoming::Prompt(text.join("\n"));
            }
            Some("session/cancel") => return Incoming::Interrupt,
            Some(_) if id.is_null() => return Incoming::Nothing,
            Some(method) => {
                wire.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601,
                    "message": format!("{method} is not mocked")}}));
                return Incoming::Nothing;
            }
        };
        wire.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        Incoming::Nothing
    }

    fn context(&self) -> (bool, Option<String>) {
        (self.read_only, self.model.clone())
    }

    fn begin(&mut self, _wire: &mut Wire) {
        self.current = self.prompts.pop_front().unwrap_or_default();
    }

    fn text(&mut self, wire: &mut Wire, chunk: &str, _first: bool) {
        Self::update(
            wire,
            json!({"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": chunk}}),
        );
    }

    fn think(&mut self, wire: &mut Wire, chunk: &str) {
        Self::update(
            wire,
            json!({"sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": chunk}}),
        );
    }

    fn tool(&mut self, wire: &mut Wire, id: &str, tool: &Tool) {
        Self::update(
            wire,
            json!({"sessionUpdate": "tool_call", "toolCallId": id,
            "title": tool.name, "kind": Self::kind(tool), "status": "pending",
            "rawInput": tool.input}),
        );
    }

    fn ask(&mut self, wire: &mut Wire, id: &str, tool: &Tool) -> String {
        self.next_request += 1;
        let request = json!(format!("perm_{}", self.next_request));
        wire.send(json!({"jsonrpc": "2.0", "id": request, "method": "session/request_permission",
            "params": {"sessionId": "ses_mock",
                "toolCall": {"toolCallId": id, "title": tool.name, "kind": Self::kind(tool)},
                "options": [
                    {"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
                    {"optionId": "reject", "name": "Reject", "kind": "reject_once"}]}}));
        request.to_string()
    }

    fn done(&mut self, wire: &mut Wire, id: &str, _tool: &Tool, ok: bool, output: &str) {
        Self::update(
            wire,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": id,
            "status": if ok { "completed" } else { "failed" },
            "content": [{"type": "content", "content": {"type": "text", "text": output}}]}),
        );
    }

    fn finish(&mut self, wire: &mut Wire, _answer: &str) {
        self.respond(wire, json!({"stopReason": "end_turn"}));
    }

    fn fail(&mut self, wire: &mut Wire, message: &str) {
        let id = std::mem::take(&mut self.current);
        wire.send(json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32603, "message": message}}));
    }

    fn interrupted(&mut self, wire: &mut Wire) {
        self.respond(wire, json!({"stopReason": "cancelled"}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_is_known_by_its_name_or_by_how_maestro_starts_it() {
        let args = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
        assert_eq!(Cli::detect("/tmp/bin/codex", &[]), Some(Cli::Codex));
        assert_eq!(Cli::detect("maestro-mock", &args(&["acp"])), Some(Cli::OpenCode));
        assert_eq!(Cli::detect("maestro-mock", &args(&["app-server"])), Some(Cli::Codex));
        let claude = args(&["--print", "--input-format", "stream-json"]);
        assert_eq!(Cli::detect("maestro-mock", &claude), Some(Cli::Claude));
        let agy = args(&["--input-format", "stream-json"]);
        assert_eq!(Cli::detect("maestro-mock", &agy), Some(Cli::Antigravity));
        assert_eq!(Cli::detect("maestro-mock", &args(&["serve"])), None);
    }

    #[test]
    fn flags_are_read_in_both_spellings() {
        let invocation = Invocation {
            args: vec!["--model=m1".into(), "--effort".into(), "high".into()],
            opencode_permission: None,
        };
        assert_eq!(invocation.flag("--model").as_deref(), Some("m1"));
        assert_eq!(invocation.flag("--effort").as_deref(), Some("high"));
        assert_eq!(invocation.flag("--missing"), None);
    }
}
