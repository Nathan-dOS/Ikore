//! ADR-043: the host's MCP client over stdio. JSON-RPC 2.0, one message per
//! line. The host starts a server for each use and stops it afterwards; the
//! server gets only the environment it declares and the minimum to run.

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The protocol revision the host asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The longest message the host reads from a server.
const MAX_MESSAGE_BYTES: u64 = 4 * 1024 * 1024;

/// The most pages of tools the host reads from one server.
const MAX_TOOL_PAGES: usize = 20;

/// What a server always gets from the app's environment, besides the
/// variables it declares: enough to find programs and run on Windows.
pub const BASE_ENVIRONMENT: [&str; 9] = [
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "ComSpec",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

/// How to start one server.
#[derive(Clone, PartialEq, Eq)]
pub struct McpLaunch {
    pub command: String,
    pub args: Vec<String>,
    /// The variables the developer declared, with their values.
    pub env: Vec<(String, String)>,
    pub working_dir: Option<PathBuf>,
}

// Values can be opaque and outside the detector catalog. Diagnostic output
// reports only declared names/counts, never arguments or environment values.
impl fmt::Debug for McpLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<_> = self.env.iter().map(|(name, _)| name).collect();
        f.debug_struct("McpLaunch")
            .field("argument_count", &self.args.len())
            .field("environment_names", &names)
            .field("working_dir_set", &self.working_dir.is_some())
            .finish()
    }
}

/// One tool a server lists.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// The server says the tool only reads (`annotations.readOnlyHint`). A
    /// claim, not a proof: the developer's grant is what authorizes.
    pub read_only: bool,
}

/// What a tool call returned, as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolOutput {
    pub text: String,
    pub is_error: bool,
}

/// Typed variants remain available for local handling, but diagnostics and
/// source chains never expose another process's free-form text.
#[derive(Clone, PartialEq, Eq)]
pub enum McpError {
    Spawn(String),
    Timeout,
    Closed,
    TooLarge,
    Protocol(String),
    Server { code: i64, message: String },
}

impl McpError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "mcp_spawn_failed",
            Self::Timeout => "mcp_timeout",
            Self::Closed => "mcp_closed",
            Self::TooLarge => "mcp_message_too_large",
            Self::Protocol(reason) => match reason.as_str() {
                "tools/list without tools" => "mcp_tools_list_invalid",
                "a tool without a name" => "mcp_tool_name_missing",
                "too many pages of tools" => "mcp_catalog_page_limit",
                "a message that is not JSON" => "mcp_invalid_json",
                _ => "mcp_protocol_error",
            },
            Self::Server { .. } => "mcp_server_error",
        }
    }

    pub fn rpc_code(&self) -> Option<i64> {
        match self {
            Self::Server { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// SQLite can contain error text written by an older build. Only a closed
    /// grammar of diagnostics produced here qualifies for a Settings reload.
    /// Requalification never rewrites the operational/historical row.
    pub(crate) fn stored_message(text: &str) -> String {
        if let Some(code) = text
            .strip_prefix("The MCP server answered with an error (mcp_server_error, RPC ")
            .and_then(|rest| rest.strip_suffix(")."))
            .and_then(|code| code.parse::<i64>().ok())
        {
            return Self::Server {
                code,
                message: String::new(),
            }
            .to_string();
        }
        let known = [
            Self::Spawn(String::new()),
            Self::Timeout,
            Self::Closed,
            Self::TooLarge,
            Self::Protocol(String::new()),
            Self::Protocol("tools/list without tools".into()),
            Self::Protocol("a tool without a name".into()),
            Self::Protocol("too many pages of tools".into()),
            Self::Protocol("a message that is not JSON".into()),
        ];
        if known.iter().any(|error| error.to_string() == text) {
            text.to_owned()
        } else {
            "The previous MCP error is unavailable in a safe format (mcp_legacy_error). Test the connection again.".into()
        }
    }
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(_) => write!(
                f,
                "The MCP server could not start ({}). Check the configured command and environment.",
                self.code()
            ),
            Self::Timeout => write!(
                f,
                "The MCP server did not answer in time ({}).",
                self.code()
            ),
            Self::Closed => write!(f, "The MCP server closed the connection ({}).", self.code()),
            Self::TooLarge => write!(
                f,
                "The MCP server sent a message over the size limit ({}).",
                self.code()
            ),
            Self::Protocol(_) => write!(f, "The MCP server broke the protocol ({}).", self.code()),
            Self::Server { code, .. } => write!(
                f,
                "The MCP server answered with an error ({}, RPC {code}).",
                self.code()
            ),
        }
    }
}

impl fmt::Debug for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpError")
            .field("code", &self.code())
            .field("rpc_code", &self.rpc_code())
            .finish()
    }
}

impl std::error::Error for McpError {}

/// The server's process: a cleared environment with [`BASE_ENVIRONMENT`] and
/// what `launch` declares, never the rest of the app's; no window.
fn command(launch: &McpLaunch) -> Command {
    let mut command = Command::new(&launch.command);
    command
        .args(&launch.args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for name in BASE_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for (name, value) in &launch.env {
        command.env(name, value);
    }
    if let Some(directory) = &launch.working_dir {
        command.current_dir(directory);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a console server opens no window.
        command.creation_flags(0x0800_0000);
    }
    command
}

/// One connection to a server, from `initialize` to drop.
pub struct McpSession {
    writer: Box<dyn Write + Send>,
    lines: Receiver<Result<String, McpError>>,
    next_id: u64,
    timeout: Duration,
    child: Option<Child>,
}

impl McpSession {
    /// Starts the server (see [`command`]).
    pub fn spawn(launch: &McpLaunch, timeout: Duration) -> Result<Self, McpError> {
        let mut child = command(launch)
            .spawn()
            .map_err(|error| McpError::Spawn(error.to_string()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(McpError::Spawn("no standard streams".into()));
        };
        let mut session = Self::over(stdout, stdin, timeout);
        session.child = Some(child);
        Ok(session)
    }

    /// A session over any pair of streams; tests use pipes.
    pub fn over(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        timeout: Duration,
    ) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let mut line = Vec::new();
                let read = (&mut reader)
                    .take(MAX_MESSAGE_BYTES + 1)
                    .read_until(b'\n', &mut line);
                let message = match read {
                    Ok(0) => Err(McpError::Closed),
                    Ok(_) if line.len() as u64 > MAX_MESSAGE_BYTES => Err(McpError::TooLarge),
                    Ok(_) => Ok(String::from_utf8_lossy(&line).trim().to_owned()),
                    Err(error) => Err(McpError::Protocol(error.to_string())),
                };
                let last = message.is_err();
                if sender.send(message).is_err() || last {
                    break;
                }
            }
        });
        Self {
            writer: Box::new(writer),
            lines,
            next_id: 1,
            timeout,
            child: None,
        }
    }

    /// The handshake: the host offers no capability of its own (no sampling,
    /// no roots).
    pub fn initialize(&mut self) -> Result<(), McpError> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ikore", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        self.notify("notifications/initialized", json!({}))
    }

    /// Every tool the server lists, page by page.
    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let result = self.request("tools/list", params)?;
            let page = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| McpError::Protocol("tools/list without tools".into()))?;
            for tool in page {
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| McpError::Protocol("a tool without a name".into()))?;
                tools.push(McpTool {
                    name: name.to_owned(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    input_schema: tool
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                    read_only: tool
                        .pointer("/annotations/readOnlyHint")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(McpError::Protocol("too many pages of tools".into()))
    }

    /// Calls one tool. Text content is kept; other content is named, not read.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<McpToolOutput, McpError> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        let mut parts = Vec::new();
        for item in result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match item.get("type").and_then(Value::as_str) {
                Some("text") => parts.push(
                    item.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
                Some("resource") => match item.pointer("/resource/text").and_then(Value::as_str) {
                    Some(text) => parts.push(text.to_owned()),
                    None => parts.push("[binary resource omitted]".into()),
                },
                Some(other) => parts.push(format!("[{other} content omitted]")),
                None => {}
            }
        }
        if parts.is_empty()
            && let Some(structured) = result.get("structuredContent")
        {
            parts.push(structured.to_string());
        }
        Ok(McpToolOutput {
            text: parts.join("\n"),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(McpError::Timeout)?;
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line?,
                Err(RecvTimeoutError::Timeout) => return Err(McpError::Timeout),
                Err(RecvTimeoutError::Disconnected) => return Err(McpError::Closed),
            };
            if line.is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line)
                .map_err(|_| McpError::Protocol("a message that is not JSON".into()))?;
            if let Some(request) = message.get("method").and_then(Value::as_str) {
                // A request of the server's own: the host answers a ping and
                // offers nothing else. Notifications need no answer.
                if let Some(request_id) = message.get("id") {
                    let answer = if request == "ping" {
                        json!({"jsonrpc": "2.0", "id": request_id, "result": {}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": request_id,
                            "error": {"code": -32601, "message": "Method not found"}})
                    };
                    self.send(&answer)?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(McpError::Server {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    // This process may echo a declared environment value,
                    // including opaque secrets outside the detector catalog.
                    // Keep the typed RPC code, never the untrusted body.
                    message: "Server error details omitted.".into(),
                });
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .and_then(|()| self.writer.flush())
            .map_err(|_| McpError::Closed)
    }
}

impl Drop for McpSession {
    /// No server outlives its use.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
