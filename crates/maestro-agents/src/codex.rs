//! Codex, spoken to as `codex app-server` JSON-RPC over stdio.
//!
//! The handshake is `initialize`, the `initialized` notification, then `thread/start`; each user
//! message is a `turn/start` on that thread, and the turn ends with a `turn/completed`
//! notification. Command and file-change approvals arrive as requests from the server.

use std::collections::HashMap;

use maestro_core::{AgentEvent, Effort, PermissionKind, PermissionRequest, ToolCall};
use maestro_host::HostCommand;
use serde_json::{Value, json};

use crate::rpc::{self, Calls};
use crate::{Adapter, Output, WorkerConfig, content_text, with_instructions};

enum Purpose {
    Initialize,
    StartThread,
    StartTurn,
    Other,
}

pub(crate) struct Codex {
    config: WorkerConfig,
    calls: Calls<Purpose>,
    thread: Option<String>,
    turn: Option<String>,
    /// A prompt waiting for the thread to exist.
    queued: Option<String>,
    answer: String,
    /// Approval requests awaiting an answer, by the id Maestro gave them.
    asked: HashMap<String, Value>,
    first: bool,
}

impl Codex {
    pub(crate) fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            calls: Calls::new(false),
            thread: None,
            turn: None,
            queued: None,
            answer: String::new(),
            asked: HashMap::new(),
            first: true,
        }
    }

    fn turn_start(&mut self, thread: &str, text: &str) -> Value {
        self.answer.clear();
        let text = if std::mem::take(&mut self.first) {
            with_instructions(&self.config.instructions, text)
        } else {
            text.to_owned()
        };
        let effort = self.config.effort.map(|effort| match effort {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "xhigh",
        });
        self.calls.request(
            "turn/start",
            json!({"threadId": thread, "input": [{"type": "text", "text": text}], "effort": effort}),
            Purpose::StartTurn,
        )
    }

    fn response(&mut self, purpose: Purpose, message: &Value) -> Output {
        if let Some(error) = rpc::error(message) {
            return Output::event(AgentEvent::TurnFailed { message: format!("Codex: {error}") });
        }
        let result = &message["result"];
        match purpose {
            Purpose::Initialize => {
                let thread = self.calls.request(
                    "thread/start",
                    json!({"model": self.config.model, "cwd": self.config.cwd,
                        "approvalPolicy": "on-request", "sandbox": "workspace-write"}),
                    Purpose::StartThread,
                );
                Output {
                    events: Vec::new(),
                    replies: vec![self.calls.notify("initialized", json!({})), thread],
                }
            }
            Purpose::StartThread => {
                let thread = result["thread"]["id"].as_str().unwrap_or_default().to_owned();
                self.thread = Some(thread.clone());
                let mut output = Output::event(AgentEvent::Started);
                if let Some(prompt) = self.queued.take() {
                    output.replies.push(self.turn_start(&thread, &prompt));
                }
                output
            }
            Purpose::StartTurn => {
                self.turn = result["turn"]["id"].as_str().map(str::to_owned);
                Output::default()
            }
            Purpose::Other => Output::default(),
        }
    }

    fn approval(&mut self, message: &Value) -> Output {
        let kind = match message["method"].as_str() {
            Some("item/commandExecution/requestApproval") => PermissionKind::Command,
            Some("item/fileChange/requestApproval") => PermissionKind::FileChange,
            _ => return Output::reply(self.calls.refuse(&message["id"])),
        };
        let params = &message["params"];
        let id = message["id"].to_string();
        let summary = match (kind, params["command"].as_str()) {
            (PermissionKind::Command, Some(command)) => format!("Run: {command}"),
            (PermissionKind::Command, None) => "Run a command".into(),
            _ => "Change files".into(),
        };
        self.asked.insert(id.clone(), message["id"].clone());
        Output::event(AgentEvent::PermissionRequested {
            request: PermissionRequest { id, kind, summary, detail: params.to_string() },
        })
    }

    fn notification(&mut self, message: &Value) -> Output {
        let params = &message["params"];
        // Notifications from an earlier turn arrive late after an interrupt; ignore them.
        if let (Some(turn), Some(current)) = (params["turnId"].as_str(), self.turn.as_deref())
            && turn != current
        {
            return Output::default();
        }
        let method = message["method"].as_str().unwrap_or_default();
        let item = &params["item"];
        let event = match method {
            "item/agentMessage/delta" => {
                let text = params["delta"].as_str().unwrap_or_default().to_owned();
                self.answer.push_str(&text);
                AgentEvent::Text { text }
            }
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                AgentEvent::Reasoning { text: params["delta"].as_str().unwrap_or_default().into() }
            }
            "item/completed" if item["type"] == "agentMessage" && self.answer.is_empty() => {
                let text = item["text"].as_str().unwrap_or_default().to_owned();
                self.answer.push_str(&text);
                AgentEvent::Text { text }
            }
            "item/started" if is_tool(item) => AgentEvent::ToolStarted {
                call: ToolCall {
                    id: item["id"].as_str().unwrap_or_default().into(),
                    name: tool_name(item),
                    input: item
                        .get("command")
                        .or(item.get("arguments"))
                        .unwrap_or(item)
                        .to_string(),
                },
            },
            "item/completed" if is_tool(item) => AgentEvent::ToolFinished {
                id: item["id"].as_str().unwrap_or_default().into(),
                ok: item["status"] == "completed"
                    && item["exitCode"].as_i64().is_none_or(|code| code == 0)
                    && item["error"].is_null(),
                output: item["aggregatedOutput"]
                    .as_str()
                    .map_or_else(|| content_text(&item["result"]["content"]), str::to_owned),
            },
            "turn/completed" => {
                self.turn = None;
                self.asked.clear();
                let turn = &params["turn"];
                match turn["status"].as_str() {
                    Some("completed") => {
                        AgentEvent::TurnCompleted { answer: std::mem::take(&mut self.answer) }
                    }
                    Some("interrupted") => {
                        AgentEvent::TurnFailed { message: "Codex turn interrupted".into() }
                    }
                    _ => AgentEvent::TurnFailed {
                        message: format!(
                            "Codex turn failed: {}",
                            turn["error"]["message"].as_str().unwrap_or("unknown error")
                        ),
                    },
                }
            }
            _ => return Output::default(),
        };
        Output::event(event)
    }
}

fn is_tool(item: &Value) -> bool {
    matches!(item["type"].as_str(), Some("commandExecution" | "fileChange" | "mcpToolCall"))
}

fn tool_name(item: &Value) -> String {
    match item["type"].as_str() {
        Some("mcpToolCall") => item["tool"].as_str().unwrap_or("mcpToolCall").into(),
        other => other.unwrap_or_default().into(),
    }
}

impl Adapter for Codex {
    fn command(&self) -> HostCommand {
        HostCommand::new(self.config.program.as_deref().unwrap_or("codex"))
            .cwd(&self.config.cwd)
            .args(["app-server", "--listen", "stdio://"])
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SANDBOX")
    }

    fn start(&mut self, prompt: &str) -> Vec<Value> {
        self.queued = Some(prompt.to_owned());
        vec![self.calls.request(
            "initialize",
            json!({"clientInfo": {"name": "maestro", "title": "Maestro", "version": env!("CARGO_PKG_VERSION")}}),
            Purpose::Initialize,
        )]
    }

    fn send(&mut self, text: &str) -> Vec<Value> {
        match self.thread.clone() {
            Some(thread) => vec![self.turn_start(&thread, text)],
            None => {
                self.queued = Some(text.to_owned());
                Vec::new()
            }
        }
    }

    fn receive(&mut self, message: Value) -> Output {
        if rpc::is_request(&message) {
            return self.approval(&message);
        }
        if let Some(purpose) = self.calls.resolve(&message) {
            return self.response(purpose, &message);
        }
        self.notification(&message)
    }

    fn answer(&mut self, request: &str, allow: bool) -> Vec<Value> {
        let Some(id) = self.asked.remove(request) else { return Vec::new() };
        vec![self.calls.respond(&id, json!({"decision": if allow { "accept" } else { "decline" }}))]
    }

    fn interrupt(&mut self) -> Vec<Value> {
        match (self.thread.clone(), self.turn.clone()) {
            (Some(thread), Some(turn)) => vec![self.calls.request(
                "turn/interrupt",
                json!({"threadId": thread, "turnId": turn}),
                Purpose::Other,
            )],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use maestro_core::ProviderKind;

    #[test]
    fn handshake_then_prompt_then_turn() {
        let mut codex = Codex::new(config(ProviderKind::Codex));
        let init = &codex.start("Fix it")[0];
        assert_eq!(init["method"], "initialize");
        assert!(init.get("jsonrpc").is_none());

        let output = codex.receive(json!({"id": init["id"], "result": {}}));
        assert_eq!(output.replies[0]["method"], "initialized");
        let thread = &output.replies[1];
        assert_eq!(thread["params"]["sandbox"], "workspace-write");
        assert_eq!(thread["params"]["model"], "model-x");

        let output = codex.receive(json!({"id": thread["id"], "result": {"thread": {"id": "th"}}}));
        assert_eq!(output.events, [AgentEvent::Started]);
        let turn = &output.replies[0];
        assert_eq!(turn["params"]["threadId"], "th");
        assert_eq!(turn["params"]["effort"], "high");
        let text = turn["params"]["input"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("Only touch src/.") && text.ends_with("Fix it"));

        codex.receive(json!({"id": turn["id"], "result": {"turn": {"id": "t1"}}}));
        let events: Vec<_> = [
            json!({"method":"item/agentMessage/delta","params":{"turnId":"t1","delta":"Hi"}}),
            json!({"method":"item/agentMessage/delta","params":{"turnId":"old","delta":"stale"}}),
            json!({"method":"item/started","params":{"turnId":"t1","item":{"id":"c1","type":"commandExecution","command":"ls"}}}),
            json!({"method":"item/completed","params":{"turnId":"t1","item":{"id":"c1","type":"commandExecution","status":"completed","exitCode":1,"aggregatedOutput":"no"}}}),
            json!({"method":"turn/completed","params":{"turn":{"id":"t1","status":"completed"}}}),
        ]
        .into_iter()
        .flat_map(|line| codex.receive(line).events)
        .collect();
        assert_eq!(events[0], AgentEvent::Text { text: "Hi".into() });
        assert!(matches!(&events[1], AgentEvent::ToolStarted { call } if call.input == r#""ls""#));
        assert!(matches!(&events[2], AgentEvent::ToolFinished { ok: false, .. }));
        assert_eq!(events[3], AgentEvent::TurnCompleted { answer: "Hi".into() });

        // The next message goes straight to the existing thread, without instructions again.
        let next = &codex.send("Again")[0];
        assert_eq!(next["params"]["input"][0]["text"], "Again");
    }

    #[test]
    fn approvals_are_answered_by_the_server_request_id() {
        let mut codex = Codex::new(config(ProviderKind::Codex));
        let output = codex
            .receive(json!({"id": 900, "method": "item/commandExecution/requestApproval",
            "params": {"command": "cargo test"}}));
        let [AgentEvent::PermissionRequested { request }] = &output.events[..] else { panic!() };
        assert_eq!(request.summary, "Run: cargo test");
        let reply = &codex.answer(&request.id, false)[0];
        assert_eq!(reply, &json!({"id": 900, "result": {"decision": "decline"}}));

        let other =
            codex.receive(json!({"id": 901, "method": "account/chatgptAuthTokens/refresh"}));
        assert_eq!(other.replies[0]["error"]["code"], -32601);
    }
}
