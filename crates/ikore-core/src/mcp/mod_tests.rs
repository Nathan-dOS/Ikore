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
