//! OpenCode, spoken to in the Agent Client Protocol (`opencode acp`), JSON-RPC 2.0 over stdio.
//!
//! The handshake is `initialize` then `session/new`, plus `session/set_model` when a model is
//! chosen. Each user message is a `session/prompt` request whose response ends the turn; the
//! reply streams meanwhile as `session/update` notifications. Permission prompts arrive as
//! `session/request_permission` requests offering options, of which Maestro only ever picks a
//! one-time allow or reject, never an "always".

use std::collections::HashMap;

use maestro_core::{AgentEvent, PermissionKind, PermissionRequest, ToolCall};
use maestro_host::HostCommand;
use serde_json::{Value, json};

use crate::rpc::{self, Calls};
use crate::{Adapter, Output, WorkerConfig, content_text, with_instructions};

enum Purpose {
    Initialize,
    NewSession,
    SetModel,
    Prompt,
}

pub(crate) struct OpenCode {
    config: WorkerConfig,
    calls: Calls<Purpose>,
    session: Option<String>,
    queued: Option<String>,
    answer: String,
    asked: HashMap<String, (Value, Value)>,
    /// Tool calls already reported as started, so updates do not start them twice.
    started: Vec<String>,
    first: bool,
}

impl OpenCode {
    pub(crate) fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            calls: Calls::new(true),
            session: None,
            queued: None,
            answer: String::new(),
            asked: HashMap::new(),
            started: Vec::new(),
            first: true,
        }
    }

    fn prompt(&mut self, session: &str, text: &str) -> Value {
        self.answer.clear();
        let text = if std::mem::take(&mut self.first) {
            with_instructions(&self.config.instructions, text)
        } else {
            text.to_owned()
        };
        self.calls.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
            Purpose::Prompt,
        )
    }

    /// The session is ready: report it and send whatever prompt was waiting.
    fn ready(&mut self) -> Output {
        let mut output = Output::event(AgentEvent::Started);
        if let (Some(session), Some(prompt)) = (self.session.clone(), self.queued.take()) {
            output.replies.push(self.prompt(&session, &prompt));
        }
        output
    }

    fn response(&mut self, purpose: Purpose, message: &Value) -> Output {
        if let Some(error) = rpc::error(message) {
            return Output::event(AgentEvent::TurnFailed { message: format!("OpenCode: {error}") });
        }
        let result = &message["result"];
        match purpose {
            Purpose::Initialize => Output::reply(self.calls.request(
                "session/new",
                json!({"cwd": self.config.cwd, "mcpServers": []}),
                Purpose::NewSession,
            )),
            Purpose::NewSession => {
                let session = result["sessionId"].as_str().unwrap_or_default().to_owned();
                self.session = Some(session.clone());
                match self.config.model.clone() {
                    Some(model) => Output::reply(self.calls.request(
                        "session/set_model",
                        json!({"sessionId": session, "modelId": model}),
                        Purpose::SetModel,
                    )),
                    None => self.ready(),
                }
            }
            Purpose::SetModel => self.ready(),
            Purpose::Prompt => {
                self.asked.clear();
                self.started.clear();
                let answer = std::mem::take(&mut self.answer);
                Output::event(match result["stopReason"].as_str() {
                    Some("end_turn") | None => AgentEvent::TurnCompleted { answer },
                    Some("cancelled") => {
                        AgentEvent::TurnFailed { message: "OpenCode turn cancelled".into() }
                    }
                    Some(reason) => {
                        AgentEvent::TurnFailed { message: format!("OpenCode stopped: {reason}") }
                    }
                })
            }
        }
    }

    fn permission(&mut self, message: &Value) -> Output {
        if message["method"] != "session/request_permission" {
            return Output::reply(self.calls.refuse(&message["id"]));
        }
        let params = &message["params"];
        let tool = &params["toolCall"];
        let kind = match tool["kind"].as_str() {
            Some("execute") => PermissionKind::Command,
            Some("edit" | "delete" | "move") => PermissionKind::FileChange,
            _ => PermissionKind::Other,
        };
        let id = message["id"].to_string();
        let summary = tool["title"].as_str().unwrap_or("OpenCode action").to_owned();
        self.asked.insert(id.clone(), (message["id"].clone(), params["options"].clone()));
        Output::event(AgentEvent::PermissionRequested {
            request: PermissionRequest { id, kind, summary, detail: params.to_string() },
        })
    }

    fn update(&mut self, update: &Value) -> Output {
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();
        let event = match kind {
            "agent_message_chunk" => {
                let text = update["content"]["text"].as_str().unwrap_or_default().to_owned();
                self.answer.push_str(&text);
                AgentEvent::Text { text }
            }
            "agent_thought_chunk" => AgentEvent::Reasoning {
                text: update["content"]["text"].as_str().unwrap_or_default().into(),
            },
            "tool_call" | "tool_call_update" => {
                let id = update["toolCallId"].as_str().unwrap_or_default().to_owned();
                let done = matches!(update["status"].as_str(), Some("completed" | "failed"));
                if done {
                    AgentEvent::ToolFinished {
                        ok: update["status"] == "completed",
                        output: update["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|part| content_text(&part["content"]["text"]))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        id,
                    }
                } else if kind == "tool_call" && !self.started.contains(&id) {
                    self.started.push(id.clone());
                    AgentEvent::ToolStarted {
                        call: ToolCall {
                            id,
                            name: update["title"].as_str().unwrap_or_default().into(),
                            input: update["rawInput"].to_string(),
                        },
                    }
                } else {
                    return Output::default();
                }
            }
            _ => return Output::default(),
        };
        Output::event(event)
    }
}

impl Adapter for OpenCode {
    fn command(&self) -> HostCommand {
        HostCommand::new(self.config.program.as_deref().unwrap_or("opencode"))
            .cwd(&self.config.cwd)
            .arg("acp")
            .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
            .env("OPENCODE_AUTO_SHARE", "false")
    }

    fn start(&mut self, prompt: &str) -> Vec<Value> {
        self.queued = Some(prompt.to_owned());
        vec![self.calls.request(
            "initialize",
            json!({"protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": "maestro", "version": env!("CARGO_PKG_VERSION")}}),
            Purpose::Initialize,
        )]
    }

    fn send(&mut self, text: &str) -> Vec<Value> {
        match self.session.clone() {
            Some(session) => vec![self.prompt(&session, text)],
            None => {
                self.queued = Some(text.to_owned());
                Vec::new()
            }
        }
    }

    fn receive(&mut self, message: Value) -> Output {
        if rpc::is_request(&message) {
            return self.permission(&message);
        }
        if let Some(purpose) = self.calls.resolve(&message) {
            return self.response(purpose, &message);
        }
        if message["method"] == "session/update"
            && message["params"]["sessionId"].as_str() == self.session.as_deref()
        {
            return self.update(&message["params"]["update"]);
        }
        Output::default()
    }

    fn answer(&mut self, request: &str, allow: bool) -> Vec<Value> {
        let Some((id, options)) = self.asked.remove(request) else { return Vec::new() };
        let kind = if allow { "allow_once" } else { "reject_once" };
        let option = options
            .as_array()
            .into_iter()
            .flatten()
            .find(|option| option["kind"] == kind && option["optionId"].is_string());
        let outcome = option.map_or_else(
            || json!({"outcome": "cancelled"}),
            |option| json!({"outcome": "selected", "optionId": option["optionId"]}),
        );
        vec![self.calls.respond(&id, json!({"outcome": outcome}))]
    }

    fn interrupt(&mut self) -> Vec<Value> {
        match &self.session {
            Some(session) => {
                vec![self.calls.notify("session/cancel", json!({"sessionId": session}))]
            }
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use maestro_core::ProviderKind;

    #[test]
    fn handshake_selects_the_model_before_the_first_prompt() {
        let mut opencode = OpenCode::new(config(ProviderKind::OpenCode));
        let init = &opencode.start("Fix it")[0];
        assert_eq!(
            (init["jsonrpc"].as_str(), init["method"].as_str()),
            (Some("2.0"), Some("initialize"))
        );

        let new = &opencode
            .receive(json!({"jsonrpc":"2.0","id":init["id"],"result":{"protocolVersion":1}}))
            .replies[0];
        assert_eq!(new["params"]["cwd"], "/work/n1");
        let set = &opencode
            .receive(json!({"jsonrpc":"2.0","id":new["id"],"result":{"sessionId":"s"}}))
            .replies[0];
        assert_eq!(set["params"]["modelId"], "model-x");
        let output = opencode.receive(json!({"jsonrpc":"2.0","id":set["id"],"result":{}}));
        assert_eq!(output.events, [AgentEvent::Started]);
        let prompt = &output.replies[0];
        assert_eq!(prompt["method"], "session/prompt");

        let update = |update: Value| json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":update}});
        let events: Vec<_> = [
            update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Ok"}})),
            update(json!({"sessionUpdate":"tool_call","toolCallId":"c","title":"bash","status":"pending","rawInput":{"command":"ls"}})),
            update(json!({"sessionUpdate":"tool_call_update","toolCallId":"c","status":"in_progress"})),
            update(json!({"sessionUpdate":"tool_call_update","toolCallId":"c","status":"completed","content":[{"type":"content","content":{"type":"text","text":"a"}}]})),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"other","update":{"sessionUpdate":"agent_message_chunk","content":{"text":"x"}}}}),
            json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"stopReason":"end_turn"}}),
        ]
        .into_iter()
        .flat_map(|line| opencode.receive(line).events)
        .collect();
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[2],
            AgentEvent::ToolFinished { id: "c".into(), ok: true, output: "a".into() }
        );
        assert_eq!(events[3], AgentEvent::TurnCompleted { answer: "Ok".into() });
    }

    #[test]
    fn permission_answers_pick_a_one_time_option() {
        let mut opencode = OpenCode::new(config(ProviderKind::OpenCode));
        let output = opencode.receive(json!({"jsonrpc":"2.0","id":7,"method":"session/request_permission","params":{
            "toolCall":{"kind":"execute","title":"bash"},
            "options":[{"kind":"allow_always","optionId":"always"},{"kind":"allow_once","optionId":"once"},{"kind":"reject_once","optionId":"no"}]}}));
        let [AgentEvent::PermissionRequested { request }] = &output.events[..] else { panic!() };
        assert_eq!(request.kind, PermissionKind::Command);
        let reply = &opencode.answer(&request.id, true)[0];
        assert_eq!(reply["result"]["outcome"], json!({"outcome":"selected","optionId":"once"}));
        assert_eq!(reply["id"], 7);
    }
}
