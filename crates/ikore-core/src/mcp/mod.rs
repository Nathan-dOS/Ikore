//! ADR-043: explicit capabilities (Qoder B1) and the host's MCP gateway
//! (Qoder B2, PI-Desktop PD12).
//!
//! The host is the MCP client; a CLI never gets an MCP server. A server runs
//! only when the developer connects it or a granted tool is called, and stops
//! right after. Each tool is the capability `mcp:<server>/<tool>`; the first
//! subject that can hold one is the Companion, for tools the server declares
//! read-only, granted one by one. Every use is recorded with its grant.

pub mod client;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::llm::CredentialStore;
use client::{McpError, McpLaunch, McpSession, McpTool, McpToolOutput};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

/// How long one request to a server may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The most text a tool result may bring back to the Companion.
pub const MAX_RESULT_CHARS: usize = 64 * 1024;

/// A typed capability (ADR-043, item 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    Read,
    Write,
    Process,
    Network,
    Mcp { server: String, tool: String },
    Skill(String),
}

impl Capability {
    pub fn mcp(server: &str, tool: &str) -> Self {
        Self::Mcp {
            server: server.to_owned(),
            tool: tool.to_owned(),
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Read => "read".into(),
            Self::Write => "write".into(),
            Self::Process => "process".into(),
            Self::Network => "network".into(),
            Self::Mcp { server, tool } => format!("mcp:{server}/{tool}"),
            Self::Skill(id) => format!("skill:{id}"),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "process" => Some(Self::Process),
            "network" => Some(Self::Network),
            _ => {
                if let Some(rest) = id.strip_prefix("mcp:") {
                    let (server, tool) = rest.split_once('/')?;
                    (!server.is_empty() && !tool.is_empty()).then(|| Self::mcp(server, tool))
                } else {
                    id.strip_prefix("skill:")
                        .filter(|skill| !skill.is_empty())
                        .map(|skill| Self::Skill(skill.to_owned()))
                }
            }
        }
    }
}

/// Who uses a capability.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    Companion,
    /// A pipeline role, by its id.
    Role(String),
    /// A free-form slot, by its id.
    Slot(String),
}

impl Subject {
    pub fn id(&self) -> String {
        match self {
            Self::Companion => "companion".into(),
            Self::Role(role) => format!("role:{role}"),
            Self::Slot(slot) => format!("slot:{slot}"),
        }
    }

    /// In this version only the Companion holds grants (ADR-043, item 2).
    pub fn accepts_grants(&self) -> bool {
        matches!(self, Self::Companion)
    }
}

/// Typed operational failures. Public diagnostics expose categories/codes,
/// not credential-store strings or arbitrary payloads from another process.
pub enum McpGatewayError {
    Validation(String),
    Refused(String),
    Server(McpError),
    Storage(rusqlite::Error),
    Credentials(String),
}

impl McpGatewayError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "mcp_validation_error",
            Self::Refused(_) => "mcp_refused",
            Self::Server(error) => error.code(),
            Self::Storage(_) => "mcp_storage_error",
            Self::Credentials(_) => "mcp_credentials_error",
        }
    }

    fn credential_category(&self) -> &'static str {
        match self {
            Self::Credentials(code) => match code.as_str() {
                "db_error" => "db_error",
                "validation_error" => "validation_error",
                "missing_api_key" => "missing_api_key",
                "sanitization_failed" => "sanitization_failed",
                _ => "credential_store_error",
            },
            _ => "credential_store_error",
        }
    }

    fn sqlite_code(&self) -> Option<i32> {
        match self {
            Self::Storage(rusqlite::Error::SqliteFailure(error, _)) => Some(error.extended_code),
            _ => None,
        }
    }
}

impl From<McpError> for McpGatewayError {
    fn from(error: McpError) -> Self {
        Self::Server(error)
    }
}

impl From<rusqlite::Error> for McpGatewayError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

impl fmt::Display for McpGatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Refused(message) => {
                f.write_str(&crate::sanitization::sanitized_observation(message))
            }
            Self::Server(error) => fmt::Display::fmt(error, f),
            Self::Storage(_) => match self.sqlite_code() {
                Some(code) => write!(f, "The MCP store failed ({}, SQLite {code}).", self.code()),
                None => write!(f, "The MCP store failed ({}).", self.code()),
            },
            Self::Credentials(_) => write!(
                f,
                "The credential store failed ({}, {}).",
                self.code(),
                self.credential_category()
            ),
        }
    }
}

impl fmt::Debug for McpGatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rpc_code = match self {
            Self::Server(error) => error.rpc_code(),
            _ => None,
        };
        f.debug_struct("McpGatewayError")
            .field("code", &self.code())
            .field("rpc_code", &rpc_code)
            .field("sqlite_code", &self.sqlite_code())
            .finish()
    }
}

impl std::error::Error for McpGatewayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Server(error) => Some(error),
            // A raw SQLite error can contain runtime values in its message.
            // Typed Storage remains available locally; diagnostic chains stop here.
            _ => None,
        }
    }
}

/// One server as the settings show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerView {
    pub id: String,
    /// `None` for a global server.
    pub project_id: Option<String>,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// The declared variables, by name; values stay in the credential store.
    pub env_names: Vec<String>,
    pub enabled: bool,
    /// When the developer first connected it. Nothing runs before.
    pub connected_at: Option<String>,
    pub last_error: Option<String>,
    pub version: u64,
    pub tools: Vec<McpToolView>,
    /// A project server of the same name hides this global one.
    pub overridden: bool,
}

/// One tool of a connected server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolView {
    pub name: String,
    pub description: String,
    pub read_only: bool,
    /// The Companion may call it in this project.
    pub granted_to_companion: bool,
}

/// What the developer saves for a server.
#[derive(Clone, PartialEq, Eq)]
pub struct SaveMcpServerRequest {
    /// `None` to create.
    pub id: Option<String>,
    /// `None` for a global server.
    pub project_id: Option<String>,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// Each variable with its new value, or `None` to keep the stored one.
    pub env: Vec<(String, Option<String>)>,
    pub enabled: bool,
    pub expected_version: Option<u64>,
}

impl fmt::Debug for SaveMcpServerRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<_> = self.env.iter().map(|(name, _)| name).collect();
        f.debug_struct("SaveMcpServerRequest")
            .field("id", &self.id)
            .field("project_id", &self.project_id)
            .field("name", &self.name)
            .field("argument_count", &self.args.len())
            .field("environment_names", &names)
            .field("enabled", &self.enabled)
            .field("expected_version", &self.expected_version)
            .finish()
    }
}

/// A tool the Companion may call, as its round lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantedTool {
    pub server: String,
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
}

/// Opens a session to a server: [`McpSession::spawn`], or a fake in tests.
type Connector = Arc<dyn Fn(&McpLaunch, Duration) -> Result<McpSession, McpError> + Send + Sync>;

/// The host's MCP gateway.
#[derive(Clone)]
pub struct McpGateway {
    db_path: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    timeout: Duration,
    connector: Connector,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn credential_ref(server_id: &str, name: &str) -> String {
    format!("mcp:{server_id}:{name}")
}

/// A server name is also part of a capability id.
fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

impl McpGateway {
    pub fn new(db_path: PathBuf, credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            db_path,
            credentials,
            timeout: REQUEST_TIMEOUT,
            connector: Arc::new(McpSession::spawn),
        }
    }

    #[cfg(test)]
    fn with_connector(mut self, connector: Connector) -> Self {
        self.connector = connector;
        self
    }

    fn connection(&self) -> Result<Connection, McpGatewayError> {
        let connection = Connection::open(&self.db_path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(connection)
    }

    /// The project's servers and the global ones; a project server hides a
    /// global server of the same name.
    pub fn servers(&self, project_id: &str) -> Result<Vec<McpServerView>, McpGatewayError> {
        let db = self.connection()?;
        let rows = db
            .prepare(
                "SELECT id,project_id,name,command,args_json,env_names_json,enabled,connected_at,last_error,version
                 FROM mcp_servers WHERE project_id IS NULL OR project_id=?1
                 ORDER BY name, project_id IS NULL",
            )?
            .query_map([project_id], |row| {
                Ok(McpServerView {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    name: row.get(2)?,
                    command: row.get(3)?,
                    args: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    env_names: serde_json::from_str(&row.get::<_, String>(5)?)
                        .unwrap_or_default(),
                    enabled: row.get(6)?,
                    connected_at: row.get(7)?,
                    last_error: row.get::<_, Option<String>>(8)?
                        .map(|text| McpError::stored_message(&text)),
                    version: row.get(9)?,
                    tools: Vec::new(),
                    overridden: false,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut servers = Vec::with_capacity(rows.len());
        for mut server in rows {
            server.overridden = server.project_id.is_none()
                && servers.iter().any(|other: &McpServerView| {
                    other.project_id.is_some() && other.name == server.name
                });
            server.tools = Self::tools_on(&db, project_id, &server.id)?;
            servers.push(server);
        }
        Ok(servers)
    }

    fn tools_on(
        db: &Connection,
        project_id: &str,
        server_id: &str,
    ) -> Result<Vec<McpToolView>, McpGatewayError> {
        Ok(db
            .prepare(
                "SELECT t.name,t.description,t.read_only,EXISTS(
                   SELECT 1 FROM capability_grants g
                   WHERE g.project_id=?1 AND g.subject='companion' AND g.server_id=t.server_id
                     AND g.capability='mcp:'||s.name||'/'||t.name
                     AND g.schema_sha256=t.schema_sha256 AND g.revoked_at IS NULL)
                 FROM mcp_tools t JOIN mcp_servers s ON s.id=t.server_id
                 WHERE t.server_id=?2 ORDER BY t.name",
            )?
            .query_map(params![project_id, server_id], |row| {
                Ok(McpToolView {
                    name: row.get(0)?,
                    description: row.get(1)?,
                    read_only: row.get(2)?,
                    granted_to_companion: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Creates or updates a server. Its variables' values go to the
    /// credential store, never to the database.
    pub fn save_server(&self, request: SaveMcpServerRequest) -> Result<String, McpGatewayError> {
        let name = request.name.trim();
        if !valid_name(name) {
            return Err(McpGatewayError::Validation(
                "Use 1 to 64 letters, digits, '-', '_' or '.' for the server name.".into(),
            ));
        }
        let command = request.command.trim();
        if command.is_empty() {
            return Err(McpGatewayError::Validation(
                "The server needs a command.".into(),
            ));
        }
        if let Some((bad, _)) = request.env.iter().find(|(name, _)| !valid_env_name(name)) {
            return Err(McpGatewayError::Validation(format!(
                "{bad} is not a valid variable name."
            )));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        let names: Vec<&str> = request.env.iter().map(|(name, _)| name.as_str()).collect();
        let args = serde_json::to_string(&request.args).unwrap_or_else(|_| "[]".into());
        let env_names = serde_json::to_string(&names).unwrap_or_else(|_| "[]".into());
        let id = match &request.id {
            Some(id) => {
                let version: Option<u64> = tx
                    .query_row("SELECT version FROM mcp_servers WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                let Some(version) = version else {
                    return Err(McpGatewayError::Validation(
                        "This server no longer exists.".into(),
                    ));
                };
                if request
                    .expected_version
                    .is_some_and(|expected| expected != version)
                {
                    return Err(McpGatewayError::Validation(
                        "The server changed meanwhile; reload and save again.".into(),
                    ));
                }
                tx.execute(
                    "UPDATE mcp_servers SET name=?2,command=?3,args_json=?4,env_names_json=?5,enabled=?6,version=version+1,updated_at=?7 WHERE id=?1",
                    params![id, name, command, args, env_names, request.enabled, now],
                )?;
                id.clone()
            }
            None => {
                let id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO mcp_servers(id,project_id,name,command,args_json,env_names_json,enabled,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)",
                    params![id, request.project_id, name, command, args, env_names, request.enabled, now],
                )
                .map_err(|error| match error {
                    rusqlite::Error::SqliteFailure(failure, _)
                        if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                    {
                        McpGatewayError::Validation(format!("A server named {name} already exists here."))
                    }
                    other => McpGatewayError::Storage(other),
                })?;
                id
            }
        };
        for (name, value) in &request.env {
            if let Some(value) = value {
                self.credentials
                    .set(&credential_ref(&id, name), value)
                    .map_err(|error| McpGatewayError::Credentials(error.kind.code().to_owned()))?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    /// Deletes a server, its tools, its grants and its stored values.
    pub fn delete_server(&self, server_id: &str) -> Result<(), McpGatewayError> {
        let db = self.connection()?;
        let names: Option<String> = db
            .query_row(
                "SELECT env_names_json FROM mcp_servers WHERE id=?1",
                [server_id],
                |row| row.get(0),
            )
            .optional()?;
        db.execute("DELETE FROM mcp_servers WHERE id=?1", [server_id])?;
        for name in names
            .and_then(|names| serde_json::from_str::<Vec<String>>(&names).ok())
            .unwrap_or_default()
        {
            let _ = self.credentials.delete(&credential_ref(server_id, &name));
        }
        Ok(())
    }

    fn launch(
        &self,
        db: &Connection,
        server_id: &str,
    ) -> Result<(String, McpLaunch), McpGatewayError> {
        let (name, command, args, names): (String, String, String, String) = db
            .query_row(
                "SELECT name,command,args_json,env_names_json FROM mcp_servers WHERE id=?1",
                [server_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| McpGatewayError::Validation("This server no longer exists.".into()))?;
        let mut env = Vec::new();
        for variable in serde_json::from_str::<Vec<String>>(&names).unwrap_or_default() {
            let value = self
                .credentials
                .get(&credential_ref(server_id, &variable))
                .map_err(|error| McpGatewayError::Credentials(error.kind.code().to_owned()))?
                .unwrap_or_default();
            env.push((variable, value));
        }
        Ok((
            name,
            McpLaunch {
                command,
                args: serde_json::from_str(&args).unwrap_or_default(),
                env,
                working_dir: None,
            },
        ))
    }

    /// "Test connection": starts the server, reads its tools, stores the
    /// catalog and stops it. A tool whose schema changed loses its grants.
    pub fn connect(&self, server_id: &str) -> Result<(), McpGatewayError> {
        let db = self.connection()?;
        let (_, launch) = self.launch(&db, server_id)?;
        drop(db);
        let listed = (self.connector)(&launch, self.timeout).and_then(|mut session| {
            session.initialize()?;
            session.list_tools()
        });
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        match listed {
            Ok(tools) => {
                Self::replace_catalog(&tx, server_id, &tools, &now)?;
                tx.execute(
                    "UPDATE mcp_servers SET connected_at=COALESCE(connected_at,?2),last_error=NULL WHERE id=?1",
                    params![server_id, now],
                )?;
                tx.commit()?;
                Ok(())
            }
            Err(error) => {
                tx.execute(
                    "UPDATE mcp_servers SET last_error=?2 WHERE id=?1",
                    params![server_id, error.to_string()],
                )?;
                tx.commit()?;
                Err(error.into())
            }
        }
    }

    fn replace_catalog(
        tx: &Transaction<'_>,
        server_id: &str,
        tools: &[McpTool],
        now: &str,
    ) -> Result<(), McpGatewayError> {
        tx.execute("DELETE FROM mcp_tools WHERE server_id=?1", [server_id])?;
        for tool in tools {
            let schema = tool.input_schema.to_string();
            tx.execute(
                "INSERT OR REPLACE INTO mcp_tools(server_id,name,description,input_schema_json,schema_sha256,read_only,listed_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![server_id, tool.name, tool.description, schema, sha256(&schema), tool.read_only, now],
            )?;
        }
        // A grant covers the tool the developer saw: a changed or vanished
        // tool, or one no longer read-only, needs a new grant.
        tx.execute(
            "UPDATE capability_grants SET revoked_at=?2 WHERE server_id=?1 AND revoked_at IS NULL AND NOT EXISTS(
               SELECT 1 FROM mcp_tools t JOIN mcp_servers s ON s.id=t.server_id
               WHERE t.server_id=capability_grants.server_id AND t.read_only=1
                 AND capability_grants.capability='mcp:'||s.name||'/'||t.name
                 AND capability_grants.schema_sha256=t.schema_sha256)",
            params![server_id, now],
        )?;
        Ok(())
    }

    /// Grants or revokes one read-only tool to the Companion in a project.
    pub fn set_companion_grant(
        &self,
        project_id: &str,
        server_id: &str,
        tool: &str,
        granted: bool,
    ) -> Result<(), McpGatewayError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now();
        let found: Option<(String, String, bool)> = tx
            .query_row(
                "SELECT s.name,t.schema_sha256,t.read_only FROM mcp_tools t JOIN mcp_servers s ON s.id=t.server_id
                 WHERE t.server_id=?1 AND t.name=?2 AND (s.project_id IS NULL OR s.project_id=?3)",
                params![server_id, tool, project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((server, schema, read_only)) = found else {
            return Err(McpGatewayError::Validation(
                "Connect the server again: this tool is not in its catalog.".into(),
            ));
        };
        let capability = Capability::mcp(&server, tool).id();
        tx.execute(
            "UPDATE capability_grants SET revoked_at=?4 WHERE project_id=?1 AND subject='companion' AND capability=?2 AND server_id=?3 AND revoked_at IS NULL",
            params![project_id, capability, server_id, now],
        )?;
        if granted {
            if !read_only {
                return Err(McpGatewayError::Refused(
                    "The Companion only gets tools the server declares read-only.".into(),
                ));
            }
            tx.execute(
                "INSERT INTO capability_grants(id,project_id,subject,capability,server_id,schema_sha256,granted_at) VALUES(?1,?2,'companion',?3,?4,?5,?6)",
                params![Uuid::new_v4().to_string(), project_id, capability, server_id, schema, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The tools the Companion may call in a project, from enabled and
    /// connected servers; a project server hides a global one of its name.
    pub fn companion_tools(&self, project_id: &str) -> Result<Vec<GrantedTool>, McpGatewayError> {
        let db = self.connection()?;
        Ok(Self::granted_on(&db, project_id)?
            .into_iter()
            .map(|(_, _, tool)| tool)
            .collect())
    }

    /// `(grant id, server id, tool)` for each tool the Companion holds.
    fn granted_on(
        db: &Connection,
        project_id: &str,
    ) -> Result<Vec<(String, String, GrantedTool)>, McpGatewayError> {
        let rows = db
            .prepare(
                "SELECT g.id,s.id,s.name,t.name,t.description,t.input_schema_json
                 FROM capability_grants g
                 JOIN mcp_servers s ON s.id=g.server_id
                 JOIN mcp_tools t ON t.server_id=s.id AND g.capability='mcp:'||s.name||'/'||t.name
                 WHERE g.project_id=?1 AND g.subject='companion' AND g.revoked_at IS NULL
                   AND g.schema_sha256=t.schema_sha256 AND t.read_only=1
                   AND s.enabled=1 AND s.connected_at IS NOT NULL
                   AND (s.project_id=?1 OR (s.project_id IS NULL AND NOT EXISTS(
                     SELECT 1 FROM mcp_servers p WHERE p.project_id=?1 AND p.name=s.name)))
                 ORDER BY s.name,t.name",
            )?
            .query_map([project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    GrantedTool {
                        server: row.get(2)?,
                        tool: row.get(3)?,
                        description: row.get(4)?,
                        input_schema: serde_json::from_str(&row.get::<_, String>(5)?)
                            .unwrap_or(Value::Null),
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Calls a tool for the Companion. The grant is checked here, right
    /// before the call (ADR-043, item 1), and the use is recorded either way.
    /// `arguments` arrive sanitized; the caller sanitizes the result.
    pub fn call_for_companion(
        &self,
        project_id: &str,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<McpToolOutput, McpGatewayError> {
        let capability = Capability::mcp(server, tool).id();
        let db = self.connection()?;
        let grant = Self::granted_on(&db, project_id)?
            .into_iter()
            .find(|(_, _, granted)| granted.server == server && granted.tool == tool);
        let started = Instant::now();
        let use_id = Uuid::new_v4().to_string();
        let recorded_arguments = arguments.to_string();
        let Some((grant_id, server_id, _)) = grant else {
            db.execute(
                "INSERT INTO capability_uses(id,project_id,subject,capability,outcome,reason,arguments_json,used_at) VALUES(?1,?2,'companion',?3,'refused','not_granted',?4,?5)",
                params![use_id, project_id, capability, recorded_arguments, now()],
            )?;
            return Err(McpGatewayError::Refused(format!(
                "{capability} is not granted to the Companion in this project."
            )));
        };
        db.execute(
            "INSERT INTO capability_uses(id,project_id,subject,capability,grant_id,outcome,arguments_json,used_at) VALUES(?1,?2,'companion',?3,?4,'allowed',?5,?6)",
            params![use_id, project_id, capability, grant_id, recorded_arguments, now()],
        )?;
        let (_, launch) = self.launch(&db, &server_id)?;
        drop(db);
        let outcome = (self.connector)(&launch, self.timeout).and_then(|mut session| {
            session.initialize()?;
            session.call_tool(tool, arguments)
        });
        let elapsed = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        let db = self.connection()?;
        match outcome {
            Ok(mut output) => {
                if output.text.chars().count() > MAX_RESULT_CHARS {
                    output.text = output.text.chars().take(MAX_RESULT_CHARS).collect();
                    output.text.push_str("\n[result truncated]");
                }
                db.execute(
                    "UPDATE capability_uses SET status=?2,result_bytes=?3,duration_ms=?4 WHERE id=?1",
                    params![
                        use_id,
                        if output.is_error { "failed" } else { "completed" },
                        i64::try_from(output.text.len()).unwrap_or(i64::MAX),
                        elapsed
                    ],
                )?;
                Ok(output)
            }
            Err(error) => {
                db.execute(
                    "UPDATE capability_uses SET status='failed',duration_ms=?2,error=?3 WHERE id=?1",
                    params![use_id, elapsed, error.to_string()],
                )?;
                Err(error.into())
            }
        }
    }
}
