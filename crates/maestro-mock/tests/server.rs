//! The OpenAI-compatible server, spoken to over plain TCP as any HTTP client would.

use std::io::{Read, Write};
use std::net::TcpStream;

use maestro_mock::{Scenario, Server};
use serde_json::{Value, json};

const SCENARIO: &str = r#"{
    "stream": {"chunk": 4},
    "rules": [
        {"when": {"prompt_contains": "weather"}, "reply": [
            {"think": "Need the tool."},
            {"say": "Checking."},
            {"tool": {"name": "get_weather", "input": {"city": "Tokyo"}}},
            {"say": "Sunny in Tokyo."}
        ]},
        {"when": {"prompt_contains": "busy"}, "times": 1,
         "reply": [{"http_error": {"status": 429, "message": "slow down"}}]},
        {"when": {"prompt_contains": "broken"}, "reply": [{"say": "Half"}, {"exit": 1}]}
    ],
    "otherwise": [{"say": "echo: {prompt}"}]
}"#;

fn server() -> Server {
    Server::start("127.0.0.1:0", Scenario::from_json(SCENARIO).unwrap()).unwrap()
}

/// Sends one request and returns the status and the body.
fn http(server: &Server, method: &str, path: &str, body: Option<&Value>) -> (u16, String) {
    let mut stream = TcpStream::connect(server.address()).unwrap();
    let body = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: mock\r\nAuthorization: Bearer x\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((&response, ""));
    let status = head.split_whitespace().nth(1).map_or(0, |code| code.parse().unwrap());
    (status, body.to_owned())
}

fn chat(server: &Server, body: Value) -> (u16, Value) {
    let (status, body) = http(server, "POST", "/v1/chat/completions", Some(&body));
    (status, serde_json::from_str(&body).unwrap())
}

/// The data of each server-sent event.
fn events(body: &str) -> Vec<String> {
    body.split("\n\n").filter_map(|event| event.strip_prefix("data: ")).map(str::to_owned).collect()
}

#[test]
fn a_reply_stops_at_each_tool_call_and_resumes_with_its_result() {
    let server = server();
    let user = json!({"role": "user", "content": "What is the weather?"});
    let (status, first) = chat(&server, json!({"model": "m", "messages": [user]}));
    assert_eq!(status, 200);
    let choice = &first["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls");
    assert_eq!(choice["message"]["content"], "Checking.");
    assert_eq!(choice["message"]["reasoning_content"], "Need the tool.");
    let call = &choice["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "get_weather");
    assert_eq!(
        serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap()["city"],
        "Tokyo"
    );

    let messages = json!([user, choice["message"],
        {"role": "tool", "tool_call_id": call["id"], "content": "sunny"}]);
    let (_, second) = chat(&server, json!({"model": "m", "messages": messages}));
    assert_eq!(second["choices"][0]["finish_reason"], "stop");
    assert_eq!(second["choices"][0]["message"]["content"], "Sunny in Tokyo.");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn a_streamed_reply_arrives_as_chunks_then_done() {
    let server = server();
    let body = json!({"model": "m", "stream": true, "stream_options": {"include_usage": true},
        "messages": [{"role": "user", "content": [{"type": "text", "text": "What is the weather?"}]}]});
    let (status, body) = http(&server, "POST", "/v1/chat/completions", Some(&body));
    assert_eq!(status, 200);
    let events = events(&body);
    assert_eq!(events.last().map(String::as_str), Some("[DONE]"));
    let chunks: Vec<Value> =
        events[..events.len() - 1].iter().map(|data| serde_json::from_str(data).unwrap()).collect();
    let text: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Checking.");
    assert!(
        chunks.iter().filter(|chunk| chunk["choices"][0]["delta"]["content"].is_string()).count()
            > 1
    );
    let call = chunks.iter().find_map(|chunk| chunk["choices"][0]["delta"]["tool_calls"].get(0));
    assert_eq!(call.unwrap()["index"], 0);
    let finish = chunks.iter().find_map(|chunk| chunk["choices"][0]["finish_reason"].as_str());
    assert_eq!(finish, Some("tool_calls"));
    assert!(chunks.last().unwrap()["usage"]["total_tokens"].is_number());
}

#[test]
fn errors_come_with_their_status_and_a_used_up_rule_lets_the_next_answer() {
    let server = server();
    let body = json!({"model": "m", "messages": [{"role": "user", "content": "are you busy"}]});
    let (status, error) = chat(&server, body.clone());
    assert_eq!(status, 429);
    assert_eq!(error["error"]["message"], "slow down");
    let (status, reply) = chat(&server, body);
    assert_eq!(status, 200);
    assert_eq!(reply["choices"][0]["message"]["content"], "echo: are you busy");
}

#[test]
fn an_exit_drops_the_stream_without_done() {
    let server = server();
    let body = json!({"model": "m", "stream": true,
        "messages": [{"role": "user", "content": "broken"}]});
    let (status, body) = http(&server, "POST", "/v1/chat/completions", Some(&body));
    assert_eq!(status, 200);
    let events = events(&body);
    assert!(!events.is_empty() && !events.contains(&"[DONE]".to_owned()), "{events:?}");
}

#[test]
fn models_and_unknown_routes() {
    let server = server();
    let (status, models) = http(&server, "GET", "/v1/models", None);
    assert_eq!(status, 200);
    assert!(models.contains("mock-model"));
    assert_eq!(http(&server, "GET", "/v1/nothing", None).0, 404);
    assert!(server.base_url().ends_with("/v1"));
}
