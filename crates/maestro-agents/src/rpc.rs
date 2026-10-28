//! Request bookkeeping shared by the two JSON-RPC protocols, Codex's and ACP.

use std::collections::HashMap;

use serde_json::{Value, json};

/// Numbers outgoing requests and remembers what each was for.
pub(crate) struct Calls<P> {
    next: u64,
    pending: HashMap<u64, P>,
    /// ACP requires the `jsonrpc` member; Codex's app-server omits it.
    tagged: bool,
}

impl<P> Calls<P> {
    pub(crate) fn new(tagged: bool) -> Self {
        Self { next: 1, pending: HashMap::new(), tagged }
    }

    /// A request message, remembering `purpose` until its response arrives.
    pub(crate) fn request(&mut self, method: &str, params: Value, purpose: P) -> Value {
        let id = self.next;
        self.next += 1;
        self.pending.insert(id, purpose);
        self.tag(json!({"id": id, "method": method, "params": params}))
    }

    /// A notification, which has no response.
    pub(crate) fn notify(&self, method: &str, params: Value) -> Value {
        self.tag(json!({"method": method, "params": params}))
    }

    /// A response to one of the CLI's own requests.
    pub(crate) fn respond(&self, id: &Value, result: Value) -> Value {
        self.tag(json!({"id": id, "result": result}))
    }

    /// Refuses one of the CLI's requests that Maestro does not serve.
    pub(crate) fn refuse(&self, id: &Value) -> Value {
        self.tag(
            json!({"id": id, "error": {"code": -32601, "message": "not supported by Maestro"}}),
        )
    }

    /// If `message` answers one of ours, the purpose it was sent for.
    pub(crate) fn resolve(&mut self, message: &Value) -> Option<P> {
        if message.get("method").is_some() {
            return None;
        }
        self.pending.remove(&message.get("id")?.as_u64()?)
    }

    fn tag(&self, mut message: Value) -> Value {
        if self.tagged {
            message["jsonrpc"] = json!("2.0");
        }
        message
    }
}

/// The error text of a JSON-RPC response, if it is an error.
pub(crate) fn error(message: &Value) -> Option<String> {
    let error = message.get("error")?;
    Some(error["message"].as_str().map_or_else(|| error.to_string(), str::to_owned))
}

/// Whether `message` is a request from the CLI (it has both a method and an id).
pub(crate) fn is_request(message: &Value) -> bool {
    message.get("method").is_some() && message.get("id").is_some()
}
