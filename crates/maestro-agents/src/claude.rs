//! Claude Code, spoken to in `stream-json` over stdio.
//!
//! One process serves the whole conversation: each user message is a line on stdin, and the turn
//! ends with a `result` line. Tool permissions arrive as `control_request`s (because of
//! `--permission-prompt-tool stdio`) and are answered with `control_response`s.

use std::collections::HashMap;

use maestro_core::{AgentEvent, Effort, PermissionKind, PermissionRequest, ToolCall};
use maestro_host::HostCommand;
use serde_json::{Value, json};

use crate::{Adapter, Output, PARAGRAPH, WorkerConfig, content_text};

pub(crate) struct Claude {
    config: WorkerConfig,
    answer: String,
    /// Whether this turn's text already arrived as deltas, so the full message is not repeated.
    streamed: bool,
    /// Inputs of permission requests awaiting an answer; Claude Code wants them echoed back.
    asked: HashMap<String, Value>,
}

impl Claude {
    pub(crate) fn new(config: WorkerConfig) -> Self {
        Self { config, answer: String::new(), streamed: false, asked: HashMap::new() }
    }

    fn user(text: &str) -> Value {
        json!({"type": "user", "message": {"role": "user", "content": text}})
    }

    fn control(&mut self, message: &Value) -> Output {
        let request = &message["request"];
        let id = message["request_id"].as_str().unwrap_or_default().to_owned();
        if request["subtype"] != "can_use_tool" {
            return Output::reply(json!({"type": "control_response", "response": {
                "subtype": "error", "request_id": id, "error": "not supported by Maestro"}}));
        }
        let tool = request["tool_name"].as_str().unwrap_or_default();
        let kind = match tool {
            "Bash" => PermissionKind::Command,
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => PermissionKind::FileChange,
            _ => PermissionKind::Other,
        };
        let input = &request["input"];
        let summary = match input["command"].as_str().or_else(|| input["file_path"].as_str()) {
            Some(target) => format!("{tool}: {target}"),
            None => tool.to_owned(),
        };
        self.asked.insert(id.clone(), input.clone());
        Output::event(AgentEvent::PermissionRequested {
            request: PermissionRequest { id, kind, summary, detail: request.to_string() },
        })
    }
}

impl Adapter for Claude {
    fn command(&self) -> HostCommand {
        let config = &self.config;
        let mut command = HostCommand::new(config.program.as_deref().unwrap_or("claude"))
            .cwd(&config.cwd)
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-mode",
                "default",
                "--permission-prompt-tool",
                "stdio",
            ])
            // A worker must not inherit the runtime of an agent that started the daemon.
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT");
        if !config.instructions.is_empty() {
            command = command.args(["--append-system-prompt", &config.instructions]);
        }
        if let Some(model) = &config.model {
            command = command.arg(format!("--model={model}"));
        }
        if let Some(effort) = config.effort {
            let effort = match effort {
                Effort::Low => "low",
                Effort::Medium => "medium",
                Effort::High => "high",
                Effort::Max => "max",
            };
            command = command.args(["--effort", effort]);
        }
        command
    }

    fn start(&mut self, prompt: &str) -> Vec<Value> {
        self.send(prompt)
    }

    fn send(&mut self, text: &str) -> Vec<Value> {
        self.answer.clear();
        self.streamed = false;
        vec![Self::user(text)]
    }

    fn receive(&mut self, message: Value) -> Output {
        if message["type"] == "control_request" {
            return self.control(&message);
        }
        // Messages from Claude Code's own subagents belong to their parent tool call.
        if !message["parent_tool_use_id"].is_null() {
            return Output::default();
        }
        let mut output = Output::default();
        match message["type"].as_str().unwrap_or_default() {
            "system" if message["subtype"] == "init" => output.events.push(AgentEvent::Started),
            "stream_event" => {
                let delta = &message["event"]["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        // A new message after a tool call starts a new paragraph.
                        if !self.streamed && !self.answer.is_empty() {
                            output.events.push(AgentEvent::Text { text: PARAGRAPH.into() });
                        }
                        self.streamed = true;
                        let text = delta["text"].as_str().unwrap_or_default().to_owned();
                        output.events.push(AgentEvent::Text { text });
                    }
                    "thinking_delta" => {
                        let text = delta["thinking"].as_str().unwrap_or_default().to_owned();
                        output.events.push(AgentEvent::Reasoning { text });
                    }
                    _ => {}
                }
            }
            "assistant" => {
                for part in message["message"]["content"].as_array().into_iter().flatten() {
                    match part["type"].as_str().unwrap_or_default() {
                        "text" => {
                            let mut text = part["text"].as_str().unwrap_or_default().to_owned();
                            if !self.answer.is_empty() {
                                text.insert_str(0, PARAGRAPH);
                            }
                            self.answer.push_str(&text);
                            if !self.streamed {
                                output.events.push(AgentEvent::Text { text });
                            }
                        }
                        "tool_use" => output.events.push(AgentEvent::ToolStarted {
                            call: ToolCall {
                                id: part["id"].as_str().unwrap_or_default().to_owned(),
                                name: part["name"].as_str().unwrap_or_default().to_owned(),
                                input: part["input"].to_string(),
                            },
                        }),
                        _ => {}
                    }
                }
                self.streamed = false;
            }
            "user" => {
                for part in message["message"]["content"].as_array().into_iter().flatten() {
                    if part["type"] == "tool_result" {
                        output.events.push(AgentEvent::ToolFinished {
                            id: part["tool_use_id"].as_str().unwrap_or_default().to_owned(),
                            ok: part["is_error"] != true,
                            output: content_text(&part["content"]),
                        });
                    }
                }
            }
            "result" => {
                self.asked.clear();
                if message["is_error"] == true || message["subtype"] != "success" {
                    let message = message["result"]
                        .as_str()
                        .or_else(|| message["subtype"].as_str())
                        .unwrap_or("Claude Code turn failed")
                        .to_owned();
                    output.events.push(AgentEvent::TurnFailed { message });
                } else {
                    let mut answer = std::mem::take(&mut self.answer);
                    if answer.is_empty() {
                        answer = message["result"].as_str().unwrap_or_default().to_owned();
                    }
                    output.events.push(AgentEvent::TurnCompleted { answer });
                }
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
            json!({"behavior": "deny", "message": "The user refused this action."})
        };
        vec![json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request, "response": response}})]
    }

    fn interrupt(&mut self) -> Vec<Value> {
        vec![json!({"type": "control_request", "request_id": "maestro-interrupt",
            "request": {"subtype": "interrupt"}})]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use maestro_core::ProviderKind;

    #[test]
    fn command_streams_json_and_routes_permissions_through_stdio() {
        let command = Claude::new(config(ProviderKind::ClaudeCode)).command();
        assert_eq!(command.program, "claude");
        let args = &command.args;
        assert!(args.windows(2).any(|pair| pair == ["--permission-prompt-tool", "stdio"]));
        assert!(args.windows(2).any(|pair| pair == ["--append-system-prompt", "Only touch src/."]));
        assert!(args.windows(2).any(|pair| pair == ["--effort", "high"]));
        assert!(args.iter().any(|arg| arg == "--model=model-x"));
        assert_eq!(command.cwd.as_deref(), Some(std::path::Path::new("/work/n1")));
    }

    #[test]
    fn a_turn_streams_text_once_and_reports_tools() {
        let mut claude = Claude::new(config(ProviderKind::ClaudeCode));
        assert_eq!(claude.start("Fix it")[0]["message"]["content"], "Fix it");
        let lines = [
            json!({"type":"system","subtype":"init"}),
            json!({"type":"stream_event","event":{"delta":{"type":"thinking_delta","thinking":"Hmm"}}}),
            json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"Done."}}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Done."},
                {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}]}}),
            json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"a.txt"}]}]}}),
            json!({"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"text","text":"sub"}]}}),
            json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"All set."}}}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"All set."}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"All set."}),
        ];
        let events: Vec<_> =
            lines.into_iter().flat_map(|line| claude.receive(line).events).collect();
        assert_eq!(
            events,
            [
                AgentEvent::Started,
                AgentEvent::Reasoning { text: "Hmm".into() },
                AgentEvent::Text { text: "Done.".into() },
                AgentEvent::ToolStarted {
                    call: ToolCall {
                        id: "t1".into(),
                        name: "Bash".into(),
                        input: r#"{"command":"ls"}"#.into()
                    }
                },
                AgentEvent::ToolFinished { id: "t1".into(), ok: true, output: "a.txt".into() },
                AgentEvent::Text { text: "\n\n".into() },
                AgentEvent::Text { text: "All set.".into() },
                AgentEvent::TurnCompleted { answer: "Done.\n\nAll set.".into() },
            ]
        );
    }

    #[test]
    fn permissions_are_asked_and_answered_with_the_original_input() {
        let mut claude = Claude::new(config(ProviderKind::ClaudeCode));
        let output = claude.receive(json!({"type":"control_request","request_id":"r1",
            "request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf build"}}}));
        let [AgentEvent::PermissionRequested { request }] = &output.events[..] else { panic!() };
        assert_eq!(
            (request.kind, request.summary.as_str()),
            (PermissionKind::Command, "Bash: rm -rf build")
        );

        let reply = &claude.answer("r1", true)[0];
        assert_eq!(reply["response"]["request_id"], "r1");
        assert_eq!(reply["response"]["response"]["updatedInput"]["command"], "rm -rf build");
        assert!(claude.answer("r1", true).is_empty(), "an answer is sent once");

        let other = claude.receive(json!({"type":"control_request","request_id":"r2","request":{"subtype":"hook_callback"}}));
        assert_eq!(other.replies[0]["response"]["subtype"], "error");
    }

    #[test]
    fn a_failed_result_fails_the_turn() {
        let mut claude = Claude::new(config(ProviderKind::ClaudeCode));
        let output =
            claude.receive(json!({"type":"result","subtype":"error_max_turns","is_error":true}));
        assert_eq!(output.events, [AgentEvent::TurnFailed { message: "error_max_turns".into() }]);
    }
}
