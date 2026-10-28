//! A running agent: the process, its stdio, and the adapter between them.
//!
//! Two threads per agent. The reader turns the CLI's stdout into JSON values; the pump owns the
//! adapter and stdin, and is the only thread that touches either, so the adapter needs no lock.
//! Everything else talks to the pump through a channel.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;

use maestro_core::{AgentEvent, Host};
use maestro_host::HostError;
use serde_json::Value;

use crate::Adapter;

enum Input {
    Line(Value),
    Closed(Option<String>),
    Send(String),
    Answer { request: String, allow: bool },
    Interrupt,
    Stop,
}

/// A handle on a running agent. Dropping it stops the agent.
pub struct AgentSession {
    input: Sender<Input>,
}

impl AgentSession {
    /// Starts the adapter's CLI on `host`, sends `prompt`, and reports every event to `on_event`
    /// from the pump thread. The last event is always [`AgentEvent::Exited`].
    pub fn spawn(
        host: &Host,
        mut adapter: Box<dyn Adapter>,
        prompt: &str,
        on_event: impl Fn(AgentEvent) + Send + 'static,
    ) -> Result<Self, HostError> {
        let command = adapter.command();
        let mut child = command
            .process(host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| HostError::Spawn { program: command.program.clone(), source })?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let (input, inbox) = mpsc::channel();
        let lines = input.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let reason = loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break None,
                    Ok(_) if line.trim().is_empty() => {}
                    // CLIs print the odd banner or warning; skip lines that are not JSON.
                    Ok(_) => {
                        if let Ok(value) = serde_json::from_str(&line)
                            && lines.send(Input::Line(value)).is_err()
                        {
                            break None;
                        }
                    }
                    Err(error) => break Some(error.to_string()),
                }
            };
            lines.send(Input::Closed(reason)).ok();
        });

        let first = adapter.start(prompt);
        thread::spawn(move || {
            let mut pump =
                Pump { adapter, stdin, child, on_event: Box::new(on_event), stopping: false };
            pump.write(first);
            for message in inbox {
                match message {
                    Input::Line(value) => {
                        let output = pump.adapter.receive(value);
                        pump.write(output.replies);
                        output.events.into_iter().for_each(&pump.on_event);
                    }
                    Input::Send(text) => {
                        let messages = pump.adapter.send(&text);
                        pump.write(messages);
                    }
                    Input::Answer { request, allow } => {
                        let messages = pump.adapter.answer(&request, allow);
                        pump.write(messages);
                    }
                    Input::Interrupt => {
                        let messages = pump.adapter.interrupt();
                        pump.write(messages);
                    }
                    Input::Stop => {
                        pump.stopping = true;
                        pump.child.kill().ok();
                    }
                    Input::Closed(reason) => {
                        let status = pump.child.wait().ok();
                        // A process killed on request did not fail.
                        let failed = status.filter(|status| !status.success() && !pump.stopping);
                        let reason = reason.or_else(|| failed.map(|status| status.to_string()));
                        (pump.on_event)(AgentEvent::Exited { reason });
                        return;
                    }
                }
            }
        });
        Ok(Self { input })
    }

    /// Sends the next user message. Call it between turns.
    pub fn send(&self, text: impl Into<String>) {
        self.input.send(Input::Send(text.into())).ok();
    }

    /// Answers a permission request.
    pub fn answer(&self, request: impl Into<String>, allow: bool) {
        self.input.send(Input::Answer { request: request.into(), allow }).ok();
    }

    /// Asks the CLI to abandon the current turn; the agent stays up for the next message.
    pub fn interrupt(&self) {
        self.input.send(Input::Interrupt).ok();
    }

    /// Kills the process. [`AgentEvent::Exited`] follows.
    pub fn stop(&self) {
        self.input.send(Input::Stop).ok();
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Pump {
    adapter: Box<dyn Adapter>,
    stdin: ChildStdin,
    child: Child,
    on_event: Box<dyn Fn(AgentEvent) + Send>,
    stopping: bool,
}

impl Pump {
    fn write(&mut self, messages: Vec<Value>) {
        for message in messages {
            let mut line = message.to_string();
            line.push('\n');
            // A write fails only once the process is gone, which the reader reports.
            if self.stdin.write_all(line.as_bytes()).and_then(|()| self.stdin.flush()).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use maestro_core::ProviderKind;

    use super::*;
    use crate::{WorkerConfig, adapter};

    /// A stand-in for `claude` that answers one turn in its stream-json and then exits.
    fn fake_claude(directory: &std::path::Path) -> String {
        let script = directory.join("claude");
        std::fs::write(
            &script,
            r#"#!/bin/sh
read line
echo 'not json'
echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"hello"}'
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        script.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn a_session_pumps_a_real_process_through_its_adapter() {
        let directory =
            std::env::temp_dir().join(format!("maestro-session-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let config = WorkerConfig {
            provider: ProviderKind::ClaudeCode,
            model: None,
            effort: None,
            cwd: directory.clone(),
            instructions: String::new(),
            program: Some(fake_claude(&directory)),
            read_only: false,
        };
        let (events, received) = mpsc::channel();
        let _session = AgentSession::spawn(&Host::Local, adapter(config), "hi", move |event| {
            events.send(event).ok();
        })
        .unwrap();
        let mut seen = Vec::new();
        while let Ok(event) = received.recv_timeout(Duration::from_secs(10)) {
            let exited = matches!(event, AgentEvent::Exited { .. });
            seen.push(event);
            if exited {
                break;
            }
        }
        std::fs::remove_dir_all(&directory).ok();
        assert_eq!(
            seen,
            [
                AgentEvent::Started,
                AgentEvent::Text { text: "hello".into() },
                AgentEvent::TurnCompleted { answer: "hello".into() },
                AgentEvent::Exited { reason: None },
            ]
        );
    }
}
