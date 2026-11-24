use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::*;

type Received = Arc<Mutex<Vec<Value>>>;

/// A server at the far end of two pipes. It keeps every message it receives
/// and writes what `answer` returns for each.
fn fake_server(
    timeout: Duration,
    answer: impl Fn(&Value) -> Vec<Value> + Send + 'static,
) -> (McpSession, Received) {
    let (client_reader, mut server_writer) = std::io::pipe().unwrap();
    let (server_reader, client_writer) = std::io::pipe().unwrap();
    let received = Received::default();
    let kept = Arc::clone(&received);
    thread::spawn(move || {
        for line in BufReader::new(server_reader).lines() {
            let Ok(line) = line else { break };
            let message: Value = serde_json::from_str(&line).unwrap();
            kept.lock().unwrap().push(message.clone());
            for reply in answer(&message) {
                if writeln!(server_writer, "{reply}").is_err() {
                    return;
                }
            }
            let _ = server_writer.flush();
        }
    });
    (
        McpSession::over(client_reader, client_writer, timeout),
        received,
    )
}

fn result(message: &Value, result: Value) -> Vec<Value> {
    vec![json!({"jsonrpc": "2.0", "id": message["id"], "result": result})]
}

/// ADR-043: the handshake, a tool list across pages with the read-only hint,
/// and a call whose text comes back while other content is only named.
#[test]
fn a_session_lists_tools_across_pages_and_calls_one() {
    let (mut session, received) = fake_server(Duration::from_secs(5), |message| {
        match message["method"].as_str() {
            Some("initialize") => result(
                message,
                json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {"tools": {}},
                    "serverInfo": {"name": "fake", "version": "1"}}),
            ),
            Some("tools/list") if message["params"].get("cursor").is_none() => result(
                message,
                json!({"tools": [{"name": "search", "description": "Search the docs",
                    "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
                    "annotations": {"readOnlyHint": true}}], "nextCursor": "page-2"}),
            ),
            Some("tools/list") => result(
                message,
                json!({"tools": [{"name": "delete", "inputSchema": {"type": "object"}}]}),
            ),
            Some("tools/call") => result(
                message,
                json!({"content": [
                    {"type": "text", "text": format!("found {}", message["params"]["arguments"]["q"].as_str().unwrap())},
                    {"type": "image", "data": "AAAA", "mimeType": "image/png"}
                ]}),
            ),
            _ => Vec::new(),
        }
    });
    session.initialize().unwrap();
    let tools = session.list_tools().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "search");
    assert!(tools[0].read_only);
    assert_eq!(tools[0].input_schema["properties"]["q"]["type"], "string");
    assert!(!tools[1].read_only);
    assert_eq!(tools[1].description, "");
    let output = session.call_tool("search", json!({"q": "pipes"})).unwrap();
    assert_eq!(output.text, "found pipes\n[image content omitted]");
    assert!(!output.is_error);
    let methods: Vec<_> = received
        .lock()
        .unwrap()
        .iter()
        .filter_map(|message| message["method"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list",
            "tools/call"
        ]
    );
}

/// The server's own requests: a ping is answered and anything else refused,
/// while the host still gets its answer. A tool error is a result; a server
/// error is an error.
#[test]
fn server_requests_and_errors() {
    let (mut session, received) = fake_server(Duration::from_secs(5), |message| {
        match (
            message["method"].as_str(),
            message["params"]["name"].as_str(),
        ) {
            (Some("tools/call"), Some("broken")) => vec![json!({"jsonrpc": "2.0",
                "id": message["id"], "error": {"code": -32602, "message": "bad arguments"}})],
            (Some("tools/call"), _) => vec![
                json!({"jsonrpc": "2.0", "id": "s1", "method": "ping"}),
                json!({"jsonrpc": "2.0", "id": "s2", "method": "sampling/createMessage", "params": {}}),
                json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {}}),
                json!({"jsonrpc": "2.0", "id": message["id"],
                    "result": {"content": [{"type": "text", "text": "no such page"}], "isError": true}}),
            ],
            _ => Vec::new(),
        }
    });
    let output = session.call_tool("read", json!({})).unwrap();
    assert_eq!(
        output,
        McpToolOutput {
            text: "no such page".into(),
            is_error: true
        }
    );
    // The server reads in order: once it answers this call, it has read the
    // host's answers to its own requests.
    assert_eq!(
        session.call_tool("broken", json!({})),
        Err(McpError::Server {
            code: -32602,
            message: "Server error details omitted.".into()
        })
    );
    let answers: Vec<Value> = received
        .lock()
        .unwrap()
        .iter()
        .filter(|message| message.get("method").is_none())
        .cloned()
        .collect();
    assert_eq!(
        answers[0],
        json!({"jsonrpc": "2.0", "id": "s1", "result": {}})
    );
    assert_eq!(answers[1]["id"], "s2");
    assert_eq!(answers[1]["error"]["code"], -32601);
}

/// A server that does not answer times out; one that goes away is closed.
#[test]
fn silence_times_out_and_a_closed_server_ends_the_session() {
    let (mut silent, _) = fake_server(Duration::from_millis(200), |_| Vec::new());
    assert_eq!(silent.call_tool("slow", json!({})), Err(McpError::Timeout));

    let (client_reader, server_writer) = std::io::pipe().unwrap();
    let (_server_reader, client_writer) = std::io::pipe().unwrap();
    drop(server_writer);
    let mut closed = McpSession::over(client_reader, client_writer, Duration::from_secs(5));
    assert_eq!(closed.call_tool("any", json!({})), Err(McpError::Closed));
}

/// A server starts with the base variables and its own, never the rest of
/// the app's environment; a missing program is a start error.
#[test]
fn a_server_gets_only_its_declared_environment() {
    let launch = McpLaunch {
        command: "ikore-mcp-test-server-that-does-not-exist".into(),
        args: vec!["--stdio".into()],
        env: vec![("DOCS_TOKEN".into(), "secret".into())],
        working_dir: None,
    };
    let built = command(&launch);
    let names: Vec<String> = built
        .get_envs()
        .filter(|(_, value)| value.is_some())
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|name| name == "DOCS_TOKEN"));
    for name in &names {
        assert!(
            name == "DOCS_TOKEN"
                || BASE_ENVIRONMENT
                    .iter()
                    .any(|base| base.eq_ignore_ascii_case(name)),
            "{name} leaked into the server's environment"
        );
    }
    assert!(matches!(
        McpSession::spawn(&launch, Duration::from_secs(1)),
        Err(McpError::Spawn(_))
    ));
}
