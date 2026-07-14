//! The MCP HTTP endpoint: handshake, tool discovery, and auth. These exercise the parts
//! served entirely on the server thread (no main-loop bounce), over a loopback socket.

use log_scouter::mcp::{random_token, McpServer};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// POST a JSON body to the server and return `(status_code, body)`.
fn post(port: u16, body: &str, auth: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let auth_line = match auth {
        Some(token) => format!("Authorization: Bearer {token}\r\n"),
        None => String::new(),
    };
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         {auth_line}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();

    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status code");
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

#[test]
fn random_token_is_32_hex_chars() {
    let token = random_token();
    assert_eq!(token.len(), 32);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(token, random_token(), "tokens should differ");
}

#[test]
fn initialize_and_tools_list_without_auth() {
    let server = McpServer::start(0, None).expect("start");
    let port = server.port();

    let (status, body) = post(
        port,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        None,
    );
    assert_eq!(status, 200);
    assert!(body.contains("\"protocolVersion\""), "body: {body}");
    assert!(body.contains("log-scouter"), "body: {body}");

    let (status, body) = post(
        port,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        None,
    );
    assert_eq!(status, 200);
    // The tools advertised over MCP are exactly the assistant's tool set.
    for tool in ["list_sources", "add_filter", "search", "count_matches"] {
        assert!(body.contains(tool), "missing {tool} in {body}");
    }
}

#[test]
fn a_notification_gets_no_envelope() {
    let server = McpServer::start(0, None).expect("start");
    // No id => a notification => 202 Accepted with an empty body.
    let (status, body) = post(
        server.port(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        None,
    );
    assert_eq!(status, 202);
    assert!(body.trim().is_empty(), "body: {body}");
}

#[test]
fn auth_is_enforced_when_a_token_is_set() {
    let token = "secret-token".to_string();
    let server = McpServer::start(0, Some(token.clone())).expect("start");
    let port = server.port();
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

    // No header, or the wrong token, is rejected; the right token gets in.
    assert_eq!(post(port, init, None).0, 401);
    assert_eq!(post(port, init, Some("wrong")).0, 401);
    assert_eq!(post(port, init, Some(&token)).0, 200);
}
