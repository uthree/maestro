//! Antigravity CLI (`agy`), spoken to in its `stream-json` over stdio.
//!
//! Each user message is a `user` event, the CLI reports progress as `step_update` events, and the
//! turn ends with a `result` event. Auris Studio drives `agy` with native tools switched off, so
//! how `agy` asks for permission in this mode has not been observed; control requests are read in
//! Claude Code's shape, which `agy`'s stream format otherwise follows, and refused when they are
//! anything else.

use std::collections::HashMap;

use maestro_core::{AgentEvent, Effort, PermissionKind, PermissionRequest, ToolCall};
use maestro_host::HostCommand;
use serde_json::{Value, json};

use crate::{Adapter, Output, PARAGRAPH, WorkerConfig, with_instructions};

/// What a refused step reports, and what `agy` is told.
const REFUSED: &str = "The user refused this action.";

/// The tools that define, start or drive a sub-agent. Rules for them would have to reach `agy`
/// through `--settings`, which Maestro does not write yet, so a worker that may not use them is
/// noticed when it does.
pub(crate) const SUBAGENT_TOOLS: [&str; 4] =
    ["invoke_subagent", "define_subagent", "manage_subagents", "browser_subagent"];

pub(crate) struct Antigravity {
    config: WorkerConfig,
    first: bool,
    steps: u64,
    /// The tool step still open, finished when the next step or the result arrives.
    open_tool: Option<String>,
    asked: HashMap<String, Value>,
    /// Permission was refused for the open tool step, or for the next one if none is open:
    /// `agy` never reports how a step ended, so a refused step would otherwise read as done.
    refused: bool,
    /// Whether this turn's reply already arrived as text. `agy` often sends the reply only in
    /// its result, and a transcript built from events would then show nothing.
    said: bool,
    /// A tool step came after this turn's text, so the next text starts a new message.
    parted: bool,
}

impl Antigravity {
    pub(crate) fn new(config: WorkerConfig) -> Self {
        Self {
            config,
            first: true,
            steps: 0,
            open_tool: None,
            asked: HashMap::new(),
            refused: false,
            said: false,
            parted: false,
        }
    }

    fn close_tool(&mut self, output: &mut Output, ok: bool) {
        if let Some(id) = self.open_tool.take() {
            let event = if std::mem::take(&mut self.refused) {
                AgentEvent::ToolFinished { id, ok: false, output: REFUSED.into() }
            } else {
                AgentEvent::ToolFinished { id, ok, output: String::new() }
            };
            output.events.push(event);
        }
    }
}

impl Adapter for Antigravity {
    fn command(&self) -> HostCommand {
        let config = &self.config;
        let mut command = HostCommand::new(config.program.as_deref().unwrap_or("agy"))
            .cwd(&config.cwd)
            .args([
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--disable-slash-commands",
            ])
            .env_remove("ANTIGRAVITY_CONVERSATION_ID")
            .env_remove("ANTIGRAVITY_AGENT")
            .env_remove("CLAUDECODE");
        if let Some(model) = &config.model {
            command = command.arg(format!("--model={model}"));
        }
        if let Some(effort) = config.effort {
            let effort = match effort {
                Effort::Low => "low",
                Effort::Medium => "medium",
                Effort::High | Effort::Max => "high",
            };
            command = command.args(["--effort", effort]);
        }
        command
    }

    fn start(&mut self, prompt: &str) -> Vec<Value> {
        self.send(prompt)
    }

    fn send(&mut self, text: &str) -> Vec<Value> {
        let text = if std::mem::take(&mut self.first) {
            with_instructions(&self.config.instructions, text)
        } else {
            text.to_owned()
        };
        vec![json!({"event": "user", "message": {"content": text}})]
    }

    fn receive(&mut self, message: Value) -> Output {
        let mut output = Output::default();
        match message["event"].as_str().or_else(|| message["type"].as_str()) {
            Some("init") => output.events.push(AgentEvent::Started),
            Some("step_update") => {
                let step = &message["step_update"];
                self.close_tool(&mut output, true);
                if step["step_type"] == "tool" {
                    self.parted = self.said;
                    self.steps += 1;
                    let id = step["step_id"]
                        .as_str()
                        .map_or_else(|| format!("step-{}", self.steps), str::to_owned);
                    self.open_tool = Some(id.clone());
                    output.events.push(AgentEvent::ToolStarted {
                        call: ToolCall {
                            id,
                            name: step["tool_name"].as_str().unwrap_or("tool").into(),
                            input: step.get("tool_input").unwrap_or(&Value::Null).to_string(),
                        },
                    });
                } else if let Some(text) = step["text"].as_str() {
                    let mut text = text.to_owned();
                    if std::mem::take(&mut self.parted) && !text.is_empty() {
                        text.insert_str(0, PARAGRAPH);
                    }
                    self.said = true;
                    output.events.push(AgentEvent::Text { text });
                }
            }
            Some("control_request") => {
                let request = &message["request"];
                let id = message["request_id"].as_str().unwrap_or_default().to_owned();
                if request["subtype"] == "can_use_tool" {
                    self.asked.insert(id.clone(), request["input"].clone());
                    output.events.push(AgentEvent::PermissionRequested {
                        request: PermissionRequest {
                            id,
                            kind: PermissionKind::Other,
                            summary: request["tool_name"]
                                .as_str()
                                .unwrap_or("Antigravity action")
                                .into(),
                            detail: request.to_string(),
                        },
                    });
                } else {
                    output.replies.push(json!({"event": "control_response", "response": {
                        "subtype": "error", "request_id": id, "error": "not supported by Maestro"}}));
                }
            }
            Some("result") => {
                let result = &message["result"];
                let ok = result["status"] == "SUCCESS";
                self.close_tool(&mut output, ok);
                self.refused = false;
                self.parted = false;
                self.asked.clear();
                let answer = result["response"].as_str().unwrap_or_default().to_owned();
                if ok && !std::mem::take(&mut self.said) && !answer.is_empty() {
                    output.events.push(AgentEvent::Text { text: answer.clone() });
                }
                output.events.push(if ok {
                    AgentEvent::TurnCompleted { answer }
                } else {
                    AgentEvent::TurnFailed {
                        message: result["error"]
                            .as_str()
                            .unwrap_or("Antigravity did not finish successfully")
                            .into(),
                    }
                });
            }
            _ => {}
        }
        output
    }

    fn answer(&mut self, request: &str, allow: bool) -> Vec<Value> {
        let Some(input) = self.asked.remove(request) else { return Vec::new() };
        let response = if allow {
            json!({"behavior": "allow", "updatedInput": input})
        } else {
            self.refused = true;
            json!({"behavior": "deny", "message": REFUSED})
        };
        vec![json!({"event": "control_response", "response": {
            "subtype": "success", "request_id": request, "response": response}})]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use maestro_core::ProviderKind;

    #[test]
    fn command_caps_effort_at_high() {
        let mut config = config(ProviderKind::Antigravity);
        config.effort = Some(Effort::Max);
        let command = Antigravity::new(config).command();
        assert_eq!(command.program, "agy");
        assert!(command.args.windows(2).any(|pair| pair == ["--effort", "high"]));
    }

    #[test]
    fn steps_become_tool_calls_and_the_result_ends_the_turn() {
        let mut agy = Antigravity::new(config(ProviderKind::Antigravity));
        let first = &agy.start("Fix it")[0];
        assert!(first["message"]["content"].as_str().unwrap().starts_with("Only touch src/."));
        let events: Vec<_> = [
            json!({"event":"init","init":{"agent":"default"}}),
            json!({"event":"step_update","step_update":{"step_type":"tool","tool_name":"run_command"}}),
            json!({"event":"step_update","step_update":{"step_type":"model"}}),
            json!({"event":"result","result":{"status":"SUCCESS","response":"Fixed."}}),
        ]
        .into_iter()
        .flat_map(|line| agy.receive(line).events)
        .collect();
        assert_eq!(events.len(), 5);
        assert!(
            matches!(&events[1], AgentEvent::ToolStarted { call } if call.name == "run_command")
        );
        assert_eq!(
            events[2],
            AgentEvent::ToolFinished { id: "step-1".into(), ok: true, output: String::new() }
        );
        // The reply came only in the result, so it is also reported as text.
        assert_eq!(events[3], AgentEvent::Text { text: "Fixed.".into() });
        assert_eq!(events[4], AgentEvent::TurnCompleted { answer: "Fixed.".into() });
        assert_eq!(agy.send("More")[0]["message"]["content"], "More");
    }

    #[test]
    fn a_refused_step_finishes_as_failed() {
        let mut agy = Antigravity::new(config(ProviderKind::Antigravity));
        let step = |id: &str| json!({"event":"step_update","step_update":{"step_type":"tool","step_id":id,"tool_name":"run_command"}});
        let ask = |id: &str| json!({"event":"control_request","request_id":id,"request":{"subtype":"can_use_tool","tool_name":"run_command","input":{}}});
        let mut events = Vec::new();
        events.extend(agy.receive(step("s1")).events);
        events.extend(agy.receive(ask("p1")).events);
        assert_eq!(agy.answer("p1", false)[0]["response"]["response"]["behavior"], "deny");
        events.extend(agy.receive(step("s2")).events);
        events.extend(agy.receive(ask("p2")).events);
        agy.answer("p2", true);
        events.extend(
            agy.receive(json!({"event":"result","result":{"status":"SUCCESS","response":"Ok"}}))
                .events,
        );
        let finished: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::ToolFinished { id, ok, .. } => Some((id.as_str(), *ok)),
                _ => None,
            })
            .collect();
        assert_eq!(finished, [("s1", false), ("s2", true)]);
    }
}
