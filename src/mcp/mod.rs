//! An MCP (Model Context Protocol) server, so an external agent -- Claude Code, Codex, or
//! any MCP client -- can drive log-scouter in real time from another terminal.
//!
//! The transport is streamable HTTP on `127.0.0.1`: the client POSTs JSON-RPC to `/mcp` and
//! gets a JSON reply. A background thread runs a tiny synchronous HTTP server; each
//! `tools/call` is forwarded to the main thread over a channel, where it runs through the
//! *same* `dispatch_ai_tool` path the built-in assistant uses -- so the panels update live
//! and nothing about the tool surface is duplicated. The tool schemas advertised over
//! `tools/list` are exactly `crate::ai::tools::specs()`.
//!
//! A random bearer token guards the endpoint by default: it lets one agent in and keeps
//! other local processes out. Binding to loopback plus the token is the whole trust model.

use serde_json::{json, Value};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// The protocol version we advertise when the client does not ask for one.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

/// How long an HTTP handler waits for the main thread to run a tool before giving up. The
/// main loop drains commands every frame, so this only trips if the app is wedged.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// One tool call to run on the main thread. The HTTP handler blocks on `reply` until the
/// main thread has executed it against the live `AppState`.
pub struct McpCommand {
    pub tool: String,
    pub args: Value,
    pub reply: Sender<Result<String, String>>,
}

/// Owns the HTTP server thread and the channel the main loop drains. Dropping it lets the
/// process exit; the daemon thread ends with it.
pub struct McpServer {
    rx: Receiver<McpCommand>,
    port: u16,
    token: Option<String>,
    _handle: JoinHandle<()>,
}

impl McpServer {
    /// Bind `127.0.0.1:port` (port 0 lets the OS choose) and start serving. `token`, when
    /// set, must be presented as `Authorization: Bearer <token>` on every request.
    pub fn start(port: u16, token: Option<String>) -> std::io::Result<Self> {
        let server = tiny_http::Server::http(("127.0.0.1", port))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let bound = server
            .server_addr()
            .to_ip()
            .map(|addr| addr.port())
            .unwrap_or(port);

        let (tx, rx) = mpsc::channel::<McpCommand>();
        let token_for_thread = token.clone();
        let handle = std::thread::Builder::new()
            .name("logscout-mcp".to_string())
            .spawn(move || serve(server, tx, token_for_thread))?;

        Ok(Self {
            rx,
            port: bound,
            token,
            _handle: handle,
        })
    }

    /// Take the next queued tool call, if any, without blocking the frame.
    pub fn poll(&self) -> Option<McpCommand> {
        self.rx.try_recv().ok()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}

/// Mint a random 32-hex-character bearer token.
pub fn random_token() -> String {
    let mut bytes = [0u8; 16];
    // A failure here is astronomically unlikely; fall back to a time-seeded value rather
    // than panicking the launch.
    if getrandom::getrandom(&mut bytes).is_err() {
        let nanos = std::time::UNIX_EPOCH
            .elapsed()
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn serve(server: tiny_http::Server, tx: Sender<McpCommand>, token: Option<String>) {
    for request in server.incoming_requests() {
        handle_request(request, &tx, token.as_deref());
    }
}

fn handle_request(request: tiny_http::Request, tx: &Sender<McpCommand>, token: Option<&str>) {
    if let Some(expected) = token {
        let presented = request.headers().iter().find_map(|header| {
            header
                .field
                .equiv("Authorization")
                .then(|| header.value.as_str().trim().to_string())
        });
        if presented.as_deref() != Some(format!("Bearer {expected}").as_str()) {
            respond(request, 401, r#"{"error":"unauthorized"}"#.to_string());
            return;
        }
    }

    match request.method() {
        // The one JSON-RPC endpoint. Read the body, answer, done.
        tiny_http::Method::Post => handle_post(request, tx),
        // Streamable HTTP allows a GET for a server-pushed SSE stream; we never push, so we
        // decline it (spec-allowed). DELETE ends a session, which we do not track.
        tiny_http::Method::Delete => respond(request, 200, String::new()),
        _ => respond(request, 405, String::new()),
    }
}

fn handle_post(mut request: tiny_http::Request, tx: &Sender<McpCommand>) {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        respond(
            request,
            400,
            rpc_error(Value::Null, -32700, "could not read body"),
        );
        return;
    }
    let message: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => {
            respond(request, 400, rpc_error(Value::Null, -32700, "invalid JSON"));
            return;
        }
    };

    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));

    // Notifications (no id, e.g. notifications/initialized) get an empty 202, no envelope.
    if message.get("id").is_none() {
        respond(request, 202, String::new());
        return;
    }

    let result = match method.as_str() {
        "initialize" => Ok(initialize_result(&params)),
        "tools/list" => Ok(tools_list_result()),
        "ping" => Ok(json!({})),
        "tools/call" => call_tool(&params, tx),
        other => Err((-32601, format!("unknown method: {other}"))),
    };

    let envelope = match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }).to_string(),
        Err((code, message)) => rpc_error(id, code, &message),
    };
    respond(request, 200, envelope);
}

fn call_tool(params: &Value, tx: &Sender<McpCommand>) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return Err((-32602, "tools/call needs a tool name".to_string()));
    }
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(McpCommand {
            tool: name,
            args,
            reply: reply_tx,
        })
        .is_err()
    {
        return Ok(tool_content("the app is shutting down", true));
    }
    // A tool error is reported inside the result (isError), not as a JSON-RPC error, so the
    // model sees it as a normal turn outcome.
    match reply_rx.recv_timeout(TOOL_TIMEOUT) {
        Ok(Ok(text)) => Ok(tool_content(&text, false)),
        Ok(Err(message)) => Ok(tool_content(&message, true)),
        Err(_) => Ok(tool_content("timed out waiting for the app", true)),
    }
}

fn initialize_result(params: &Value) -> Value {
    let protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_PROTOCOL);
    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "log-scouter", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tools_list_result() -> Value {
    let tools: Vec<Value> = crate::ai::tools::specs()
        .into_iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "inputSchema": spec.parameters,
            })
        })
        .collect();
    json!({ "tools": tools })
}

fn tool_content(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

fn rpc_error(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn respond(request: tiny_http::Request, status: u16, body: String) {
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header");
    let response = tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(response);
}
