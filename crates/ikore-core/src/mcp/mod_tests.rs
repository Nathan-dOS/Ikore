use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;
use std::thread;

use serde_json::json;

use super::*;
use crate::llm::{LlmError, LlmErrorKind};
use crate::persistence_service::{EnsureProjectForWorkspaceRequest, PersistenceService};

#[derive(Default)]
struct MemoryCredentials(Mutex<HashMap<String, String>>);

impl CredentialStore for MemoryCredentials {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError> {
        Ok(self.0.lock().unwrap().get(credential_ref).cloned())
    }
    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        if secret.is_empty() {
            return Err(LlmError::new(LlmErrorKind::InvalidRequest, "empty"));
        }
        self.0
            .lock()
            .unwrap()
            .insert(credential_ref.into(), secret.into());
        Ok(())
    }
    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError> {
        Ok(self.0.lock().unwrap().remove(credential_ref).is_some())
    }
}

/// What the fake server lists, shared so a test can change it between connections.
type Catalog = Arc<Mutex<Vec<Value>>>;

struct Harness {
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    gateway: McpGateway,
    db_path: PathBuf,
    project: String,
    credentials: Arc<MemoryCredentials>,
    catalog: Catalog,
    launches: Arc<Mutex<Vec<McpLaunch>>>,
}

/// A server on pipes: it lists `catalog` and answers a call with its arguments.
fn fake_session(catalog: Catalog, timeout: Duration) -> McpSession {
    let (client_reader, mut server_writer) = std::io::pipe().unwrap();
    let (server_reader, client_writer) = std::io::pipe().unwrap();
    thread::spawn(move || {
        for line in BufReader::new(server_reader).lines() {
            let Ok(line) = line else { break };
            let message: Value = serde_json::from_str(&line).unwrap();
            let result = match message["method"].as_str() {
                Some("initialize") => json!({"protocolVersion": client::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}),
                Some("tools/list") => json!({"tools": catalog.lock().unwrap().clone()}),
                Some("tools/call") => json!({"content": [{"type": "text",
                    "text": format!("{} {}", message["params"]["name"], message["params"]["arguments"])}]}),
                _ => continue,
            };
            let reply = json!({"jsonrpc": "2.0", "id": message["id"], "result": result});
            if writeln!(server_writer, "{reply}").is_err() {
                return;
            }
        }
    });
    McpSession::over(client_reader, client_writer, timeout)
}

fn tool(name: &str, read_only: bool, schema: Value) -> Value {
    json!({"name": name, "description": format!("The {name} tool"), "inputSchema": schema,
        "annotations": {"readOnlyHint": read_only}})
}

impl Harness {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let persistence = PersistenceService::initialize(data.path()).unwrap();
        let project = persistence
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        let db_path = persistence.database_path().to_path_buf();
        let credentials = Arc::new(MemoryCredentials::default());
        let catalog: Catalog = Arc::new(Mutex::new(vec![
            tool(
                "search",
                true,
                json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            ),
            tool("delete", false, json!({"type": "object"})),
        ]));
        let launches = Arc::new(Mutex::new(Vec::new()));
        let connector: Connector = {
            let catalog = Arc::clone(&catalog);
            let launches = Arc::clone(&launches);
            Arc::new(move |launch: &McpLaunch, timeout| {
                launches.lock().unwrap().push(launch.clone());
                Ok(fake_session(Arc::clone(&catalog), timeout))
            })
        };
        let gateway = McpGateway::new(
            db_path.clone(),
            credentials.clone() as Arc<dyn CredentialStore>,
        )
        .with_connector(connector);
        Self {
            _data: data,
            _workspace: workspace,
            gateway,
            db_path,
            project,
            credentials,
            catalog,
            launches,
        }
    }

    fn save(&self, name: &str, project: Option<&str>) -> String {
        self.gateway
            .save_server(SaveMcpServerRequest {
                id: None,
                project_id: project.map(str::to_owned),
                name: name.into(),
                command: "docs-server".into(),
                args: vec!["--stdio".into()],
                env: vec![("DOCS_TOKEN".into(), Some("secret-value".into()))],
                enabled: true,
                expected_version: None,
            })
            .unwrap()
    }

    fn uses(&self) -> Vec<(String, String, Option<String>, Option<String>)> {
        Connection::open(&self.db_path)
            .unwrap()
            .prepare("SELECT capability,outcome,status,grant_id FROM capability_uses ORDER BY used_at,rowid")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

/// ADR-043: the capability ids are closed and round-trip.
#[test]
fn capabilities_and_subjects_have_closed_ids() {
    for capability in [
        Capability::Read,
        Capability::Write,
        Capability::Process,
        Capability::Network,
        Capability::mcp("docs", "search"),
        Capability::Skill("review".into()),
    ] {
        assert_eq!(Capability::parse(&capability.id()), Some(capability));
    }
    assert_eq!(Capability::parse("mcp:docs"), None);
    assert_eq!(Capability::parse("mcp:/search"), None);
    assert_eq!(Capability::parse("shell"), None);
    assert_eq!(Subject::Companion.id(), "companion");
    assert!(Subject::Companion.accepts_grants());
    assert!(!Subject::Role("backend_agent".into()).accepts_grants());
    assert!(!Subject::Slot("slot".into()).accepts_grants());
}

/// A saved server keeps its variables' values out of the database and runs
/// nothing until it is connected; connecting stores its catalog.
#[test]
fn a_server_runs_only_when_connected_and_keeps_its_values_out_of_the_database() {
    let harness = Harness::new();
    let id = harness.save("docs", Some(&harness.project));
    let servers = harness.gateway.servers(&harness.project).unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].env_names, ["DOCS_TOKEN"]);
    assert_eq!(servers[0].connected_at, None);
    assert!(servers[0].tools.is_empty());
    assert!(harness.launches.lock().unwrap().is_empty());
    let row: String = Connection::open(&harness.db_path)
        .unwrap()
        .query_row(
            "SELECT name||command||args_json||env_names_json FROM mcp_servers",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!row.contains("secret-value"));
    assert_eq!(
        harness
            .credentials
            .get(&format!("mcp:{id}:DOCS_TOKEN"))
            .unwrap()
            .as_deref(),
        Some("secret-value")
    );
    // A bad name or variable is refused.
    let bad = harness.gateway.save_server(SaveMcpServerRequest {
        id: None,
        project_id: None,
        name: "has space".into(),
        command: "x".into(),
        args: vec![],
        env: vec![],
        enabled: true,
        expected_version: None,
    });
    assert!(matches!(bad, Err(McpGatewayError::Validation(_))));

    harness.gateway.connect(&id).unwrap();
    let launches = harness.launches.lock().unwrap().clone();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].command, "docs-server");
    assert_eq!(
        launches[0].env,
        [("DOCS_TOKEN".to_owned(), "secret-value".to_owned())]
    );
    let server = &harness.gateway.servers(&harness.project).unwrap()[0];
    assert!(server.connected_at.is_some());
    let tools: Vec<_> = server
        .tools
        .iter()
        .map(|tool| {
            (
                tool.name.as_str(),
                tool.read_only,
                tool.granted_to_companion,
            )
        })
        .collect();
    assert_eq!(tools, [("delete", false, false), ("search", true, false)]);

    harness.gateway.delete_server(&id).unwrap();
    assert!(
        harness
            .gateway
            .servers(&harness.project)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        harness
            .credentials
            .get(&format!("mcp:{id}:DOCS_TOKEN"))
            .unwrap(),
        None
    );
}

/// The Companion calls only a read-only tool the developer granted in the
/// project; each use, refused or allowed, is recorded with its grant.
#[test]
fn the_companion_calls_only_granted_read_only_tools() {
    let harness = Harness::new();
    let id = harness.save("docs", Some(&harness.project));
    harness.gateway.connect(&id).unwrap();
    assert!(
        harness
            .gateway
            .companion_tools(&harness.project)
            .unwrap()
            .is_empty()
    );
    let refused = harness.gateway.call_for_companion(
        &harness.project,
        "docs",
        "search",
        json!({"q": "pipes"}),
    );
    assert!(matches!(refused, Err(McpGatewayError::Refused(_))));
    // A tool the server does not call read-only cannot be granted.
    assert!(matches!(
        harness
            .gateway
            .set_companion_grant(&harness.project, &id, "delete", true),
        Err(McpGatewayError::Refused(_))
    ));

    harness
        .gateway
        .set_companion_grant(&harness.project, &id, "search", true)
        .unwrap();
    let granted = harness.gateway.companion_tools(&harness.project).unwrap();
    assert_eq!(granted.len(), 1);
    assert_eq!(
        (granted[0].server.as_str(), granted[0].tool.as_str()),
        ("docs", "search")
    );
    assert_eq!(granted[0].input_schema["properties"]["q"]["type"], "string");
    let output = harness
        .gateway
        .call_for_companion(&harness.project, "docs", "search", json!({"q": "pipes"}))
        .unwrap();
    assert_eq!(output.text, r#""search" {"q":"pipes"}"#);
    // Another project holds no grant.
    assert!(matches!(
        harness
            .gateway
            .call_for_companion("another-project", "docs", "search", json!({})),
        Err(McpGatewayError::Refused(_)) | Err(McpGatewayError::Storage(_))
    ));

    harness
        .gateway
        .set_companion_grant(&harness.project, &id, "search", false)
        .unwrap();
    assert!(matches!(
        harness
            .gateway
            .call_for_companion(&harness.project, "docs", "search", json!({})),
        Err(McpGatewayError::Refused(_))
    ));
    let uses = harness.uses();
    let outcomes: Vec<_> = uses
        .iter()
        .map(|(capability, outcome, status, grant)| {
            (
                capability.as_str(),
                outcome.as_str(),
                status.as_deref(),
                grant.is_some(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("mcp:docs/search", "refused", None, false),
            ("mcp:docs/search", "allowed", Some("completed"), true),
            ("mcp:docs/search", "refused", None, false),
        ]
    );
}

/// A grant covers the tool the developer saw: a changed schema, or a tool
/// no longer read-only, needs a new grant.
#[test]
fn a_changed_tool_loses_its_grant() {
    let harness = Harness::new();
    let id = harness.save("docs", Some(&harness.project));
    harness.gateway.connect(&id).unwrap();
    harness
        .gateway
        .set_companion_grant(&harness.project, &id, "search", true)
        .unwrap();
    harness.catalog.lock().unwrap()[0] = tool(
        "search",
        true,
        json!({"type": "object", "properties": {"query": {"type": "string"}}}),
    );
    harness.gateway.connect(&id).unwrap();
    assert!(
        harness
            .gateway
            .companion_tools(&harness.project)
            .unwrap()
            .is_empty()
    );
    let server = &harness.gateway.servers(&harness.project).unwrap()[0];
    assert!(server.tools.iter().all(|tool| !tool.granted_to_companion));
    // Granted again, then the server stops calling it read-only.
    harness
        .gateway
        .set_companion_grant(&harness.project, &id, "search", true)
        .unwrap();
    harness.catalog.lock().unwrap()[0]["annotations"]["readOnlyHint"] = json!(false);
    harness.gateway.connect(&id).unwrap();
    assert!(
        harness
            .gateway
            .companion_tools(&harness.project)
            .unwrap()
            .is_empty()
    );
}

/// A project server hides the global server of its name, grants included.
#[test]
fn a_project_server_hides_the_global_one_of_its_name() {
    let harness = Harness::new();
    let global = harness.save("docs", None);
    harness.gateway.connect(&global).unwrap();
    harness
        .gateway
        .set_companion_grant(&harness.project, &global, "search", true)
        .unwrap();
    assert_eq!(
        harness
            .gateway
            .companion_tools(&harness.project)
            .unwrap()
            .len(),
        1
    );
    let local = harness.save("docs", Some(&harness.project));
    let servers = harness.gateway.servers(&harness.project).unwrap();
    let flags: Vec<_> = servers
        .iter()
        .map(|server| (server.id == local, server.overridden))
        .collect();
    assert_eq!(flags, [(true, false), (false, true)]);
    assert!(
        harness
            .gateway
            .companion_tools(&harness.project)
            .unwrap()
            .is_empty()
    );
}

// REV-0002 C004: diagnostics may keep category/codes, never a server's free text
// or a credential-store message, including opaque values outside the detector.
const C004_OPAQUE: &str = "C004OpaqueValue-9bW4nXq7";
const C004_SHAPED: &str = "sk-proj-syntheticc004abcdefghijklmnopqrstuv";

fn c004_assert_hidden(text: &str) {
    assert!(
        !text.contains(C004_OPAQUE) && !text.contains(C004_SHAPED),
        "a fixture credential reached observable MCP diagnostics"
    );
}

fn c004_error_session(
    catalog: Catalog,
    timeout: Duration,
    failing_method: Option<&'static str>,
    message: String,
) -> McpSession {
    let (client_reader, mut server_writer) = std::io::pipe().unwrap();
    let (server_reader, client_writer) = std::io::pipe().unwrap();
    thread::spawn(move || {
        for line in BufReader::new(server_reader).lines() {
            let Ok(line) = line else { break };
            let request: Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str();
            let reply = if method == failing_method && failing_method.is_some() {
                json!({"jsonrpc":"2.0", "id":request["id"],
                    "error":{"code":-32004,"message":message}})
            } else {
                let result = match method {
                    Some("initialize") => json!({"protocolVersion":client::PROTOCOL_VERSION,
                        "capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
                    Some("tools/list") => json!({"tools":catalog.lock().unwrap().clone()}),
                    Some("tools/call") => {
                        json!({"content":[{"type":"text","text":"fixture answer"}]})
                    }
                    _ => continue,
                };
                json!({"jsonrpc":"2.0","id":request["id"],"result":result})
            };
            if writeln!(server_writer, "{reply}").is_err() {
                return;
            }
        }
    });
    McpSession::over(client_reader, client_writer, timeout)
}

#[test]
fn mcp_error_messages_are_safe_before_storage_and_settings() {
    for failing_method in ["initialize", "tools/list", "tools/call"] {
        let mut harness = Harness::new();
        let id = harness.save("docs", Some(&harness.project));
        harness
            .credentials
            .set(&credential_ref(&id, "DOCS_TOKEN"), C004_OPAQUE)
            .unwrap();
        let fault = Arc::new(Mutex::new(None));
        let connector = {
            let catalog = Arc::clone(&harness.catalog);
            let fault = Arc::clone(&fault);
            Arc::new(move |launch: &McpLaunch, timeout| {
                // Use the actual value supplied to this launch. No real store or
                // process is involved; both directions are anonymous pipes.
                let echoed = launch
                    .env
                    .iter()
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                Ok(c004_error_session(
                    Arc::clone(&catalog),
                    timeout,
                    *fault.lock().unwrap(),
                    format!("upstream rejected {echoed} and {C004_SHAPED}"),
                ))
            }) as Connector
        };
        harness.gateway = harness.gateway.with_connector(connector);
        let error = if failing_method == "tools/call" {
            harness.gateway.connect(&id).unwrap();
            harness
                .gateway
                .set_companion_grant(&harness.project, &id, "search", true)
                .unwrap();
            *fault.lock().unwrap() = Some(failing_method);
            harness
                .gateway
                .call_for_companion(&harness.project, "docs", "search", json!({"q":"fixture"}))
                .unwrap_err()
        } else {
            *fault.lock().unwrap() = Some(failing_method);
            harness.gateway.connect(&id).unwrap_err()
        };
        assert!(matches!(
            &error,
            McpGatewayError::Server(McpError::Server { code: -32004, .. })
        ));
        c004_assert_hidden(&error.to_string());
        c004_assert_hidden(&format!("{error:?}"));
        assert!(
            error.to_string().contains("-32004"),
            "the RPC code remains useful"
        );
        let db = Connection::open(&harness.db_path).unwrap();
        if failing_method == "tools/call" {
            let (stored, status, grant): (String, String, Option<String>) = db
                .query_row(
                    "SELECT error,status,grant_id FROM capability_uses ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            c004_assert_hidden(&stored);
            assert!(stored.contains("-32004"));
            assert_eq!(status, "failed");
            assert!(grant.is_some());
            assert_eq!(
                harness
                    .gateway
                    .companion_tools(&harness.project)
                    .unwrap()
                    .len(),
                1
            );
        } else {
            let stored: String = db
                .query_row(
                    "SELECT last_error FROM mcp_servers WHERE id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .unwrap();
            c004_assert_hidden(&stored);
            assert!(stored.contains("-32004"));
            // Settings reloads this exact projection, also after reopening.
            let reopened = McpGateway::new(harness.db_path.clone(), harness.credentials.clone());
            let view = reopened.servers(&harness.project).unwrap();
            let visible = view[0].last_error.as_deref().unwrap();
            c004_assert_hidden(visible);
            assert!(visible.contains("-32004"));
            c004_assert_hidden(&format!("{:?}", view[0]));
        }
    }
}

struct C004FailingCredentials;

impl CredentialStore for C004FailingCredentials {
    fn get(&self, _: &str) -> Result<Option<String>, LlmError> {
        Err(LlmError::new(
            LlmErrorKind::Persistence,
            format!("store rejected {C004_OPAQUE} and {C004_SHAPED}"),
        ))
    }
    fn set(&self, _: &str, _: &str) -> Result<(), LlmError> {
        Err(LlmError::new(
            LlmErrorKind::Persistence,
            format!("store rejected {C004_OPAQUE} and {C004_SHAPED}"),
        ))
    }
    fn delete(&self, _: &str) -> Result<bool, LlmError> {
        Ok(false)
    }
}

#[test]
fn mcp_credential_failures_keep_the_category_without_store_text() {
    let harness = Harness::new();
    let id = harness.save("docs", Some(&harness.project));
    let gateway = McpGateway::new(harness.db_path.clone(), Arc::new(C004FailingCredentials));
    let read_error = gateway.connect(&id).unwrap_err();
    let write_error = gateway
        .save_server(SaveMcpServerRequest {
            id: None,
            project_id: Some(harness.project.clone()),
            name: "other".into(),
            command: "unused-fixture".into(),
            args: vec![],
            env: vec![("DOCS_TOKEN".into(), Some(C004_OPAQUE.into()))],
            enabled: true,
            expected_version: None,
        })
        .unwrap_err();
    for error in [read_error, write_error] {
        assert!(matches!(&error, McpGatewayError::Credentials(_)));
        c004_assert_hidden(&error.to_string());
        c004_assert_hidden(&format!("{error:?}"));
        assert!(error.to_string().contains("db_error"));
    }
}

#[test]
fn mcp_diagnostic_formatting_omits_free_text_including_error_sources() {
    let message = format!("untrusted {C004_OPAQUE} {C004_SHAPED}");
    let errors = [
        McpGatewayError::Server(McpError::Spawn(message.clone())),
        McpGatewayError::Server(McpError::Protocol(message.clone())),
        McpGatewayError::Server(McpError::Server {
            code: -32602,
            message: message.clone(),
        }),
        McpGatewayError::Credentials(message.clone()),
        McpGatewayError::Storage(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(message),
        )),
    ];
    for error in errors {
        let mut source: Option<&dyn std::error::Error> = Some(&error);
        while let Some(current) = source {
            c004_assert_hidden(&current.to_string());
            c004_assert_hidden(&format!("{current:?}"));
            source = current.source();
        }
    }
}

#[test]
fn mcp_request_and_launch_debug_keep_environment_names_without_values() {
    let request = SaveMcpServerRequest {
        id: None,
        project_id: Some("fixture-project".into()),
        name: "docs".into(),
        command: format!("fixture-server-{C004_OPAQUE}"),
        args: vec![format!("--credential={C004_SHAPED}"), C004_OPAQUE.into()],
        env: vec![
            ("DOCS_TOKEN".into(), Some(C004_OPAQUE.into())),
            ("AI_TOKEN".into(), Some(C004_SHAPED.into())),
            ("KEEP_TOKEN".into(), None),
        ],
        enabled: true,
        expected_version: None,
    };
    let launch = McpLaunch {
        command: request.command.clone(),
        args: request.args.clone(),
        env: vec![
            ("DOCS_TOKEN".into(), C004_OPAQUE.into()),
            ("AI_TOKEN".into(), C004_SHAPED.into()),
        ],
        working_dir: None,
    };
    for debug in [format!("{request:?}"), format!("{launch:?}")] {
        c004_assert_hidden(&debug);
        assert!(debug.contains("DOCS_TOKEN") && debug.contains("AI_TOKEN"));
    }
}

#[test]
fn mcp_settings_reload_omits_unqualified_legacy_error_text() {
    let harness = Harness::new();
    let id = harness.save("docs", Some(&harness.project));
    let db = Connection::open(&harness.db_path).unwrap();
    let reopened = McpGateway::new(harness.db_path.clone(), harness.credentials.clone());
    let omitted = "The previous MCP error is unavailable in a safe format (mcp_legacy_error). Test the connection again.";
    let check_projection = |historical: &str, expected: &str| {
        db.execute(
            "UPDATE mcp_servers SET last_error=?2 WHERE id=?1",
            params![id, historical],
        )
        .unwrap();
        let view = reopened.servers(&harness.project).unwrap();
        let visible = view[0].last_error.as_deref().unwrap();
        c004_assert_hidden(visible);
        c004_assert_hidden(&format!("{:?}", view[0]));
        assert_eq!(visible, expected);
        // Requalification is a read projection; never rewrite the stored row.
        let stored: String = db
            .query_row(
                "SELECT last_error FROM mcp_servers WHERE id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, historical);
    };
    check_projection(
        &format!("old upstream message {C004_OPAQUE} {C004_SHAPED}"),
        omitted,
    );

    // Every closed non-RPC category must survive a Settings reload verbatim.
    for safe in [
        "The MCP server could not start (mcp_spawn_failed). Check the configured command and environment.",
        "The MCP server did not answer in time (mcp_timeout).",
        "The MCP server closed the connection (mcp_closed).",
        "The MCP server sent a message over the size limit (mcp_message_too_large).",
        "The MCP server broke the protocol (mcp_protocol_error).",
        "The MCP server broke the protocol (mcp_tools_list_invalid).",
        "The MCP server broke the protocol (mcp_tool_name_missing).",
        "The MCP server broke the protocol (mcp_catalog_page_limit).",
        "The MCP server broke the protocol (mcp_invalid_json).",
    ] {
        check_projection(safe, safe);
    }
    for code in [i64::MIN, -32004, 0, i64::MAX] {
        let safe = format!("The MCP server answered with an error (mcp_server_error, RPC {code}).");
        check_projection(&safe, &safe);
    }
    // Accepted numeric spellings are projected back to canonical i64 text.
    let canonical = "The MCP server answered with an error (mcp_server_error, RPC 7).";
    check_projection(
        "The MCP server answered with an error (mcp_server_error, RPC +7).",
        canonical,
    );
    check_projection(
        "The MCP server answered with an error (mcp_server_error, RPC 0007).",
        canonical,
    );

    for invalid in [
        String::new(),
        format!("{canonical} {C004_OPAQUE}"),
        format!("{C004_OPAQUE} {canonical}"),
        format!("The MCP server answered with an error (mcp_server_error, RPC {C004_OPAQUE})."),
        "The MCP server answered with an error (mcp_server_error, RPC 9223372036854775808).".into(),
        "The MCP server answered with an error (mcp_server_error, RPC -9223372036854775809)."
            .into(),
        "The MCP server answered with an error (mcp_server_error, RPC 7.5).".into(),
        format!("The MCP server broke the protocol (mcp_invalid_json). {C004_SHAPED}"),
    ] {
        check_projection(&invalid, omitted);
    }
}
