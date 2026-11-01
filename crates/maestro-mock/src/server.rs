//! An OpenAI-compatible HTTP server answering from the same scenario as the stand-in CLIs.
//!
//! It serves `POST /v1/chat/completions`, streamed (server-sent events) or not, and
//! `GET /v1/models`. A real CLI pointed at it (Codex and OpenCode both take a custom
//! OpenAI-compatible base URL) then runs a whole turn, tools included, without an account, which
//! checks the CLI's own output against the adapters when the stand-ins are not enough.
//!
//! The server keeps no conversation. The rule is matched against the last user message, and the
//! reply resumes after as many tool calls as the request already carries results for: the first
//! request gets the reply up to its first tool call (`finish_reason: "tool_calls"`), the request
//! with that call's result gets the reply up to the next, and so on. A continuation is matched
//! like the first request, except that a rule with `times` still matches after its last use.
//!
//! Only what tests need of HTTP is here: one request per connection, a `Content-Length` body,
//! and `Connection: close`.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::scenario::{Player, Prompt, Scenario, Step, Stream};

/// A running server. Dropping it stops it.
pub struct Server {
    address: SocketAddr,
    shared: Arc<Shared>,
}

struct Shared {
    player: Mutex<Player>,
    stream: Stream,
    requests: Mutex<Vec<Value>>,
    stopped: AtomicBool,
    next: AtomicU64,
}

impl Server {
    /// Listens on `address` (port 0 picks a free one) and serves on a thread of its own.
    pub fn start(address: impl ToSocketAddrs, scenario: Scenario) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            stream: scenario.stream,
            player: Mutex::new(Player::new(scenario)),
            requests: Mutex::default(),
            stopped: AtomicBool::new(false),
            next: AtomicU64::new(1),
        });
        let serving = shared.clone();
        thread::spawn(move || {
            for connection in listener.incoming() {
                if serving.stopped.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(connection) = connection else { continue };
                let shared = serving.clone();
                thread::spawn(move || shared.serve(connection));
            }
        });
        Ok(Self { address, shared })
    }

    /// Where it listens.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The base URL a client is given, ending in `/v1`.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }

    /// The body of every chat request so far, oldest first.
    pub fn requests(&self) -> Vec<Value> {
        self.shared.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        // Wakes the accepting thread so it sees the flag.
        TcpStream::connect(self.address).ok();
    }
}

/// A parsed request.
struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &TcpStream) -> io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut length = 0;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Request { method, path, body })
}

fn respond(mut stream: &TcpStream, status: u16, body: &Value) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Mock Status",
    };
    let body = body.to_string();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes())).ok();
}

fn error_body(status: u16, message: &str) -> Value {
    let kind = match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        429 => "rate_limit_error",
        _ => "server_error",
    };
    json!({"error": {"message": message, "type": kind, "param": null, "code": null}})
}

/// What one chat request is answered with: the steps from where the conversation stands to the
/// next tool call, or to the end.
struct Answer {
    steps: Vec<Step>,
    /// The number of this reply's tool calls already made, so ids continue.
    made: usize,
    turn: u32,
}

impl Shared {
    fn serve(&self, stream: TcpStream) {
        let Ok(request) = read_request(&stream) else { return };
        let route = request.path.split('?').next().unwrap_or_default().trim_end_matches('/');
        match (request.method.as_str(), route) {
            ("GET", path) if path.ends_with("/models") => respond(
                &stream,
                200,
                &json!({"object": "list", "data": [
                    {"id": "mock-model", "object": "model", "created": 0, "owned_by": "maestro-mock"}]}),
            ),
            ("POST", path) if path.ends_with("/chat/completions") => {
                match serde_json::from_slice::<Value>(&request.body) {
                    Ok(body) => self.chat(&stream, body),
                    Err(error) => respond(&stream, 400, &error_body(400, &error.to_string())),
                }
            }
            _ => respond(&stream, 404, &error_body(404, &format!("no route for {route}"))),
        }
    }

    fn answer(&self, body: &Value) -> Answer {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let last_user = messages.iter().rposition(|message| message["role"] == "user");
        let text = last_user.map(|at| content_text(&messages[at]["content"])).unwrap_or_default();
        let turn = messages.iter().filter(|message| message["role"] == "user").count() as u32;
        // Tool calls of this reply already answered: the assistant's calls after the prompt.
        let made: usize = messages[last_user.map_or(0, |at| at + 1)..]
            .iter()
            .filter(|message| message["role"] == "assistant")
            .map(|message| message["tool_calls"].as_array().map_or(0, Vec::len))
            .sum();
        let prompt = Prompt {
            provider: "openai",
            text: &text,
            turn,
            read_only: false,
            model: body["model"].as_str(),
        };
        let mut player = self.player.lock().expect("player lock");
        let steps = if made == 0 { player.reply(&prompt) } else { player.resume(&prompt) };
        // Skip what came before the `made`-th tool call, and stop after the next one.
        let mut calls = 0;
        let mut rest = Vec::new();
        for step in steps {
            let tool = matches!(step, Step::Tool(_));
            if calls >= made {
                rest.push(step);
                if tool {
                    break;
                }
            }
            if tool {
                calls += 1;
            }
        }
        Answer { steps: rest, made, turn }
    }

    fn chat(&self, stream: &TcpStream, body: Value) {
        self.requests.lock().expect("requests lock").push(body.clone());
        let answer = self.answer(&body);
        let model = body["model"].as_str().unwrap_or("mock-model").to_owned();
        let id = format!("chatcmpl-mock-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let usage = body["stream_options"]["include_usage"] == true;
        if body["stream"] == true {
            let mut events = Events { stream, id, model, started: false, broken: false };
            events.play(&answer, self.stream, usage);
        } else {
            self.whole(stream, &answer, &id, &model);
        }
    }

    /// Answers in one JSON body.
    fn whole(&self, stream: &TcpStream, answer: &Answer, id: &str, model: &str) {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        for step in &answer.steps {
            match step {
                Step::Say(said) => {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(said);
                }
                Step::Think(thought) => reasoning.push_str(thought),
                Step::Tool(tool) => calls.push(call(answer, calls.len(), tool)),
                Step::SleepMs(millis) => thread::sleep(Duration::from_millis(*millis)),
                Step::Hang => loop {
                    thread::sleep(Duration::from_secs(3600));
                },
                Step::Fail(message) => return respond(stream, 500, &error_body(500, message)),
                Step::HttpError { status, message } => {
                    return respond(stream, *status, &error_body(*status, message));
                }
                Step::Exit(_) => return,
                Step::Noise(_) => {}
            }
        }
        let mut message = json!({"role": "assistant", "content": text});
        if !calls.is_empty() {
            if text.is_empty() {
                message["content"] = Value::Null;
            }
            message["tool_calls"] = json!(calls);
        }
        if !reasoning.is_empty() {
            message["reasoning_content"] = json!(reasoning);
        }
        let finish = if calls.is_empty() { "stop" } else { "tool_calls" };
        let completion = (text.len() + reasoning.len()) as u64 / 4 + 1;
        respond(
            stream,
            200,
            &json!({"id": id, "object": "chat.completion", "created": 0, "model": model,
                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                "usage": usage(completion)}),
        );
    }
}

fn usage(completion: u64) -> Value {
    json!({"prompt_tokens": 1, "completion_tokens": completion, "total_tokens": completion + 1})
}

/// The `n`-th tool call of `answer`'s part of the reply, in OpenAI's shape.
fn call(answer: &Answer, n: usize, tool: &crate::scenario::Tool) -> Value {
    let arguments = match &tool.input {
        Value::Null => "{}".to_owned(),
        input => input.to_string(),
    };
    json!({"id": format!("call_{}_{}", answer.turn, answer.made + n + 1), "type": "function",
        "function": {"name": tool.name, "arguments": arguments}})
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n")
        }
        _ => String::new(),
    }
}

/// A streamed answer, as server-sent events.
struct Events<'a> {
    stream: &'a TcpStream,
    id: String,
    model: String,
    started: bool,
    broken: bool,
}

impl Events<'_> {
    fn raw(&mut self, text: &str) {
        if !self.started {
            self.started = true;
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                        Cache-Control: no-cache\r\nConnection: close\r\n\r\n";
            self.write(head);
        }
        self.write(text);
    }

    fn write(&mut self, text: &str) {
        let mut stream = self.stream;
        if !self.broken && stream.write_all(text.as_bytes()).and_then(|()| stream.flush()).is_err()
        {
            self.broken = true;
        }
    }

    fn chunk(&mut self, delta: Value, finish: Option<&str>) {
        let chunk = json!({"id": self.id, "object": "chat.completion.chunk", "created": 0,
            "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        self.raw(&format!("data: {chunk}\n\n"));
    }

    fn play(&mut self, answer: &Answer, stream: Stream, usage: bool) {
        let mut said = false;
        let mut calls = 0;
        let mut first = true;
        let mut pause = |events: &mut Self| {
            if !std::mem::replace(&mut first, false) {
                thread::sleep(stream.delay());
            }
            events.broken
        };
        for step in &answer.steps {
            match step {
                Step::Say(text) => {
                    let mut chunks = stream.chunks(text);
                    if std::mem::replace(&mut said, true) {
                        chunks.insert(0, "\n\n".into());
                    }
                    for chunk in chunks {
                        if pause(self) {
                            return;
                        }
                        self.chunk(json!({"role": "assistant", "content": chunk}), None);
                    }
                }
                Step::Think(text) => {
                    for chunk in stream.chunks(text) {
                        if pause(self) {
                            return;
                        }
                        self.chunk(json!({"role": "assistant", "reasoning_content": chunk}), None);
                    }
                }
                Step::Tool(tool) => {
                    let mut call = call(answer, calls, tool);
                    call["index"] = json!(calls);
                    calls += 1;
                    self.chunk(json!({"role": "assistant", "tool_calls": [call]}), None);
                }
                Step::SleepMs(millis) => thread::sleep(Duration::from_millis(*millis)),
                Step::Hang => {
                    // Comments keep the connection open; a failed write means the client left.
                    while !self.broken {
                        self.raw(": waiting\n\n");
                        thread::sleep(Duration::from_millis(500));
                    }
                    return;
                }
                Step::Fail(message) | Step::HttpError { message, .. } if self.started => {
                    let error = error_body(500, message);
                    self.raw(&format!("data: {error}\n\n"));
                    return;
                }
                Step::Fail(message) => return respond(self.stream, 500, &error_body(500, message)),
                Step::HttpError { status, message } => {
                    return respond(self.stream, *status, &error_body(*status, message));
                }
                Step::Exit(_) => return,
                Step::Noise(line) => self.raw(&format!("data: {line}\n\n")),
            }
        }
        self.chunk(json!({}), Some(if calls > 0 { "tool_calls" } else { "stop" }));
        if usage {
            let chunk = json!({"id": self.id, "object": "chat.completion.chunk", "created": 0,
                "model": self.model, "choices": [], "usage": usage_of(answer)});
            self.raw(&format!("data: {chunk}\n\n"));
        }
        self.raw("data: [DONE]\n\n");
    }
}

fn usage_of(answer: &Answer) -> Value {
    let length: usize = answer
        .steps
        .iter()
        .map(|step| match step {
            Step::Say(text) | Step::Think(text) => text.len(),
            _ => 0,
        })
        .sum();
    usage(length as u64 / 4 + 1)
}
