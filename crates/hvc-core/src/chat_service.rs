use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::{StreamExt, stream};
use regex::Regex;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::llm::catalog::{estimate_cost_usd, model_entry};
use crate::llm::credentials::{CredentialStore, KeyringCredentialStore};
use crate::llm::domain::{ProviderProtocol, ResolvedConnection};
use crate::llm::profiles::{AuthStyle, ProviderProfile, profile_by_id};
use crate::llm::repository::SqliteLlmRepository;
use crate::llm::streaming::{ParsedStreamEvent, SseParser};
use crate::repository_context::{
    ListRepositoryTreeArgs, ReadRepositoryFileArgs, RepositoryContext, SearchRepositoryArgs,
};
use crate::secret_redactor::redact_secrets;
use crate::terminals::{
    CliBackend, CliModelSelectionSource, CompanionCliRoundRequest, TerminalError, TerminalService,
    discover_clis, validate_catalog_model_selection,
};
use crate::usage::{UsageContext, UsageTransport, api::UsageRecorder};

const MAX_MODEL_ROUNDS: usize = 8;
const MAX_TOOL_CALLS: usize = 24;
const MAX_TOOL_RESULT_BYTES: usize = 256 * 1024;
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
/// A CLI turn runs one fresh CLI process per round and re-sends every read so
/// far. A seven-round repository review took about 70 s of rounds plus two
/// repairs and hit the API's 120 s limit on its last round (2026-09-26). Each
/// round keeps its own `TURN_TIMEOUT`; the developer can cancel at any time.
const CLI_TURN_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_USER_MESSAGE_CHARS: usize = 20_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatCommandError {
    pub code: String,
    pub message: String,
}

impl ChatCommandError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn database(error: impl Display) -> Self {
        Self::new(
            "chat_persistence",
            format!("Failed to save the chat: {error}"),
        )
    }
}

impl Display for ChatCommandError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ChatCommandError {}

impl From<rusqlite::Error> for ChatCommandError {
    fn from(error: rusqlite::Error) -> Self {
        Self::database(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatEventKind {
    TurnStarted,
    ToolStarted,
    ToolCompleted,
    TokenDelta,
    ContentReset,
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
}

impl ChatEventKind {
    pub fn event_name(self) -> &'static str {
        match self {
            Self::TurnStarted => "chat://turn-started",
            Self::ToolStarted => "chat://tool-started",
            Self::ToolCompleted => "chat://tool-completed",
            Self::TokenDelta => "chat://token-delta",
            Self::ContentReset => "chat://content-reset",
            Self::TurnCompleted => "chat://turn-completed",
            Self::TurnFailed => "chat://turn-failed",
            Self::TurnCancelled => "chat://turn-cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEventError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolStarted {
    pub id: String,
    pub name: String,
    pub arguments: ChatToolArguments,
    pub round_index: usize,
}

pub type ChatToolArguments = BTreeMap<String, ChatToolArgumentValue>;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ChatToolArgumentValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolCompleted {
    pub id: String,
    pub name: String,
    pub status: String,
    pub summary: String,
    pub result_bytes: usize,
    pub round_index: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ChatEventDetail {
    Empty {},
    ContentReset {
        reason: String,
    },
    TokenDelta {
        delta: String,
    },
    Failed {
        error: ChatEventError,
    },
    ToolStarted {
        #[serde(rename = "toolCall")]
        tool_call: ChatToolStarted,
    },
    ToolCompleted {
        #[serde(rename = "toolCall")]
        tool_call: ChatToolCompleted,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEventEnvelope {
    #[serde(skip)]
    pub kind: ChatEventKind,
    pub schema_version: u8,
    pub sequence: u64,
    pub occurred_at: String,
    pub conversation_id: String,
    pub assistant_message_id: String,
    pub execution_id: String,
    pub generation_id: String,
    pub detail: ChatEventDetail,
}

impl ChatEventEnvelope {
    pub fn event_name(&self) -> &'static str {
        self.kind.event_name()
    }
}

pub trait ChatEventSink: Send + Sync {
    fn emit(&self, envelope: &ChatEventEnvelope);
}

#[derive(Default)]
pub struct NoopChatEventSink;

impl ChatEventSink for NoopChatEventSink {
    fn emit(&self, _envelope: &ChatEventEnvelope) {}
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConversation {
    pub id: String,
    pub project_id: String,
    pub connection_id: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    pub transport: ChatTransportKind,
    pub cli_backend: Option<CliBackend>,
    pub model_profile: Option<String>,
    pub model_source: CliModelSelectionSource,
    pub title: String,
    pub connection_available: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatTransportKind {
    #[default]
    Api,
    Cli,
}

impl ChatTransportKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Cli => "cli",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "api" => Some(Self::Api),
            "cli" => Some(Self::Cli),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum ChatExecutionTargetRequest {
    Api {
        connection_id: String,
    },
    Cli {
        backend: CliBackend,
        model: Option<String>,
        model_profile: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub ordinal: i64,
    pub role: String,
    pub content: String,
    pub status: String,
    pub execution_id: Option<String>,
    pub generation_id: Option<String>,
    pub last_sequence: u64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub estimated_cost_usd: Option<f64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    /// Frozen at send time; older turns are reconstructed from their first model call.
    pub target_label: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatToolCall {
    pub id: String,
    pub assistant_message_id: String,
    pub round_index: usize,
    pub tool_call_id: String,
    pub tool_name: String,
    pub sanitized_arguments: ChatToolArguments,
    pub result_summary: Option<String>,
    pub result_bytes: usize,
    pub status: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListChatConversationsRequest {
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListChatConversationsResponse {
    pub conversations: Vec<ChatConversation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatConversationRequest {
    pub project_id: String,
    pub target: ChatExecutionTargetRequest,
}

/// Retargets an existing conversation (ADR-014 amendment, 2026-09-22): the
/// Companion panel applies to the open conversation from its next message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateChatConversationTargetRequest {
    pub conversation_id: String,
    pub target: ChatExecutionTargetRequest,
}

/// The validated columns a target resolves to.
struct ResolvedTarget {
    connection_id: Option<String>,
    provider_id: String,
    model_id: String,
    transport: ChatTransportKind,
    cli_backend: Option<CliBackend>,
    model_profile: Option<String>,
    model_source: CliModelSelectionSource,
}

/// `runtime_settings` key of a project's last chosen Companion target.
pub fn companion_target_setting_key(project_id: &str) -> String {
    format!("companion.target.{project_id}")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateChatConversationResponse {
    pub conversation: ChatConversation,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetChatConversationRequest {
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetChatConversationResponse {
    pub conversation: ChatConversation,
    pub messages: Vec<ChatMessage>,
    pub tool_calls: Vec<ChatToolCall>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteChatConversationRequest {
    pub conversation_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteChatConversationResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendChatMessageRequest {
    pub conversation_id: String,
    pub content: String,
    /// The panel target the developer saw when pressing Send.
    #[serde(default)]
    pub expected_target: Option<ChatExecutionTargetRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendChatMessageResponse {
    pub execution_id: String,
    pub generation_id: String,
    pub user_message_id: String,
    pub assistant_message_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelChatTurnRequest {
    pub execution_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelChatTurnResponse {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetChatTurnSnapshotRequest {
    pub generation_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetChatTurnSnapshotResponse {
    pub message: ChatMessage,
    pub tool_calls: Vec<ChatToolCall>,
}

#[derive(Default)]
struct ChatRuntime {
    turns: Mutex<HashMap<String, ActiveTurn>>,
    sequences: Mutex<HashMap<String, u64>>,
    turns_idle: Notify,
}

#[derive(Clone)]
struct ActiveTurn {
    conversation_id: String,
    cancellation: CancellationToken,
}

pub struct ChatService {
    db_path: PathBuf,
    llm_repository: SqliteLlmRepository,
    credentials: Arc<dyn CredentialStore>,
    http_client: reqwest::Client,
    event_sink: Arc<dyn ChatEventSink>,
    runtime: Arc<ChatRuntime>,
    runtime_handle: Option<Handle>,
    terminals: Mutex<Option<Arc<TerminalService>>>,
}

impl ChatService {
    pub fn new(
        db_path: impl AsRef<Path>,
        credentials: Arc<dyn CredentialStore>,
        event_sink: Arc<dyn ChatEventSink>,
    ) -> Self {
        let db_path = db_path.as_ref().to_path_buf();
        Self {
            llm_repository: SqliteLlmRepository::new(&db_path),
            db_path,
            credentials,
            http_client: reqwest::Client::new(),
            event_sink,
            runtime: Arc::new(ChatRuntime::default()),
            runtime_handle: Handle::try_current().ok(),
            terminals: Mutex::new(None),
        }
    }

    pub fn with_keyring(db_path: impl AsRef<Path>, event_sink: Arc<dyn ChatEventSink>) -> Self {
        Self::new(db_path, Arc::new(KeyringCredentialStore), event_sink)
    }

    pub fn with_keyring_and_runtime(
        db_path: impl AsRef<Path>,
        event_sink: Arc<dyn ChatEventSink>,
        runtime_handle: Handle,
    ) -> Self {
        Self::with_dependencies_and_runtime(
            db_path,
            Arc::new(KeyringCredentialStore),
            event_sink,
            runtime_handle,
        )
    }

    pub fn with_dependencies_and_runtime(
        db_path: impl AsRef<Path>,
        credentials: Arc<dyn CredentialStore>,
        event_sink: Arc<dyn ChatEventSink>,
        runtime_handle: Handle,
    ) -> Self {
        Self::with_http_client_and_runtime(
            db_path,
            credentials,
            reqwest::Client::new(),
            event_sink,
            runtime_handle,
        )
    }

    pub fn with_http_client_and_runtime(
        db_path: impl AsRef<Path>,
        credentials: Arc<dyn CredentialStore>,
        http_client: reqwest::Client,
        event_sink: Arc<dyn ChatEventSink>,
        runtime_handle: Handle,
    ) -> Self {
        let db_path = db_path.as_ref().to_path_buf();
        Self {
            llm_repository: SqliteLlmRepository::new(&db_path),
            db_path,
            credentials,
            http_client,
            event_sink,
            runtime: Arc::new(ChatRuntime::default()),
            runtime_handle: Some(runtime_handle),
            terminals: Mutex::new(None),
        }
    }

    pub fn attach_terminal_service(&self, terminals: Arc<TerminalService>) {
        *self
            .terminals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(terminals);
    }

    fn connection(&self) -> Result<Connection, ChatCommandError> {
        let connection = Connection::open(&self.db_path)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(ChatCommandError::from)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(ChatCommandError::from)?;
        Ok(connection)
    }

    pub fn recover_interrupted_turns(&self) -> Result<usize, ChatCommandError> {
        let now = now_iso();
        let database = self.connection()?;
        let transaction = database.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE chat_messages
             SET status = 'interrupted', error_code = 'app_restarted',
                  error_message = 'O aplicativo foi encerrado durante a resposta.', updated_at = ?1
             WHERE status IN ('pending', 'streaming')",
            params![now.as_str()],
        )?;
        transaction.execute(
            "UPDATE chat_model_calls
             SET status = 'interrupted', error_code = 'app_restarted', completed_at = ?1
             WHERE status = 'running'",
            params![now.as_str()],
        )?;
        transaction.execute(
            "UPDATE chat_tool_calls
             SET status = 'interrupted', error_code = 'app_restarted',
                 error_message = 'O aplicativo foi encerrado durante a ferramenta.', completed_at = ?1
             WHERE status = 'running'",
            params![now],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    pub fn list_conversations(
        &self,
        request: ListChatConversationsRequest,
    ) -> Result<ListChatConversationsResponse, ChatCommandError> {
        let project_id = normalize_id(&request.project_id, "projectId")?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
                    title, created_at, updated_at, transport_kind, cli_backend,
                    model_profile, model_source
             FROM chat_conversations WHERE project_id = ?1
             ORDER BY updated_at DESC, id",
        )?;
        let rows = statement.query_map(params![project_id], map_conversation_record)?;
        let mut conversations = Vec::new();
        for row in rows {
            conversations.push(self.decorate_conversation(row?)?);
        }
        Ok(ListChatConversationsResponse { conversations })
    }

    /// Validates a Companion target exactly as conversation creation does.
    fn resolve_target(
        &self,
        database: &Connection,
        project_id: &str,
        target: ChatExecutionTargetRequest,
    ) -> Result<ResolvedTarget, ChatCommandError> {
        let (
            connection_id,
            provider_id,
            model_id,
            transport,
            cli_backend,
            model_profile,
            model_source,
        ) = match target {
            ChatExecutionTargetRequest::Api { connection_id } => {
                let connection_id = normalize_id(&connection_id, "connectionId")?;
                let provider_connection = self
                    .llm_repository
                    .find_resolved_connection(&connection_id)
                    .map_err(|error| {
                        ChatCommandError::new("connection_not_found", error.message)
                    })?;
                self.ensure_connection_eligible(&provider_connection)?;
                (
                    Some(connection_id),
                    provider_connection.provider_id.as_str().to_string(),
                    provider_connection.default_model,
                    ChatTransportKind::Api,
                    None,
                    None,
                    CliModelSelectionSource::Catalog,
                )
            }
            ChatExecutionTargetRequest::Cli {
                backend,
                model,
                model_profile,
            } => {
                if backend == CliBackend::Custom {
                    return Err(ChatCommandError::new(
                        "companion_cli_unsupported",
                        "Custom CLI is not allowed for the Companion.",
                    ));
                }
                let model = normalize_chat_model_id(model)?;
                let model_profile =
                    normalize_optional_chat_value(model_profile, 100, "modelProfile")?;
                if matches!(backend, CliBackend::Gemini) && model_profile.is_some() {
                    return Err(ChatCommandError::new(
                        "model_profile_unsupported",
                        "Gemini CLI does not expose a verified model profile contract.",
                    ));
                }
                let model_source = match model.as_deref() {
                    None => CliModelSelectionSource::Default,
                    Some(model_id) => {
                        let in_catalog: bool = database.query_row(
                            "SELECT EXISTS(
                                   SELECT 1 FROM cli_model_catalog_entries e
                                   JOIN cli_model_catalog_snapshots s ON s.id=e.snapshot_id
                                   WHERE s.project_id=?1 AND s.backend=?2 AND e.model_id=?3
                                 )",
                            params![project_id, backend.as_str(), model_id],
                            |row| row.get(0),
                        )?;
                        if in_catalog {
                            CliModelSelectionSource::Catalog
                        } else {
                            CliModelSelectionSource::Manual
                        }
                    }
                };
                validate_catalog_model_selection(
                    database,
                    project_id,
                    backend,
                    model.as_deref(),
                    model_profile.as_deref(),
                    model_source,
                )
                .map_err(|error| ChatCommandError::new(error.code(), error.to_string()))?;
                (
                    None,
                    backend.as_str().to_string(),
                    model.clone().unwrap_or_else(|| "default".into()),
                    ChatTransportKind::Cli,
                    Some(backend),
                    model_profile,
                    model_source,
                )
            }
        };

        Ok(ResolvedTarget {
            connection_id,
            provider_id,
            model_id,
            transport,
            cli_backend,
            model_profile,
            model_source,
        })
    }

    /// The project's last chosen Companion target, if any. An unreadable
    /// value is ignored rather than failing the chat.
    pub fn default_target(
        &self,
        project_id: &str,
    ) -> Result<Option<ChatExecutionTargetRequest>, ChatCommandError> {
        let project_id = normalize_id(project_id, "projectId")?;
        let value: Option<String> = self
            .connection()?
            .query_row(
                "SELECT value FROM runtime_settings WHERE key = ?1",
                params![companion_target_setting_key(&project_id)],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.and_then(|value| serde_json::from_str(&value).ok()))
    }

    /// Remembers `target` as the project's Companion target after validating
    /// it, so the panel and resolution conversations survive a restart.
    pub fn save_default_target(
        &self,
        project_id: &str,
        target: ChatExecutionTargetRequest,
    ) -> Result<(), ChatCommandError> {
        let project_id = normalize_id(project_id, "projectId")?;
        let database = self.connection()?;
        self.resolve_target(&database, &project_id, target.clone())?;
        Self::store_default_target(&database, &project_id, &target)
    }

    fn store_default_target(
        database: &Connection,
        project_id: &str,
        target: &ChatExecutionTargetRequest,
    ) -> Result<(), ChatCommandError> {
        let value = serde_json::to_string(target).map_err(|error| {
            ChatCommandError::new(
                "invalid_target",
                format!("Invalid Companion target: {error}"),
            )
        })?;
        database.execute(
            "INSERT INTO runtime_settings(key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![companion_target_setting_key(project_id), value, now_iso()],
        )?;
        Ok(())
    }

    /// Applies `target` to an existing conversation from its next message and
    /// remembers it as the project default. Refused while a reply is running.
    pub fn update_conversation_target(
        &self,
        request: UpdateChatConversationTargetRequest,
    ) -> Result<GetChatConversationResponse, ChatCommandError> {
        let conversation_id = normalize_id(&request.conversation_id, "conversationId")?;
        let record = self.get_conversation_record(&conversation_id)?;
        let mut database = self.connection()?;
        let transaction = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let running: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE conversation_id = ?1 AND status IN ('pending', 'streaming'))",
            params![conversation_id],
            |row| row.get(0),
        )?;
        if running {
            return Err(ChatCommandError::new(
                "conversation_busy",
                "Wait for the current reply to finish before changing the connection.",
            ));
        }
        let resolved =
            self.resolve_target(&transaction, &record.project_id, request.target.clone())?;
        transaction.execute(
            "UPDATE chat_conversations
             SET connection_id = ?2, provider_id_snapshot = ?3, model_id_snapshot = ?4,
                 transport_kind = ?5, cli_backend = ?6, model_profile = ?7, model_source = ?8,
                 updated_at = ?9
             WHERE id = ?1",
            params![
                conversation_id,
                resolved.connection_id,
                resolved.provider_id,
                resolved.model_id,
                resolved.transport.as_str(),
                resolved.cli_backend.map(CliBackend::as_str),
                resolved.model_profile,
                resolved.model_source.as_str(),
                now_iso(),
            ],
        )?;
        Self::store_default_target(&transaction, &record.project_id, &request.target)?;
        transaction.commit()?;
        self.get_conversation(GetChatConversationRequest { conversation_id })
    }

    pub fn create_conversation(
        &self,
        request: CreateChatConversationRequest,
    ) -> Result<CreateChatConversationResponse, ChatCommandError> {
        let project_id = normalize_id(&request.project_id, "projectId")?;
        let database = self.connection()?;
        let project_exists: bool = database.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1 AND is_archived = 0)",
            params![project_id],
            |row| row.get(0),
        )?;
        if !project_exists {
            return Err(ChatCommandError::new(
                "project_not_found",
                "Active project not found.",
            ));
        }
        let ResolvedTarget {
            connection_id,
            provider_id,
            model_id,
            transport,
            cli_backend,
            model_profile,
            model_source,
        } = self.resolve_target(&database, &project_id, request.target.clone())?;

        let id = Uuid::new_v4().to_string();
        let now = now_iso();
        database.execute(
            "INSERT INTO chat_conversations (
               id, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
               title, created_at, updated_at, transport_kind, cli_backend, model_profile,
               model_source
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'Nova conversa', ?6, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                project_id,
                connection_id,
                provider_id,
                model_id,
                now,
                transport.as_str(),
                cli_backend.map(CliBackend::as_str),
                model_profile,
                model_source.as_str(),
            ],
        )?;
        Self::store_default_target(&database, &project_id, &request.target)?;
        let conversation = self.get_conversation_record(&id)?;
        Ok(CreateChatConversationResponse {
            conversation: self.decorate_conversation(conversation)?,
        })
    }

    pub fn get_conversation(
        &self,
        request: GetChatConversationRequest,
    ) -> Result<GetChatConversationResponse, ChatCommandError> {
        let conversation_id = normalize_id(&request.conversation_id, "conversationId")?;
        let conversation =
            self.decorate_conversation(self.get_conversation_record(&conversation_id)?)?;
        let messages = self.list_messages(&conversation_id)?;
        let tool_calls = self.list_tool_calls_for_conversation(&conversation_id)?;
        Ok(GetChatConversationResponse {
            conversation,
            messages,
            tool_calls,
        })
    }

    pub fn delete_conversation(
        &self,
        request: DeleteChatConversationRequest,
    ) -> Result<DeleteChatConversationResponse, ChatCommandError> {
        let conversation_id = normalize_id(&request.conversation_id, "conversationId")?;
        if self
            .runtime
            .turns
            .lock()
            .map_err(|_| runtime_lock_error())?
            .values()
            .any(|turn| turn.conversation_id == conversation_id)
        {
            return Err(ChatCommandError::new(
                "turn_active",
                "Cancel the reply in progress before deleting the conversation.",
            ));
        }
        let database = self.connection()?;
        let transaction = database.unchecked_transaction()?;
        let specification_linked: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM specification_conversations WHERE conversation_id=?1)",
            [&conversation_id],
            |r| r.get(0),
        )?;
        if specification_linked {
            return Err(ChatCommandError::new(
                "specification_history",
                "This conversation keeps Specification questions and answers. Read and answer them in Pulsations.",
            ));
        }
        let resolution_linked: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM specification_resolution_conversations WHERE conversation_id=?1)",
            [&conversation_id],
            |r| r.get(0),
        )?;
        if resolution_linked {
            return Err(ChatCommandError::new(
                "specification_history",
                "This conversation keeps the resolution of a Specification clarification. Read it in Pulsations.",
            ));
        }
        transaction.execute(
            "UPDATE ui_sessions SET active_chat_conversation_id = NULL
             WHERE active_chat_conversation_id = ?1",
            params![conversation_id],
        )?;
        let deleted = transaction.execute(
            "DELETE FROM chat_conversations WHERE id = ?1",
            params![conversation_id],
        )? > 0;
        transaction.commit()?;
        Ok(DeleteChatConversationResponse { deleted })
    }

    pub fn start_turn(
        self: &Arc<Self>,
        request: SendChatMessageRequest,
        workspace_root: &Path,
    ) -> Result<SendChatMessageResponse, ChatCommandError> {
        let conversation_id = normalize_id(&request.conversation_id, "conversationId")?;
        let content = request.content.trim();
        if content.is_empty() || content.chars().count() > MAX_USER_MESSAGE_CHARS {
            return Err(ChatCommandError::new(
                "invalid_message",
                "The message must have between 1 and 20,000 characters.",
            ));
        }
        let conversation = self.get_conversation_record(&conversation_id)?;
        if request
            .expected_target
            .as_ref()
            .is_some_and(|target| !target_matches_record(target, &conversation))
        {
            return Err(ChatCommandError::new(
                "target_changed",
                "The conversation target changed before Send. Review the selected model and send again.",
            ));
        }
        let provider_connection = match conversation.transport {
            ChatTransportKind::Api => {
                let connection_id = conversation.connection_id.clone().ok_or_else(|| {
                    ChatCommandError::new(
                        "connection_removed",
                        "This conversation's connection was removed. Start a new conversation.",
                    )
                })?;
                let provider_connection = self
                    .llm_repository
                    .find_resolved_connection(&connection_id)
                    .map_err(|_| {
                        ChatCommandError::new(
                            "connection_removed",
                            "This conversation's connection no longer exists. Start a new conversation.",
                        )
                    })?;
                if provider_connection.provider_id.as_str() != conversation.provider_id {
                    return Err(ChatCommandError::new(
                        "connection_changed",
                        "The connection's provider changed. Start a new conversation.",
                    ));
                }
                self.ensure_connection_eligible_for_model(
                    &provider_connection,
                    &conversation.model_id,
                )?;
                Some(provider_connection)
            }
            ChatTransportKind::Cli => {
                if conversation.cli_backend.is_none() {
                    return Err(ChatCommandError::new(
                        "companion_cli_invalid",
                        "The conversation does not contain a valid CLI backend snapshot.",
                    ));
                }
                if self
                    .terminals
                    .lock()
                    .map_err(|_| runtime_lock_error())?
                    .is_none()
                {
                    return Err(ChatCommandError::new(
                        "companion_cli_unavailable",
                        "The CLI runtime is unavailable.",
                    ));
                }
                None
            }
        };
        self.ensure_workspace_matches(&conversation.project_id, workspace_root)?;

        let mut turns = self
            .runtime
            .turns
            .lock()
            .map_err(|_| runtime_lock_error())?;
        if turns
            .values()
            .any(|turn| turn.conversation_id == conversation_id)
        {
            return Err(ChatCommandError::new(
                "turn_active",
                "This conversation already has a reply in progress.",
            ));
        }

        let execution_id = Uuid::new_v4().to_string();
        let generation_id = Uuid::new_v4().to_string();
        let user_message_id = Uuid::new_v4().to_string();
        let assistant_message_id = Uuid::new_v4().to_string();
        let now = now_iso();
        let mut database = self.connection()?;
        let transaction = database.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = transaction.query_row(
            "SELECT id, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
                    title, created_at, updated_at, transport_kind, cli_backend,
                    model_profile, model_source FROM chat_conversations WHERE id=?1",
            [&conversation_id],
            map_conversation_record,
        )?;
        if !same_target(&conversation, &current)
            || request
                .expected_target
                .as_ref()
                .is_some_and(|target| !target_matches_record(target, &current))
        {
            return Err(ChatCommandError::new(
                "target_changed",
                "The conversation target changed before Send. Review the selected model and send again.",
            ));
        }
        let next_ordinal: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM chat_messages WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO chat_messages (
               id, conversation_id, ordinal, role, content, status, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'user', ?4, 'completed', ?5, ?5)",
            params![user_message_id, conversation_id, next_ordinal, content, now],
        )?;
        transaction.execute(
            "INSERT INTO chat_messages (
               id, conversation_id, ordinal, role, content, status, execution_id,
               generation_id, created_at, updated_at, target_label
             ) VALUES (?1, ?2, ?3, 'assistant', '', 'pending', ?4, ?5, ?6, ?6, ?7)",
            params![
                assistant_message_id,
                conversation_id,
                next_ordinal + 1,
                execution_id,
                generation_id,
                now,
                target_label(&current),
            ],
        )?;
        if next_ordinal == 0 {
            transaction.execute(
                "UPDATE chat_conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![local_title(content), now, conversation_id],
            )?;
        } else {
            transaction.execute(
                "UPDATE chat_conversations SET updated_at = ?1 WHERE id = ?2",
                params![now, conversation_id],
            )?;
        }
        transaction.commit()?;

        let cancellation = CancellationToken::new();
        turns.insert(
            execution_id.clone(),
            ActiveTurn {
                conversation_id: conversation_id.clone(),
                cancellation: cancellation.clone(),
            },
        );
        drop(turns);
        self.runtime
            .sequences
            .lock()
            .map_err(|_| runtime_lock_error())?
            .insert(generation_id.clone(), 0);

        let response = SendChatMessageResponse {
            execution_id: execution_id.clone(),
            generation_id: generation_id.clone(),
            user_message_id,
            assistant_message_id: assistant_message_id.clone(),
        };
        let service = Arc::clone(self);
        let root = workspace_root.to_path_buf();
        let turn = async move {
            service
                .run_turn(TurnContext {
                    conversation,
                    connection: provider_connection,
                    execution_id,
                    generation_id,
                    assistant_message_id,
                    workspace_root: root,
                    cancellation,
                })
                .await;
        };
        if let Some(handle) = &self.runtime_handle {
            handle.spawn(turn);
        } else {
            std::thread::Builder::new()
                .name("hvc-chat-turn".into())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("temporary chat runtime must start");
                    runtime.block_on(turn);
                })
                .map_err(|error| {
                    ChatCommandError::new(
                        "runtime_unavailable",
                        format!("Failed to start the chat turn: {error}"),
                    )
                })?;
        }
        Ok(response)
    }

    pub fn cancel_all_turns(&self) -> usize {
        let Ok(turns) = self.runtime.turns.lock() else {
            return 0;
        };
        for turn in turns.values() {
            turn.cancellation.cancel();
        }
        turns.len()
    }

    pub async fn wait_for_idle(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.runtime.turns_idle.notified();
            let is_idle = self
                .runtime
                .turns
                .lock()
                .map(|turns| turns.is_empty())
                .unwrap_or(true);
            if is_idle {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline
                || tokio::time::timeout(deadline - now, notified)
                    .await
                    .is_err()
            {
                return false;
            }
        }
    }

    pub fn cancel_turn(
        &self,
        request: CancelChatTurnRequest,
    ) -> Result<CancelChatTurnResponse, ChatCommandError> {
        let execution_id = normalize_id(&request.execution_id, "executionId")?;
        let turns = self
            .runtime
            .turns
            .lock()
            .map_err(|_| runtime_lock_error())?;
        let cancelled = turns.get(&execution_id).is_some_and(|turn| {
            turn.cancellation.cancel();
            true
        });
        Ok(CancelChatTurnResponse { cancelled })
    }

    pub fn get_turn_snapshot(
        &self,
        request: GetChatTurnSnapshotRequest,
    ) -> Result<GetChatTurnSnapshotResponse, ChatCommandError> {
        let generation_id = normalize_id(&request.generation_id, "generationId")?;
        let database = self.connection()?;
        let message = database
            .query_row(
                "SELECT id, conversation_id, ordinal, role, content, status, execution_id,
                        generation_id, last_sequence, input_tokens, output_tokens,
                        estimated_cost_usd, error_code, error_message, created_at, updated_at,
                        target_label
                 FROM chat_messages WHERE generation_id = ?1",
                params![generation_id],
                map_message,
            )
            .optional()?
            .ok_or_else(|| ChatCommandError::new("turn_not_found", "Turn not found."))?;
        let tool_calls = self.list_tool_calls_for_message(&message.id)?;
        Ok(GetChatTurnSnapshotResponse {
            message,
            tool_calls,
        })
    }

    fn ensure_workspace_matches(
        &self,
        project_id: &str,
        workspace_root: &Path,
    ) -> Result<(), ChatCommandError> {
        let expected: String = self
            .connection()?
            .query_row(
                "SELECT workspace_path FROM projects WHERE id = ?1 AND is_archived = 0",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ChatCommandError::new("project_not_found", "Project not found."))?;
        let expected = Path::new(&expected).canonicalize().map_err(|_| {
            ChatCommandError::new(
                "workspace_mismatch",
                "The project workspace does not exist.",
            )
        })?;
        let actual = workspace_root.canonicalize().map_err(|_| {
            ChatCommandError::new("workspace_mismatch", "The active workspace is invalid.")
        })?;
        if expected != actual {
            return Err(ChatCommandError::new(
                "workspace_mismatch",
                "A conversa pertence a outro workspace.",
            ));
        }
        Ok(())
    }

    fn ensure_connection_eligible(
        &self,
        connection: &ResolvedConnection,
    ) -> Result<(), ChatCommandError> {
        self.ensure_connection_eligible_for_model(connection, &connection.default_model)
    }

    fn ensure_connection_eligible_for_model(
        &self,
        connection: &ResolvedConnection,
        model: &str,
    ) -> Result<(), ChatCommandError> {
        let profile = profile_by_id(connection.provider_id.as_str()).ok_or_else(|| {
            ChatCommandError::new(
                "provider_incompatible",
                "The provider has no compatible profile.",
            )
        })?;
        let model = model_entry(connection.provider_id.as_str(), model).ok_or_else(|| {
            ChatCommandError::new(
                "model_incompatible",
                "The model is not marked as tool-calling capable in the versioned catalog.",
            )
        })?;
        if !connection.enabled || connection.state == "blocked" {
            return Err(ChatCommandError::new(
                "connection_unavailable",
                "The connection is disabled or blocked.",
            ));
        }
        if !profile.capabilities.tool_calling || !model.tool_calling {
            return Err(ChatCommandError::new(
                "model_incompatible",
                "The connection and model must support tool calling.",
            ));
        }
        if profile.auth_style != AuthStyle::None
            && self
                .credentials
                .get(&connection.credential_ref)
                .map_err(|error| ChatCommandError::new("credential_unavailable", error.message))?
                .is_none()
        {
            return Err(ChatCommandError::new(
                "credential_unavailable",
                "The connection has no credential configured.",
            ));
        }
        Ok(())
    }

    fn get_conversation_record(&self, id: &str) -> Result<ConversationRecord, ChatCommandError> {
        self.connection()?
            .query_row(
                "SELECT id, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
                        title, created_at, updated_at, transport_kind, cli_backend,
                        model_profile, model_source
                 FROM chat_conversations WHERE id = ?1",
                params![id],
                map_conversation_record,
            )
            .optional()?
            .ok_or_else(|| {
                ChatCommandError::new("conversation_not_found", "Conversation not found.")
            })
    }

    fn decorate_conversation(
        &self,
        record: ConversationRecord,
    ) -> Result<ChatConversation, ChatCommandError> {
        let connection_available = match record.transport {
            ChatTransportKind::Api => record.connection_id.as_ref().is_some_and(|id| {
                self.llm_repository
                    .find_resolved_connection(id)
                    .ok()
                    .filter(|connection| connection.provider_id.as_str() == record.provider_id)
                    .and_then(|connection| {
                        self.ensure_connection_eligible_for_model(&connection, &record.model_id)
                            .ok()
                    })
                    .is_some()
            }),
            ChatTransportKind::Cli => record.cli_backend.is_some_and(|backend| {
                discover_clis().into_iter().any(|availability| {
                    availability.backend == backend && availability.executable_path.is_some()
                })
            }),
        };
        Ok(ChatConversation {
            id: record.id,
            project_id: record.project_id,
            connection_id: record.connection_id,
            provider_id: record.provider_id,
            model_id: record.model_id,
            transport: record.transport,
            cli_backend: record.cli_backend,
            model_profile: record.model_profile,
            model_source: record.model_source,
            title: record.title,
            connection_available,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }

    fn list_messages(&self, conversation_id: &str) -> Result<Vec<ChatMessage>, ChatCommandError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, ordinal, role, content, status, execution_id,
                    generation_id, last_sequence, input_tokens, output_tokens,
                    estimated_cost_usd, error_code, error_message, created_at, updated_at,
                    target_label
             FROM chat_messages WHERE conversation_id = ?1 ORDER BY ordinal",
        )?;
        let rows = statement.query_map(params![conversation_id], map_message)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(ChatCommandError::from)
    }

    fn list_tool_calls_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<ChatToolCall>, ChatCommandError> {
        self.list_tool_calls(
            "SELECT tc.id, tc.assistant_message_id, tc.round_index, tc.tool_call_id,
                    tc.tool_name, tc.sanitized_arguments, tc.result_summary, tc.result_bytes,
                    tc.status, tc.error_code, tc.error_message, tc.started_at, tc.completed_at
             FROM chat_tool_calls tc
             JOIN chat_messages m ON m.id = tc.assistant_message_id
             WHERE m.conversation_id = ?1 ORDER BY m.ordinal, tc.round_index, tc.started_at",
            conversation_id,
        )
    }

    fn list_tool_calls_for_message(
        &self,
        message_id: &str,
    ) -> Result<Vec<ChatToolCall>, ChatCommandError> {
        self.list_tool_calls(
            "SELECT id, assistant_message_id, round_index, tool_call_id, tool_name,
                    sanitized_arguments, result_summary, result_bytes, status, error_code,
                    error_message, started_at, completed_at
             FROM chat_tool_calls WHERE assistant_message_id = ?1
             ORDER BY round_index, started_at",
            message_id,
        )
    }

    fn list_tool_calls(&self, sql: &str, id: &str) -> Result<Vec<ChatToolCall>, ChatCommandError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(sql)?;
        let rows = statement.query_map(params![id], map_tool_call)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(ChatCommandError::from)
    }
}

#[derive(Debug, Clone)]
struct ConversationRecord {
    id: String,
    project_id: String,
    connection_id: Option<String>,
    provider_id: String,
    model_id: String,
    transport: ChatTransportKind,
    cli_backend: Option<CliBackend>,
    model_profile: Option<String>,
    model_source: CliModelSelectionSource,
    title: String,
    created_at: String,
    updated_at: String,
}

fn same_target(a: &ConversationRecord, b: &ConversationRecord) -> bool {
    a.transport == b.transport
        && a.connection_id == b.connection_id
        && a.provider_id == b.provider_id
        && a.model_id == b.model_id
        && a.cli_backend == b.cli_backend
        && a.model_profile == b.model_profile
        && a.model_source == b.model_source
}

fn target_matches_record(target: &ChatExecutionTargetRequest, record: &ConversationRecord) -> bool {
    match target {
        ChatExecutionTargetRequest::Api { connection_id } => {
            record.transport == ChatTransportKind::Api
                && record.connection_id.as_deref() == Some(connection_id)
        }
        ChatExecutionTargetRequest::Cli {
            backend,
            model,
            model_profile,
        } => {
            record.transport == ChatTransportKind::Cli
                && record.cli_backend == Some(*backend)
                && record.model_id == model.as_deref().unwrap_or("default")
                && record.model_profile == *model_profile
        }
    }
}

fn target_label(record: &ConversationRecord) -> String {
    let mut label = format!("{} / {}", record.provider_id, record.model_id);
    if let Some(profile) = &record.model_profile {
        label.push_str(" / ");
        label.push_str(profile);
    }
    label
}

fn map_conversation_record(row: &Row<'_>) -> rusqlite::Result<ConversationRecord> {
    let transport: String = row.get(8)?;
    let cli_backend: Option<String> = row.get(9)?;
    let model_source: String = row.get(11)?;
    Ok(ConversationRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        connection_id: row.get(2)?,
        provider_id: row.get(3)?,
        model_id: row.get(4)?,
        title: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        transport: ChatTransportKind::parse(&transport).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(8, transport, rusqlite::types::Type::Text)
        })?,
        cli_backend: cli_backend
            .map(|backend| {
                CliBackend::parse(&backend).ok_or_else(|| {
                    rusqlite::Error::InvalidColumnType(9, backend, rusqlite::types::Type::Text)
                })
            })
            .transpose()?,
        model_profile: row.get(10)?,
        model_source: CliModelSelectionSource::parse(&model_source).ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(11, model_source, rusqlite::types::Type::Text)
        })?,
    })
}

fn map_message(row: &Row<'_>) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        ordinal: row.get(2)?,
        role: row.get(3)?,
        content: row.get(4)?,
        status: row.get(5)?,
        execution_id: row.get(6)?,
        generation_id: row.get(7)?,
        last_sequence: row.get::<_, i64>(8)?.max(0) as u64,
        input_tokens: row.get(9)?,
        output_tokens: row.get(10)?,
        estimated_cost_usd: row.get(11)?,
        error_code: row.get(12)?,
        error_message: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
        target_label: row.get(16)?,
    })
}

fn map_tool_call(row: &Row<'_>) -> rusqlite::Result<ChatToolCall> {
    let arguments: String = row.get(5)?;
    let arguments = serde_json::from_str(&arguments).unwrap_or_else(|_| json!({}));
    Ok(ChatToolCall {
        id: row.get(0)?,
        assistant_message_id: row.get(1)?,
        round_index: row.get::<_, i64>(2)?.max(0) as usize,
        tool_call_id: row.get(3)?,
        tool_name: row.get(4)?,
        sanitized_arguments: typed_tool_arguments(&arguments),
        result_summary: row.get(6)?,
        result_bytes: row.get::<_, i64>(7)?.max(0) as usize,
        status: row.get(8)?,
        error_code: row.get(9)?,
        error_message: row.get(10)?,
        started_at: row.get(11)?,
        completed_at: row.get(12)?,
    })
}

fn typed_tool_arguments(arguments: &Value) -> ChatToolArguments {
    let Some(arguments) = arguments.as_object() else {
        return BTreeMap::new();
    };
    arguments
        .iter()
        .filter_map(|(key, value)| {
            let value = match value {
                Value::String(value) => ChatToolArgumentValue::Text(value.clone()),
                Value::Number(value) => value.as_i64().map(ChatToolArgumentValue::Integer)?,
                Value::Bool(value) => ChatToolArgumentValue::Boolean(*value),
                Value::Null | Value::Array(_) | Value::Object(_) => return None,
            };
            Some((key.clone(), value))
        })
        .collect()
}

fn normalize_id(raw: &str, field: &str) -> Result<String, ChatCommandError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ChatCommandError::new(
            "invalid_request",
            format!("{field} cannot be empty."),
        ));
    }
    Ok(value.to_string())
}

fn normalize_optional_chat_value(
    value: Option<String>,
    max_chars: usize,
    field: &str,
) -> Result<Option<String>, ChatCommandError> {
    let value = value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if value.as_ref().is_some_and(|value| {
        value.chars().count() > max_chars || value.chars().any(char::is_control)
    }) {
        return Err(ChatCommandError::new(
            "invalid_target",
            format!("{field} is invalid."),
        ));
    }
    Ok(value)
}

fn normalize_chat_model_id(value: Option<String>) -> Result<Option<String>, ChatCommandError> {
    let model = normalize_optional_chat_value(value, 200, "model")?;
    if model.as_ref().is_some_and(|value| {
        !value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | ':' | '/' | '@' | '+')
        })
    }) {
        return Err(ChatCommandError::new(
            "invalid_target",
            "model contains unsupported characters.",
        ));
    }
    Ok(model)
}

fn project_history(
    messages: Vec<ProtocolMessage>,
    character_budget: usize,
) -> Vec<ProtocolMessage> {
    if messages.is_empty() {
        return messages;
    }
    let mut used = 0usize;
    let mut projected = Vec::new();
    let mut omitted = false;
    for (reverse_index, message) in messages.into_iter().rev().enumerate() {
        let size = message.content.chars().count();
        if reverse_index > 0 && used.saturating_add(size) > character_budget {
            omitted = true;
            continue;
        }
        used = used.saturating_add(size);
        projected.push(message);
    }
    projected.reverse();
    if omitted {
        projected.insert(
            0,
            ProtocolMessage {
                role: "user".into(),
                content: "[Earlier local conversation messages were omitted from this inference because of the model context budget.]".into(),
            },
        );
    }
    projected
}

fn local_title(message: &str) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut title: String = normalized.chars().take(72).collect();
    if normalized.chars().count() > 72 {
        title.push('…');
    }
    title
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

fn runtime_lock_error() -> ChatCommandError {
    ChatCommandError::new("chat_runtime", "Failed to access the internal chat state.")
}

#[derive(Clone)]
struct TurnContext {
    conversation: ConversationRecord,
    connection: Option<ResolvedConnection>,
    execution_id: String,
    generation_id: String,
    assistant_message_id: String,
    workspace_root: PathBuf,
    cancellation: CancellationToken,
}

#[derive(Debug, Clone)]
struct ProtocolMessage {
    role: String,
    content: String,
}

#[derive(Debug, Clone)]
struct NormalizedToolCall {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Debug, Clone)]
struct ToolResult {
    call: NormalizedToolCall,
    content: String,
    is_error: bool,
}

#[derive(Debug, Clone)]
struct ToolExchange {
    assistant_content: String,
    calls: Vec<NormalizedToolCall>,
    results: Vec<ToolResult>,
}

#[derive(Debug, Clone)]
struct ModelResponse {
    content: String,
    tool_calls: Vec<NormalizedToolCall>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    streamed: bool,
}

#[derive(Debug, Clone)]
struct ModelFailure {
    code: String,
    message: String,
    retryable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum CompanionControlEnvelope {
    Final { text: String },
    ToolCalls { calls: Vec<CompanionControlCall> },
}

fn exceeds_remaining_read_budget(envelope: &CompanionControlEnvelope, remaining: usize) -> bool {
    matches!(envelope, CompanionControlEnvelope::ToolCalls { calls } if calls.len() > remaining)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompanionControlCall {
    id: String,
    name: String,
    #[serde(default = "empty_json_object")]
    arguments: Value,
}

impl ChatService {
    /// Closes the calls of a turn that ended with a round still in flight.
    fn close_running_model_calls(
        &self,
        assistant_message_id: &str,
        reason: &str,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_model_calls SET status='failed',completed_at=?2,error_code=?3,termination_reason=?3
             WHERE assistant_message_id=?1 AND status='running'",
            params![assistant_message_id, now_iso(), reason],
        )?;
        Ok(())
    }

    async fn run_turn(&self, context: TurnContext) {
        let _ = self.set_message_status(&context.assistant_message_id, "streaming", None, None);
        let _ = self.emit_turn_event(
            ChatEventKind::TurnStarted,
            &context,
            ChatEventDetail::Empty {},
        );

        let (limit, limit_message) = if context.conversation.transport == ChatTransportKind::Cli {
            (
                CLI_TURN_TIMEOUT,
                "The turn reached its 5-minute limit. Narrow the question and try again.",
            )
        } else {
            (
                TURN_TIMEOUT,
                "The turn reached its 120-second limit. Narrow the question and try again.",
            )
        };
        let result = tokio::select! {
            biased;
            _ = context.cancellation.cancelled() => Err(TurnOutcome::Cancelled),
            timed = tokio::time::timeout(limit, self.run_model_loop(&context)) => {
                match timed {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(TurnOutcome::Failed(error)),
                    Err(_) => Err(TurnOutcome::Failed(ChatCommandError::new(
                        "turn_timeout",
                        limit_message,
                    ))),
                }
            }
        };
        // A timeout or cancellation drops the round in flight; its call row
        // stayed `running` forever (2026-09-26).
        match &result {
            Err(TurnOutcome::Cancelled) => {
                let _ = self.close_running_model_calls(&context.assistant_message_id, "cancelled");
            }
            Err(TurnOutcome::Failed(error)) => {
                let _ = self.close_running_model_calls(&context.assistant_message_id, &error.code);
            }
            Ok(()) => {}
        }

        match result {
            Ok(()) => {
                let _ =
                    self.set_message_status(&context.assistant_message_id, "completed", None, None);
                let _ = self.emit_turn_event(
                    ChatEventKind::TurnCompleted,
                    &context,
                    ChatEventDetail::Empty {},
                );
            }
            Err(TurnOutcome::Cancelled) => {
                let _ = self.set_message_status(
                    &context.assistant_message_id,
                    "cancelled",
                    Some("cancelled"),
                    Some("Reply cancelled by the user."),
                );
                let _ = self.emit_turn_event(
                    ChatEventKind::TurnCancelled,
                    &context,
                    ChatEventDetail::Empty {},
                );
            }
            Err(TurnOutcome::Failed(error)) => {
                let _ = self.set_message_status(
                    &context.assistant_message_id,
                    "failed",
                    Some(&error.code),
                    Some(&error.message),
                );
                let _ = self.emit_turn_event(
                    ChatEventKind::TurnFailed,
                    &context,
                    ChatEventDetail::Failed {
                        error: ChatEventError {
                            code: error.code,
                            message: error.message,
                        },
                    },
                );
            }
        }

        if let Ok(mut turns) = self.runtime.turns.lock() {
            turns.remove(&context.execution_id);
        }
        self.runtime.turns_idle.notify_waiters();
        if let Ok(mut sequences) = self.runtime.sequences.lock() {
            sequences.remove(&context.generation_id);
        }
    }

    async fn run_model_loop(&self, context: &TurnContext) -> Result<(), ChatCommandError> {
        match context.conversation.transport {
            ChatTransportKind::Api => self.run_api_model_loop(context).await,
            ChatTransportKind::Cli => self.run_cli_model_loop(context).await,
        }
    }

    async fn run_api_model_loop(&self, context: &TurnContext) -> Result<(), ChatCommandError> {
        let connection = context.connection.as_ref().ok_or_else(|| {
            ChatCommandError::new(
                "connection_removed",
                "API connection snapshot is unavailable.",
            )
        })?;
        let profile = profile_by_id(connection.provider_id.as_str()).ok_or_else(|| {
            ChatCommandError::new(
                "provider_incompatible",
                "The provider has no compatible profile.",
            )
        })?;
        let api_key = if profile.auth_style == AuthStyle::None {
            None
        } else {
            self.credentials
                .get(&connection.credential_ref)
                .map_err(|error| ChatCommandError::new("credential_unavailable", error.message))?
        };
        let repository = RepositoryContext::new(&context.workspace_root)
            .map_err(|error| ChatCommandError::new("repository_unavailable", error.0))?;
        let messages =
            self.load_protocol_messages(&context.conversation.id, &context.assistant_message_id)?;
        let mut exchanges = Vec::<ToolExchange>::new();
        let mut total_tool_calls = 0usize;
        let mut total_tool_bytes = 0usize;
        let mut total_input_tokens = 0i64;
        let mut total_output_tokens = 0i64;
        let mut total_cost = Some(0.0f64);

        for round_index in 0..MAX_MODEL_ROUNDS {
            if context.cancellation.is_cancelled() {
                return Err(ChatCommandError::new("cancelled", "Resposta cancelada."));
            }
            let mut attempts = 0usize;
            let model_response = loop {
                attempts += 1;
                let model_call_id = Uuid::new_v4().to_string();
                let started_at = now_iso();
                self.connection()?.execute(
                    "INSERT INTO chat_model_calls (
                       id, assistant_message_id, round_index, provider_id, model_id,
                       started_at, status
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running')",
                    params![
                        model_call_id,
                        context.assistant_message_id,
                        round_index as i64,
                        context.conversation.provider_id,
                        context.conversation.model_id,
                        started_at
                    ],
                )?;
                let started = Instant::now();
                let usage = UsageRecorder::new(
                    &self.db_path,
                    UsageContext {
                        source_kind: "chat_model_call".into(),
                        source_id: model_call_id.clone(),
                        pulsation_id: None,
                        project_id: Some(context.conversation.project_id.clone()),
                        transport: UsageTransport::Api,
                        provider_id: context.conversation.provider_id.clone(),
                        requested_model: Some(context.conversation.model_id.clone()),
                        started_at: started_at.clone(),
                    },
                );
                let response = self
                    .call_model(
                        &profile,
                        connection,
                        &context.conversation.model_id,
                        api_key.as_deref(),
                        context,
                        &messages,
                        &exchanges,
                        &usage,
                    )
                    .await;
                usage
                    .finish(response.is_ok())
                    .map_err(ChatCommandError::database)?;
                let latency_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
                match response {
                    Ok(response) => {
                        let cost = estimate_cost_usd(
                            &context.conversation.provider_id,
                            &context.conversation.model_id,
                            response.input_tokens,
                            response.output_tokens,
                        );
                        self.finish_model_call(
                            &model_call_id,
                            "completed",
                            latency_ms,
                            response.input_tokens,
                            response.output_tokens,
                            cost,
                            None,
                        )?;
                        break (model_call_id, response, cost);
                    }
                    Err(error) => {
                        self.finish_model_call(
                            &model_call_id,
                            "failed",
                            latency_ms,
                            None,
                            None,
                            None,
                            Some(&error.code),
                        )?;
                        if attempts < 2 && error.retryable {
                            continue;
                        }
                        return Err(ChatCommandError::new(error.code, error.message));
                    }
                }
            };
            let (model_call_id, response, cost) = model_response;
            total_input_tokens += response.input_tokens.unwrap_or(0).max(0);
            total_output_tokens += response.output_tokens.unwrap_or(0).max(0);
            total_cost = match (total_cost, cost) {
                (Some(total), Some(current)) => Some(total + current),
                _ => None,
            };

            if response.tool_calls.is_empty() {
                if response.content.trim().is_empty() {
                    return Err(ChatCommandError::new(
                        "empty_response",
                        "The provider ended the turn with no content or tool call.",
                    ));
                }
                if !response.streamed {
                    self.stream_visible_content(context, &response.content)
                        .await?;
                }
                self.update_message_usage(
                    &context.assistant_message_id,
                    total_input_tokens,
                    total_output_tokens,
                    total_cost,
                )?;
                return Ok(());
            }

            if !response.content.trim().is_empty() {
                self.replace_message_content(&context.assistant_message_id, &response.content)?;
                self.emit_turn_event(
                    ChatEventKind::ContentReset,
                    context,
                    ChatEventDetail::ContentReset {
                        reason: "tool_round".into(),
                    },
                )?;
                self.replace_message_content(&context.assistant_message_id, "")?;
            }

            total_tool_calls += response.tool_calls.len();
            if total_tool_calls > MAX_TOOL_CALLS {
                return Err(ChatCommandError::new(
                    "tool_call_limit",
                    "The turn reached its limit of 24 reads. Narrow the question and try again.",
                ));
            }

            let results = self
                .execute_tool_calls(
                    context,
                    &repository,
                    &model_call_id,
                    round_index,
                    &response.tool_calls,
                    true,
                )
                .await?;
            for result in &results {
                total_tool_bytes = total_tool_bytes.saturating_add(result.content.len());
            }
            if total_tool_bytes > MAX_TOOL_RESULT_BYTES {
                return Err(ChatCommandError::new(
                    "tool_result_limit",
                    "The turn reached 256 KiB of results. Narrow the question and try again.",
                ));
            }
            exchanges.push(ToolExchange {
                assistant_content: response.content,
                calls: response.tool_calls,
                results,
            });

            if round_index + 1 == MAX_MODEL_ROUNDS {
                return Err(ChatCommandError::new(
                    "model_round_limit",
                    "The turn reached its limit of 8 rounds. Narrow the question and try again.",
                ));
            }
        }
        Err(ChatCommandError::new(
            "model_round_limit",
            "The turn reached its round limit.",
        ))
    }

    async fn run_cli_model_loop(&self, context: &TurnContext) -> Result<(), ChatCommandError> {
        let terminals = self
            .terminals
            .lock()
            .map_err(|_| runtime_lock_error())?
            .clone()
            .ok_or_else(|| {
                ChatCommandError::new(
                    "companion_cli_unavailable",
                    "The CLI runtime is unavailable.",
                )
            })?;
        let backend = context.conversation.cli_backend.ok_or_else(|| {
            ChatCommandError::new(
                "companion_cli_invalid",
                "The conversation does not contain a CLI backend snapshot.",
            )
        })?;
        let repository = RepositoryContext::new(&context.workspace_root)
            .map_err(|error| ChatCommandError::new("repository_unavailable", error.0))?;
        let messages =
            self.load_protocol_messages(&context.conversation.id, &context.assistant_message_id)?;
        let mut exchanges = Vec::<ToolExchange>::new();
        let mut total_tool_calls = 0usize;
        let mut total_tool_bytes = 0usize;

        for round_index in 0..MAX_MODEL_ROUNDS {
            if context.cancellation.is_cancelled() {
                return Err(ChatCommandError::new("cancelled", "Resposta cancelada."));
            }
            let mut repair = false;
            let (model_call_id, envelope) = loop {
                let model_call_id = Uuid::new_v4().to_string();
                let started_at = now_iso();
                self.connection()?.execute(
                    "INSERT INTO chat_model_calls(
                       id,assistant_message_id,round_index,provider_id,model_id,started_at,status,
                       transport_kind,cli_backend,model_profile
                     ) VALUES(?1,?2,?3,?4,?5,?6,'running','cli',?4,?7)",
                    params![
                        model_call_id,
                        context.assistant_message_id,
                        round_index as i64,
                        backend.as_str(),
                        context.conversation.model_id,
                        started_at,
                        context.conversation.model_profile,
                    ],
                )?;
                let remaining_reads = MAX_TOOL_CALLS.saturating_sub(total_tool_calls);
                let prompt = build_cli_companion_prompt(
                    &messages,
                    &exchanges,
                    repair,
                    backend,
                    remaining_reads,
                );
                let started = Instant::now();
                let outcome = terminals
                    .execute_companion_round(
                        CompanionCliRoundRequest {
                            usage_context: UsageContext {
                                source_kind: "chat_model_call".into(),
                                source_id: model_call_id.clone(),
                                pulsation_id: None,
                                project_id: Some(context.conversation.project_id.clone()),
                                transport: UsageTransport::Cli,
                                provider_id: backend.as_str().into(),
                                requested_model: Some(context.conversation.model_id.clone()),
                                started_at: started_at.clone(),
                            },
                            project_id: context.conversation.project_id.clone(),
                            backend,
                            model: (context.conversation.model_id != "default")
                                .then(|| context.conversation.model_id.clone()),
                            model_profile: context.conversation.model_profile.clone(),
                            model_source: context.conversation.model_source,
                            prompt,
                            schema: companion_schema_for_backend(backend),
                        },
                        &context.cancellation,
                        Instant::now() + TURN_TIMEOUT,
                    )
                    .await;
                let latency_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
                match outcome {
                    Ok(output) => {
                        self.connection()?.execute(
                            "UPDATE chat_model_calls SET observed_model=?2,model_verification=?3 WHERE id=?1",
                            params![model_call_id, output.observed_model, output.model_verification],
                        )?;
                        let parsed = parse_cli_companion_envelope(&output.envelope_json, backend);
                        match parsed {
                            Ok(envelope) => {
                                if exceeds_remaining_read_budget(&envelope, remaining_reads) {
                                    self.finish_cli_model_call(
                                        &model_call_id,
                                        "failed",
                                        latency_ms,
                                        output.detected_cli_version.as_deref(),
                                        output.exit_code,
                                        "read_budget_exceeded",
                                        output.output_bytes,
                                        output.redaction_count,
                                        Some("tool_call_limit"),
                                    )?;
                                    if !repair {
                                        repair = true;
                                        continue;
                                    }
                                    return Err(ChatCommandError::new(
                                        "tool_call_limit",
                                        "The turn has no remaining reads. Ask a narrower question or use the evidence already gathered.",
                                    ));
                                }
                                self.finish_cli_model_call(
                                    &model_call_id,
                                    "completed",
                                    latency_ms,
                                    output.detected_cli_version.as_deref(),
                                    output.exit_code,
                                    "exited",
                                    output.output_bytes,
                                    output.redaction_count,
                                    None,
                                )?;
                                break (model_call_id, envelope);
                            }
                            Err(error) => {
                                self.finish_cli_model_call(
                                    &model_call_id,
                                    "failed",
                                    latency_ms,
                                    output.detected_cli_version.as_deref(),
                                    output.exit_code,
                                    "invalid_envelope",
                                    output.output_bytes,
                                    output.redaction_count,
                                    Some("invalid_control_envelope"),
                                )?;
                                if !repair {
                                    repair = true;
                                    continue;
                                }
                                return Err(error);
                            }
                        }
                    }
                    Err(error) => {
                        // The miss cause (`unavailable_tool`, `schema_rejected`, …)
                        // is kept on the call: a repaired round used to leave
                        // no trace of why its first attempt failed.
                        let envelope_miss = match &error {
                            TerminalError::Process(message)
                                if message.contains("invalid control envelope") =>
                            {
                                Some(
                                    message
                                        .rsplit_once('(')
                                        .and_then(|(_, cause)| cause.strip_suffix(')'))
                                        .unwrap_or("no_envelope"),
                                )
                            }
                            _ => None,
                        };
                        let invalid_envelope = envelope_miss.is_some();
                        self.finish_cli_model_call(
                            &model_call_id,
                            "failed",
                            latency_ms,
                            None,
                            None,
                            envelope_miss.unwrap_or_else(|| terminal_termination_reason(&error)),
                            0,
                            0,
                            Some(if invalid_envelope {
                                "invalid_control_envelope"
                            } else {
                                error.code()
                            }),
                        )?;
                        if invalid_envelope && !repair {
                            repair = true;
                            continue;
                        }
                        return Err(chat_cli_error(error));
                    }
                }
            };

            match envelope {
                CompanionControlEnvelope::Final { text } => {
                    self.stream_visible_content(context, &text).await?;
                    self.connection()?.execute(
                        "UPDATE chat_messages SET
                         input_tokens=(SELECT SUM(input_tokens) FROM chat_model_calls WHERE assistant_message_id=?1),
                         output_tokens=(SELECT SUM(output_tokens) FROM chat_model_calls WHERE assistant_message_id=?1),
                         updated_at=?2 WHERE id=?1",params![context.assistant_message_id,now_iso()])?;
                    return Ok(());
                }
                CompanionControlEnvelope::ToolCalls { calls } => {
                    let calls = calls
                        .into_iter()
                        .map(|call| NormalizedToolCall {
                            id: call.id,
                            name: call.name,
                            arguments: call.arguments,
                        })
                        .collect::<Vec<_>>();
                    total_tool_calls = total_tool_calls.saturating_add(calls.len());
                    if total_tool_calls > MAX_TOOL_CALLS {
                        return Err(ChatCommandError::new(
                            "tool_call_limit",
                            "The turn reached its limit of 24 reads. Narrow the question and try again.",
                        ));
                    }
                    let results = self
                        .execute_tool_calls(
                            context,
                            &repository,
                            &model_call_id,
                            round_index,
                            &calls,
                            false,
                        )
                        .await?;
                    total_tool_bytes = total_tool_bytes.saturating_add(
                        results
                            .iter()
                            .map(|result| result.content.len())
                            .sum::<usize>(),
                    );
                    if total_tool_bytes > MAX_TOOL_RESULT_BYTES {
                        return Err(ChatCommandError::new(
                            "tool_result_limit",
                            "The turn reached 256 KiB of results. Narrow the question and try again.",
                        ));
                    }
                    exchanges.push(ToolExchange {
                        assistant_content: String::new(),
                        calls,
                        results,
                    });
                }
            }
        }
        Err(ChatCommandError::new(
            "model_round_limit",
            "The turn reached its limit of 8 rounds. Narrow the question and try again.",
        ))
    }

    async fn execute_tool_calls(
        &self,
        context: &TurnContext,
        repository: &RepositoryContext,
        model_call_id: &str,
        round_index: usize,
        calls: &[NormalizedToolCall],
        emit_events: bool,
    ) -> Result<Vec<ToolResult>, ChatCommandError> {
        let mut persisted_ids = HashMap::new();
        for call in calls {
            let row_id = Uuid::new_v4().to_string();
            let arguments = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
            self.connection()?.execute(
                "INSERT INTO chat_tool_calls (
                   id, assistant_message_id, model_call_id, round_index, tool_call_id,
                   tool_name, sanitized_arguments, status, started_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'running', ?8)",
                params![
                    row_id,
                    context.assistant_message_id,
                    model_call_id,
                    round_index as i64,
                    call.id,
                    call.name,
                    arguments,
                    now_iso()
                ],
            )?;
            persisted_ids.insert(call.id.clone(), row_id);
            if emit_events {
                self.emit_turn_event(
                    ChatEventKind::ToolStarted,
                    context,
                    ChatEventDetail::ToolStarted {
                        tool_call: ChatToolStarted {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: typed_tool_arguments(&call.arguments),
                            round_index,
                        },
                    },
                )?;
            }
        }

        let executions = stream::iter(calls.iter().cloned())
            .map(|call| {
                let repository = repository.clone();
                let db_path = self.db_path.clone();
                let project_id = context.conversation.project_id.clone();
                let fallback_call = call.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        execute_companion_tool(&db_path, &project_id, &repository, call)
                    })
                    .await
                    .unwrap_or_else(|error| ToolResult {
                        call: fallback_call,
                        content: json!({ "error": format!("Internal tool failure: {error}") })
                            .to_string(),
                        is_error: true,
                    })
                }
            })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;

        for result in &executions {
            let summary = summarize_tool_result(result);
            let status = if result.is_error {
                "failed"
            } else {
                "completed"
            };
            if let Some(row_id) = persisted_ids.get(&result.call.id) {
                self.connection()?.execute(
                    "UPDATE chat_tool_calls
                     SET result_summary = ?1, result_bytes = ?2, status = ?3,
                         completed_at = ?4, error_code = ?5, error_message = ?6
                     WHERE id = ?7",
                    params![
                        summary,
                        result.content.len() as i64,
                        status,
                        now_iso(),
                        result.is_error.then_some("tool_error"),
                        result.is_error.then_some(summary.as_str()),
                        row_id
                    ],
                )?;
            }
            if emit_events {
                self.emit_turn_event(
                    ChatEventKind::ToolCompleted,
                    context,
                    ChatEventDetail::ToolCompleted {
                        tool_call: ChatToolCompleted {
                            id: result.call.id.clone(),
                            name: result.call.name.clone(),
                            status: status.into(),
                            summary,
                            result_bytes: result.content.len(),
                            round_index,
                        },
                    },
                )?;
            }
        }
        Ok(executions)
    }

    async fn stream_visible_content(
        &self,
        context: &TurnContext,
        content: &str,
    ) -> Result<(), ChatCommandError> {
        self.replace_message_content(&context.assistant_message_id, "")?;
        let characters: Vec<char> = content.chars().collect();
        for chunk in characters.chunks(96) {
            if context.cancellation.is_cancelled() {
                return Err(ChatCommandError::new("cancelled", "Resposta cancelada."));
            }
            let delta: String = chunk.iter().collect();
            self.append_message_content(&context.assistant_message_id, &delta)?;
            self.emit_turn_event(
                ChatEventKind::TokenDelta,
                context,
                ChatEventDetail::TokenDelta { delta },
            )?;
            tokio::task::yield_now().await;
        }
        Ok(())
    }

    fn load_protocol_messages(
        &self,
        conversation_id: &str,
        current_assistant_id: &str,
    ) -> Result<Vec<ProtocolMessage>, ChatCommandError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT role, content FROM chat_messages
             WHERE conversation_id = ?1 AND id != ?2 AND content != ''
             ORDER BY ordinal",
        )?;
        let rows = statement.query_map(params![conversation_id, current_assistant_id], |row| {
            Ok(ProtocolMessage {
                role: row.get(0)?,
                content: row.get(1)?,
            })
        })?;
        let messages = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(ChatCommandError::from)?;
        let context_window: Option<u64> = connection
            .query_row(
                "SELECT e.context_window
                 FROM chat_conversations c
                 JOIN cli_model_catalog_snapshots s
                   ON s.project_id=c.project_id AND s.backend=c.cli_backend
                 JOIN cli_model_catalog_entries e
                   ON e.snapshot_id=s.id AND e.model_id=CASE
                     WHEN c.model_id_snapshot='default' THEN '' ELSE c.model_id_snapshot END
                 WHERE c.id=?1",
                [conversation_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let character_budget = context_window
            .map(|tokens| tokens.saturating_mul(2).clamp(40_000, 800_000) as usize)
            .unwrap_or(80_000);
        Ok(project_history(messages, character_budget))
    }

    fn finish_model_call(
        &self,
        id: &str,
        status: &str,
        latency_ms: i64,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cost: Option<f64>,
        error_code: Option<&str>,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_model_calls
             SET completed_at = ?1, latency_ms = ?2, input_tokens = ?3, output_tokens = ?4,
                 estimated_cost_usd = ?5, status = ?6, error_code = ?7
             WHERE id = ?8",
            params![
                now_iso(),
                latency_ms,
                input_tokens,
                output_tokens,
                cost,
                status,
                error_code,
                id
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_cli_model_call(
        &self,
        id: &str,
        status: &str,
        latency_ms: i64,
        detected_cli_version: Option<&str>,
        exit_code: Option<i32>,
        termination_reason: &str,
        output_bytes: usize,
        redaction_count: u64,
        error_code: Option<&str>,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_model_calls
             SET completed_at=?1,latency_ms=?2,status=?3,error_code=?4,
                 detected_cli_version=?5,exit_code=?6,termination_reason=?7,
                 output_bytes=?8,redaction_count=?9,
                 input_tokens=(SELECT SUM(input_tokens) FROM token_usage_records WHERE source_kind='chat_model_call' AND source_id=?10),
                 output_tokens=(SELECT SUM(output_tokens) FROM token_usage_records WHERE source_kind='chat_model_call' AND source_id=?10)
             WHERE id=?10",
            params![
                now_iso(),
                latency_ms,
                status,
                error_code,
                detected_cli_version,
                exit_code,
                termination_reason,
                output_bytes.min(i64::MAX as usize) as i64,
                redaction_count.min(i64::MAX as u64) as i64,
                id,
            ],
        )?;
        Ok(())
    }

    fn update_message_usage(
        &self,
        message_id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost: Option<f64>,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_messages
             SET input_tokens = ?1, output_tokens = ?2, estimated_cost_usd = ?3, updated_at = ?4
             WHERE id = ?5",
            params![input_tokens, output_tokens, cost, now_iso(), message_id],
        )?;
        Ok(())
    }

    fn set_message_status(
        &self,
        message_id: &str,
        status: &str,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_messages
             SET status = ?1, error_code = ?2, error_message = ?3, updated_at = ?4
             WHERE id = ?5",
            params![status, error_code, error_message, now_iso(), message_id],
        )?;
        Ok(())
    }

    fn replace_message_content(
        &self,
        message_id: &str,
        content: &str,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_messages SET content = ?1, updated_at = ?2 WHERE id = ?3",
            params![content, now_iso(), message_id],
        )?;
        Ok(())
    }

    fn append_message_content(
        &self,
        message_id: &str,
        delta: &str,
    ) -> Result<(), ChatCommandError> {
        self.connection()?.execute(
            "UPDATE chat_messages SET content = content || ?1, updated_at = ?2 WHERE id = ?3",
            params![delta, now_iso(), message_id],
        )?;
        Ok(())
    }

    fn emit_turn_event(
        &self,
        kind: ChatEventKind,
        context: &TurnContext,
        detail: ChatEventDetail,
    ) -> Result<(), ChatCommandError> {
        let sequence = {
            let mut sequences = self
                .runtime
                .sequences
                .lock()
                .map_err(|_| runtime_lock_error())?;
            let current = sequences.entry(context.generation_id.clone()).or_insert(0);
            *current += 1;
            *current
        };
        self.connection()?.execute(
            "UPDATE chat_messages SET last_sequence = ?1, updated_at = ?2 WHERE id = ?3",
            params![sequence as i64, now_iso(), context.assistant_message_id],
        )?;
        self.event_sink.emit(&ChatEventEnvelope {
            kind,
            schema_version: 1,
            sequence,
            occurred_at: now_iso(),
            conversation_id: context.conversation.id.clone(),
            assistant_message_id: context.assistant_message_id.clone(),
            execution_id: context.execution_id.clone(),
            generation_id: context.generation_id.clone(),
            detail,
        });
        Ok(())
    }
}

enum TurnOutcome {
    Cancelled,
    Failed(ChatCommandError),
}

fn execute_repository_tool(repository: &RepositoryContext, call: NormalizedToolCall) -> ToolResult {
    let outcome = match call.name.as_str() {
        "list_repository_tree" => {
            serde_json::from_value::<ListRepositoryTreeArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| repository.list_tree(args).map_err(|error| error.0))
                .and_then(|result| {
                    serde_json::to_string(&result).map_err(|error| error.to_string())
                })
        }
        "search_repository" => {
            serde_json::from_value::<SearchRepositoryArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| repository.search(args).map_err(|error| error.0))
                .and_then(|result| {
                    serde_json::to_string(&result).map_err(|error| error.to_string())
                })
        }
        "read_repository_file" => {
            serde_json::from_value::<ReadRepositoryFileArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| repository.read_file(args).map_err(|error| error.0))
                .and_then(|result| {
                    serde_json::to_string(&result).map_err(|error| error.to_string())
                })
        }
        _ => Err("Tool is not in the repository read-only allowlist.".into()),
    };
    match outcome {
        Ok(content) => ToolResult {
            call,
            content,
            is_error: false,
        },
        Err(message) => ToolResult {
            call,
            content: json!({ "error": message }).to_string(),
            is_error: true,
        },
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAgentRunsArgs {
    agent: Option<String>,
    status: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchAgentOutputsArgs {
    query: String,
    agent: Option<String>,
    backend: Option<String>,
    model: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadAgentOutputArgs {
    reference: String,
    start_line: Option<usize>,
    end_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchCliModelsArgs {
    query: Option<String>,
    backend: Option<String>,
    provider: Option<String>,
    capability: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SummarizeAgentModelPerformanceArgs {
    agent: Option<String>,
    backend: Option<String>,
    model: Option<String>,
}

fn execute_companion_tool(
    db_path: &Path,
    project_id: &str,
    repository: &RepositoryContext,
    call: NormalizedToolCall,
) -> ToolResult {
    let outcome = match call.name.as_str() {
        "list_repository_tree" | "search_repository" | "read_repository_file" => {
            let result = execute_repository_tool(repository, call.clone());
            if result.is_error {
                Err(result.content)
            } else {
                Ok(result.content)
            }
        }
        "list_agent_runs" => serde_json::from_value::<ListAgentRunsArgs>(call.arguments.clone())
            .map_err(|error| format!("Invalid arguments: {error}"))
            .and_then(|args| list_agent_runs_tool(db_path, project_id, args)),
        "search_agent_outputs" => {
            serde_json::from_value::<SearchAgentOutputsArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| search_agent_outputs_tool(db_path, project_id, args))
        }
        "read_agent_output" => {
            serde_json::from_value::<ReadAgentOutputArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| read_agent_output_tool(db_path, project_id, args))
        }
        "search_cli_models" => {
            serde_json::from_value::<SearchCliModelsArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| search_cli_models_tool(db_path, project_id, args))
        }
        "summarize_agent_model_performance" => {
            serde_json::from_value::<SummarizeAgentModelPerformanceArgs>(call.arguments.clone())
                .map_err(|error| format!("Invalid arguments: {error}"))
                .and_then(|args| summarize_agent_model_performance_tool(db_path, project_id, args))
        }
        _ => Err("Tool is not in the Companion read-only allowlist.".into()),
    };
    match outcome {
        Ok(content) => match sanitize_companion_tool_content(db_path, project_id, &content) {
            Ok(content) => ToolResult {
                call,
                content,
                is_error: false,
            },
            Err(message) => ToolResult {
                call,
                content: json!({ "error": message }).to_string(),
                is_error: true,
            },
        },
        Err(message) => ToolResult {
            call,
            content: json!({ "error": message }).to_string(),
            is_error: true,
        },
    }
}

fn companion_database(db_path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(db_path).map_err(|error| error.to_string())?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn list_agent_runs_tool(
    db_path: &Path,
    project_id: &str,
    args: ListAgentRunsArgs,
) -> Result<String, String> {
    let connection = companion_database(db_path)?;
    let limit = args.limit.unwrap_or(50).clamp(1, 200) as i64;
    let mut statement = connection
        .prepare(
            "SELECT run_id,agent,pulsation_id,origin,status,backend,model,observed_model,
                    duration_ms,cost_usd,authoritative,created_at
             FROM (
               SELECT tr.id AS run_id,COALESCE(tr.agent_role,tr.free_form_label) AS agent,
                      tr.pulsation_id,tr.origin,tr.status,tr.backend,tr.model,tr.observed_model,
                      CAST(MAX(0,(julianday(COALESCE(tr.finished_at,tr.updated_at))-
                        julianday(COALESCE(tr.started_at,tr.created_at)))*86400000) AS INTEGER) AS duration_ms,
                      NULL AS cost_usd,CASE WHEN tr.status='completed' THEN 1 ELSE 0 END AS authoritative,
                      tr.created_at
               FROM terminal_runs tr WHERE tr.project_id=?1
               UNION ALL
               SELECT ar.id,ar.agent_name,j.pulsation_id,'pipeline_api',ar.status,ar.provider,
                      ar.model,NULL,
                      CAST(MAX(0,(julianday(ar.updated_at)-julianday(ar.created_at))*86400000) AS INTEGER),
                      ar.estimated_cost_usd,CASE WHEN ar.status='completed' THEN 1 ELSE 0 END,
                      ar.created_at
               FROM agent_runs ar
               JOIN jobs j ON j.id=ar.job_id
               JOIN pulsations p ON p.id=j.pulsation_id
               WHERE p.project_id=?1
             )
             WHERE (?2 IS NULL OR agent=?2) AND (?3 IS NULL OR status=?3)
             ORDER BY created_at DESC LIMIT ?4",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![project_id, args.agent, args.status, limit], |row| {
            Ok(json!({
                "runId": row.get::<_, String>(0)?,
                "agent": row.get::<_, Option<String>>(1)?,
                "pulsationId": row.get::<_, Option<String>>(2)?,
                "origin": row.get::<_, String>(3)?,
                "status": row.get::<_, String>(4)?,
                "backend": row.get::<_, Option<String>>(5)?,
                "requestedModel": row.get::<_, Option<String>>(6)?,
                "observedModel": row.get::<_, Option<String>>(7)?,
                "durationMs": row.get::<_, Option<i64>>(8)?,
                "knownCostUsd": row.get::<_, Option<f64>>(9)?,
                "authoritative": row.get::<_, bool>(10)?,
                "createdAt": row.get::<_, String>(11)?,
            }))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&json!({ "runs": rows })).map_err(|error| error.to_string())
}

fn refresh_agent_output_index(connection: &mut Connection, project_id: &str) -> Result<(), String> {
    #[derive(Debug)]
    struct Document {
        reference: String,
        source_kind: String,
        source_id: String,
        agent_role: Option<String>,
        backend: Option<String>,
        model: Option<String>,
        status: String,
        authoritative: bool,
        title: String,
        content: String,
        created_at: String,
        updated_at: String,
    }

    let mut documents = Vec::<Document>::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT o.id,r.agent_name,r.provider,r.model,r.status,
                        CASE WHEN r.status='completed' THEN 1 ELSE 0 END,
                        CASE WHEN o.structured_output IS NOT NULL AND trim(o.structured_output)<>''
                             THEN o.structured_output ELSE o.raw_output END,
                        o.created_at,o.updated_at
                 FROM agent_run_outputs o
                 JOIN agent_runs r ON r.id=o.agent_run_id
                 JOIN jobs j ON j.id=o.job_id
                 JOIN pulsations p ON p.id=COALESCE(o.pulsation_id,j.pulsation_id)
                 WHERE p.project_id=?1",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([project_id], |row| {
                let id: String = row.get(0)?;
                let agent: String = row.get(1)?;
                Ok(Document {
                    reference: format!("agent-run-output:{id}"),
                    source_kind: "agent_run_output".into(),
                    source_id: id,
                    agent_role: Some(agent.clone()),
                    backend: row.get(2)?,
                    model: row.get(3)?,
                    status: row.get(4)?,
                    authoritative: row.get(5)?,
                    title: format!("{agent} agent output"),
                    content: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })
            .map_err(|error| error.to_string())?;
        documents.extend(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?,
        );
    }
    {
        let mut statement = connection
            .prepare(
                "SELECT a.id,a.name,a.producer_stage,a.acceptance,a.content,a.created_at,a.updated_at
                 FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id
                 WHERE p.project_id=?1 AND a.content IS NOT NULL AND trim(a.content)<>''",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([project_id], |row| {
                let id: String = row.get(0)?;
                let status: String = row.get(3)?;
                Ok(Document {
                    reference: format!("artifact:{id}"),
                    source_kind: "artifact".into(),
                    source_id: id,
                    agent_role: row.get(2)?,
                    backend: None,
                    model: None,
                    authoritative: status == "accepted",
                    status,
                    title: row.get(1)?,
                    content: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .map_err(|error| error.to_string())?;
        documents.extend(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?,
        );
    }
    {
        let mut statement = connection
            .prepare(
                "SELECT e.id,e.run_id,e.sequence,COALESCE(r.agent_role,r.free_form_label),
                        r.backend,r.model,r.status,e.content,e.created_at
                 FROM terminal_events e JOIN terminal_runs r ON r.id=e.run_id
                 WHERE r.project_id=?1 AND e.kind IN ('message','tool_result','stdout')",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([project_id], |row| {
                let id: String = row.get(0)?;
                let run_id: String = row.get(1)?;
                let sequence: i64 = row.get(2)?;
                let agent: Option<String> = row.get(3)?;
                let status: String = row.get(6)?;
                Ok(Document {
                    reference: format!("terminal-event:{run_id}:{sequence}"),
                    source_kind: "terminal_event".into(),
                    source_id: id,
                    agent_role: agent.clone(),
                    backend: row.get(4)?,
                    model: row.get(5)?,
                    authoritative: status == "completed",
                    status,
                    title: format!(
                        "{} terminal output",
                        agent.unwrap_or_else(|| "FreeForm".into())
                    ),
                    content: row.get(7)?,
                    created_at: row.get(8)?,
                    updated_at: row.get(8)?,
                })
            })
            .map_err(|error| error.to_string())?;
        documents.extend(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?,
        );
    }

    let workspace_path: String = connection
        .query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for document in documents {
        let content = redact_output_projection(&document.content, &workspace_path)?;
        let content = content.chars().take(512 * 1024).collect::<String>();
        transaction
            .execute(
                "INSERT INTO companion_agent_output_documents(
                   project_id,stable_reference,source_kind,source_id,agent_role,backend,model,
                   status,authoritative,title,content,created_at,updated_at
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(project_id,stable_reference) DO UPDATE SET
                   agent_role=excluded.agent_role,backend=excluded.backend,model=excluded.model,
                   status=excluded.status,authoritative=excluded.authoritative,title=excluded.title,
                   content=excluded.content,updated_at=excluded.updated_at",
                params![
                    project_id,
                    document.reference,
                    document.source_kind,
                    document.source_id,
                    document.agent_role,
                    document.backend,
                    document.model,
                    document.status,
                    document.authoritative,
                    document.title,
                    content,
                    document.created_at,
                    document.updated_at,
                ],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn search_agent_outputs_tool(
    db_path: &Path,
    project_id: &str,
    args: SearchAgentOutputsArgs,
) -> Result<String, String> {
    let query = args.query.trim();
    if query.is_empty() || query.chars().count() > 256 {
        return Err("query must contain between 1 and 256 characters".into());
    }
    let mut connection = companion_database(db_path)?;
    refresh_agent_output_index(&mut connection, project_id)?;
    let fts_query = query
        .split_whitespace()
        .take(16)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ");
    let limit = args.limit.unwrap_or(20).clamp(1, 50) as i64;
    let mut statement = connection
        .prepare(
            "SELECT d.stable_reference,d.source_kind,d.agent_role,d.backend,d.model,d.status,
                    d.authoritative,d.title,
                    snippet(companion_agent_output_fts,1,'','','…',32),d.created_at
             FROM companion_agent_output_fts
             JOIN companion_agent_output_documents d ON d.id=companion_agent_output_fts.rowid
             WHERE companion_agent_output_fts MATCH ?2 AND d.project_id=?1
               AND (?3 IS NULL OR d.agent_role=?3)
               AND (?4 IS NULL OR d.backend=?4)
               AND (?5 IS NULL OR d.model=?5)
             ORDER BY rank,d.created_at DESC LIMIT ?6",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![
                project_id,
                fts_query,
                args.agent,
                args.backend,
                args.model,
                limit
            ],
            |row| {
                Ok(json!({
                    "reference": row.get::<_, String>(0)?,
                    "source": row.get::<_, String>(1)?,
                    "agent": row.get::<_, Option<String>>(2)?,
                    "backend": row.get::<_, Option<String>>(3)?,
                    "model": row.get::<_, Option<String>>(4)?,
                    "status": row.get::<_, String>(5)?,
                    "authoritative": row.get::<_, bool>(6)?,
                    "title": row.get::<_, String>(7)?,
                    "snippet": row.get::<_, String>(8)?,
                    "createdAt": row.get::<_, String>(9)?,
                }))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&json!({ "matches": rows })).map_err(|error| error.to_string())
}

fn read_agent_output_tool(
    db_path: &Path,
    project_id: &str,
    args: ReadAgentOutputArgs,
) -> Result<String, String> {
    let reference = args.reference.trim();
    if reference.is_empty() || reference.len() > 512 {
        return Err("reference is invalid".into());
    }
    let mut connection = companion_database(db_path)?;
    refresh_agent_output_index(&mut connection, project_id)?;
    let row = connection
        .query_row(
            "SELECT source_kind,agent_role,backend,model,status,authoritative,title,content,created_at
             FROM companion_agent_output_documents
             WHERE project_id=?1 AND stable_reference=?2",
            params![project_id, reference],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "output reference was not found in this project".to_string())?;
    let start = args.start_line.unwrap_or(1).max(1);
    let end = args.end_line.unwrap_or(start.saturating_add(199));
    if end < start || end.saturating_sub(start) >= 400 {
        return Err("line range must contain at most 400 lines".into());
    }
    let total_lines = row.7.lines().count();
    let selected_lines = row
        .7
        .lines()
        .enumerate()
        .filter(|(index, _)| *index + 1 >= start && *index < end)
        .map(|(index, line)| format!("{}: {}", index + 1, line))
        .collect::<Vec<_>>();
    let selected_line_count = selected_lines.len();
    let selected_unbounded = selected_lines.join("\n");
    let selected = selected_unbounded
        .chars()
        .take(64 * 1024)
        .collect::<String>();
    let truncated = start > 1
        || start.saturating_sub(1).saturating_add(selected_line_count) < total_lines
        || selected.chars().count() < selected_unbounded.chars().count();
    serde_json::to_string(&json!({
        "reference": reference,
        "source": row.0,
        "agent": row.1,
        "backend": row.2,
        "model": row.3,
        "status": row.4,
        "authoritative": row.5,
        "title": row.6,
        "content": selected,
        "createdAt": row.8,
        "truncated": truncated,
    }))
    .map_err(|error| error.to_string())
}

fn search_cli_models_tool(
    db_path: &Path,
    project_id: &str,
    args: SearchCliModelsArgs,
) -> Result<String, String> {
    if args.backend.as_deref() == Some("custom") {
        return Err("Custom CLI does not expose a verified model catalog".into());
    }
    let connection = companion_database(db_path)?;
    let query = args.query.unwrap_or_default();
    let query = format!("%{}%", query.trim());
    let capability = args
        .capability
        .map(|value| format!("%\"{}\"%", value.trim()));
    let limit = args.limit.unwrap_or(100).clamp(1, 500) as i64;
    let mut statement = connection
        .prepare(
            "SELECT s.backend,s.state,s.source,s.cli_version,s.refreshed_at,e.provider_id,
                    e.model_id,e.display_name,e.context_window,e.input_cost_usd_per_million,
                    e.output_cost_usd_per_million,e.capabilities_json,e.profiles_json
             FROM cli_model_catalog_entries e
             JOIN cli_model_catalog_snapshots s ON s.id=e.snapshot_id
             WHERE s.project_id=?1
               AND (?2 IS NULL OR s.backend=?2)
               AND (?3 IS NULL OR e.provider_id=?3)
               AND (e.model_id LIKE ?4 OR e.display_name LIKE ?4 OR e.capabilities_json LIKE ?4)
               AND (?5 IS NULL OR e.capabilities_json LIKE ?5)
             ORDER BY s.backend,e.provider_id,e.position,e.model_id LIMIT ?6",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![project_id, args.backend, args.provider, query, capability, limit],
            |row| {
                let model_id: String = row.get(6)?;
                let capabilities: String = row.get(11)?;
                let profiles: String = row.get(12)?;
                Ok(json!({
                    "backend": row.get::<_, String>(0)?,
                    "catalogState": row.get::<_, String>(1)?,
                    "catalogSource": row.get::<_, String>(2)?,
                    "cliVersion": row.get::<_, Option<String>>(3)?,
                    "refreshedAt": row.get::<_, String>(4)?,
                    "provider": row.get::<_, Option<String>>(5)?,
                    "modelId": (!model_id.is_empty()).then_some(model_id),
                    "name": row.get::<_, String>(7)?,
                    "contextWindow": row.get::<_, Option<i64>>(8)?,
                    "inputCostUsdPerMillion": row.get::<_, Option<f64>>(9)?,
                    "outputCostUsdPerMillion": row.get::<_, Option<f64>>(10)?,
                    "capabilities": serde_json::from_str::<Value>(&capabilities).unwrap_or_else(|_| json!([])),
                    "profiles": serde_json::from_str::<Value>(&profiles).unwrap_or_else(|_| json!([])),
                }))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&json!({ "models": rows })).map_err(|error| error.to_string())
}

fn summarize_agent_model_performance_tool(
    db_path: &Path,
    project_id: &str,
    args: SummarizeAgentModelPerformanceArgs,
) -> Result<String, String> {
    let connection = companion_database(db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT agent,backend,COALESCE(model,'default') AS model,COUNT(*) AS runs,
                    SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END) AS successes,
                    SUM(CASE WHEN status IN ('failed','interrupted') THEN 1 ELSE 0 END) AS failures,
                    SUM(CASE WHEN termination_reason='timeout' THEN 1 ELSE 0 END) AS timeouts,
                    CAST(AVG(duration_ms) AS INTEGER) AS average_latency_ms,
                    SUM(cost_usd) AS known_cost_usd
             FROM (
               SELECT COALESCE(tr.agent_role,tr.free_form_label) AS agent,tr.backend,tr.model,tr.status,
                      tr.termination_reason,
                      MAX(0,(julianday(COALESCE(tr.finished_at,tr.updated_at))-
                        julianday(COALESCE(tr.started_at,tr.created_at)))*86400000) AS duration_ms,
                      NULL AS cost_usd
               FROM terminal_runs tr WHERE tr.project_id=?1
               UNION ALL
               SELECT ar.agent_name,ar.provider,ar.model,ar.status,NULL,
                      MAX(0,(julianday(ar.updated_at)-julianday(ar.created_at))*86400000),
                      ar.estimated_cost_usd
               FROM agent_runs ar JOIN jobs j ON j.id=ar.job_id
               JOIN pulsations p ON p.id=j.pulsation_id WHERE p.project_id=?1
             )
             WHERE (?2 IS NULL OR agent=?2) AND (?3 IS NULL OR backend=?3)
               AND (?4 IS NULL OR model=?4)
             GROUP BY agent,backend,COALESCE(model,'default')
             ORDER BY runs DESC,agent,backend,model LIMIT 200",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(
            params![project_id, args.agent, args.backend, args.model],
            |row| {
                Ok(json!({
                    "agent": row.get::<_, Option<String>>(0)?,
                    "backend": row.get::<_, Option<String>>(1)?,
                    "model": row.get::<_, String>(2)?,
                    "runs": row.get::<_, i64>(3)?,
                    "successes": row.get::<_, i64>(4)?,
                    "failures": row.get::<_, i64>(5)?,
                    "timeouts": row.get::<_, i64>(6)?,
                    "averageLatencyMs": row.get::<_, Option<i64>>(7)?,
                    "knownCostUsd": row.get::<_, Option<f64>>(8)?,
                }))
            },
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&json!({ "performance": rows })).map_err(|error| error.to_string())
}

fn sanitize_companion_tool_content(
    db_path: &Path,
    project_id: &str,
    content: &str,
) -> Result<String, String> {
    let connection = companion_database(db_path)?;
    let workspace_path: String = connection
        .query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [project_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    redact_output_projection(content, &workspace_path)
}

fn redact_output_projection(content: &str, workspace_path: &str) -> Result<String, String> {
    static WINDOWS_PATH: OnceLock<Regex> = OnceLock::new();
    static POSIX_PATH: OnceLock<Regex> = OnceLock::new();
    let mut content = redact_secrets(content)?.text;
    if !workspace_path.is_empty() {
        content = content.replace(workspace_path, "[WORKSPACE]");
        content = content.replace(&workspace_path.replace('\\', "/"), "[WORKSPACE]");
    }
    let windows_path = WINDOWS_PATH.get_or_init(|| {
        Regex::new(r#"(?i)(?:[A-Z]:\\|\\\\)[^\s\"'<>|]+"#)
            .expect("Windows absolute path regex must compile")
    });
    content = windows_path
        .replace_all(&content, "[REDACTED ABSOLUTE PATH]")
        .into_owned();
    let posix_path = POSIX_PATH.get_or_init(|| {
        Regex::new(r#"(^|[\s\"'])/[^\s\"'<>|]+"#).expect("POSIX absolute path regex must compile")
    });
    Ok(posix_path
        .replace_all(&content, "$1[REDACTED ABSOLUTE PATH]")
        .into_owned())
}

fn summarize_tool_result(result: &ToolResult) -> String {
    if result.is_error {
        return serde_json::from_str::<Value>(&result.content)
            .ok()
            .and_then(|value| value.get("error")?.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| "Leitura rejeitada.".into())
            .chars()
            .take(300)
            .collect();
    }
    match result.call.name.as_str() {
        "list_repository_tree" => "Repository tree read.".into(),
        "search_repository" => "Repository search finished.".into(),
        "read_repository_file" => "File excerpt read.".into(),
        _ => "Tool finished.".into(),
    }
}

impl ChatService {
    async fn call_model(
        &self,
        profile: &ProviderProfile,
        connection: &ResolvedConnection,
        model: &str,
        api_key: Option<&str>,
        context: &TurnContext,
        messages: &[ProtocolMessage],
        exchanges: &[ToolExchange],
        usage: &UsageRecorder,
    ) -> Result<ModelResponse, ModelFailure> {
        let payload = match profile.protocol {
            ProviderProtocol::OpenaiResponses => {
                build_openai_responses_payload(model, messages, exchanges)
            }
            ProviderProtocol::OpenaiChat => build_openai_chat_payload(model, messages, exchanges),
            ProviderProtocol::AnthropicMessages => {
                build_anthropic_payload(model, messages, exchanges)
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        match profile.auth_style {
            AuthStyle::Bearer => {
                let secret = api_key.ok_or_else(|| ModelFailure {
                    code: "credential_unavailable".into(),
                    message: "The connection has no credential configured.".into(),
                    retryable: false,
                })?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|_| {
                        ModelFailure {
                            code: "invalid_credential".into(),
                            message: "The credential contains invalid characters.".into(),
                            retryable: false,
                        }
                    })?,
                );
            }
            AuthStyle::Anthropic => {
                let secret = api_key.ok_or_else(|| ModelFailure {
                    code: "credential_unavailable".into(),
                    message: "The connection has no credential configured.".into(),
                    retryable: false,
                })?;
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(secret).map_err(|_| ModelFailure {
                        code: "invalid_credential".into(),
                        message: "The credential contains invalid characters.".into(),
                        retryable: false,
                    })?,
                );
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
            AuthStyle::None => {}
        }

        let response = self
            .http_client
            .post(&connection.endpoint_url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|error| ModelFailure {
                code: if error.is_timeout() {
                    "provider_timeout"
                } else {
                    "provider_unavailable"
                }
                .into(),
                message: if error.is_timeout() {
                    "The provider did not answer in time."
                } else {
                    "Could not reach the configured provider."
                }
                .into(),
                retryable: true,
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.map_err(|_| ModelFailure {
                code: "invalid_response".into(),
                message: "Could not read the provider's error response.".into(),
                retryable: false,
            })?;
            let value: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
            usage.observe(&crate::usage::api::parse_usage(profile.protocol, &value));
            let message = provider_error_message(&value);
            let classified = profile.classify_http_error(
                status.as_u16(),
                provider_error_code(&value).as_deref(),
                &message,
                None,
            );
            return Err(ModelFailure {
                code: classified.kind.code().to_string(),
                message: classified.message,
                retryable: status.as_u16() == 408
                    || status.as_u16() == 429
                    || status.is_server_error(),
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.contains("application/json") {
            let body = response
                .text()
                .await
                .map_err(|_| invalid_response("Could not read the provider's response."))?;
            let value: Value = serde_json::from_str(&body)
                .map_err(|_| invalid_response("The provider returned an invalid JSON response."))?;
            usage.observe(&crate::usage::api::parse_usage(profile.protocol, &value));
            return match profile.protocol {
                ProviderProtocol::OpenaiResponses => parse_openai_responses(&value),
                ProviderProtocol::OpenaiChat => parse_openai_chat(&value),
                ProviderProtocol::AnthropicMessages => parse_anthropic(&value),
            };
        }
        self.parse_streaming_response(response, profile.protocol, context, usage)
            .await
    }

    async fn parse_streaming_response(
        &self,
        response: reqwest::Response,
        protocol: ProviderProtocol,
        context: &TurnContext,
        usage: &UsageRecorder,
    ) -> Result<ModelResponse, ModelFailure> {
        let mut parser = SseParser::new(protocol);
        let mut stream = response.bytes_stream();
        let mut content = String::new();
        let mut partial_calls = BTreeMap::<usize, PartialToolCall>::new();
        let mut input_tokens = None;
        let mut output_tokens = None;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ModelFailure {
                code: "stream_failed".into(),
                message: "The provider stream was interrupted.".into(),
                retryable: content.is_empty(),
            })?;
            let parsed_events = parser.push(&chunk);
            usage.observe(parser.usage());
            let events = parsed_events.map_err(|error| ModelFailure {
                code: "stream_failed".into(),
                message: error.message,
                retryable: content.is_empty(),
            })?;
            self.consume_stream_events(
                events,
                context,
                &mut content,
                &mut partial_calls,
                &mut input_tokens,
                &mut output_tokens,
            )?;
        }
        let parsed_events = parser.finish();
        usage.observe(parser.usage());
        let events = parsed_events.map_err(|error| ModelFailure {
            code: "stream_failed".into(),
            message: error.message,
            retryable: content.is_empty(),
        })?;
        self.consume_stream_events(
            events,
            context,
            &mut content,
            &mut partial_calls,
            &mut input_tokens,
            &mut output_tokens,
        )?;
        let tool_calls = partial_calls
            .into_values()
            .map(|call| {
                let arguments = if call.arguments.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&call.arguments).map_err(|_| ModelFailure {
                        code: "invalid_tool_arguments".into(),
                        message: "The provider returned invalid fragmented tool arguments.".into(),
                        retryable: false,
                    })?
                };
                Ok(NormalizedToolCall {
                    id: if call.id.is_empty() {
                        "tool-call".into()
                    } else {
                        call.id
                    },
                    name: call.name,
                    arguments,
                })
            })
            .collect::<Result<Vec<_>, ModelFailure>>()?;
        Ok(ModelResponse {
            content,
            tool_calls,
            input_tokens,
            output_tokens,
            streamed: true,
        })
    }

    fn consume_stream_events(
        &self,
        events: Vec<ParsedStreamEvent>,
        context: &TurnContext,
        content: &mut String,
        partial_calls: &mut BTreeMap<usize, PartialToolCall>,
        input_tokens: &mut Option<i64>,
        output_tokens: &mut Option<i64>,
    ) -> Result<(), ModelFailure> {
        for event in events {
            match event {
                ParsedStreamEvent::Delta(delta) => {
                    content.push_str(&delta);
                    self.append_message_content(&context.assistant_message_id, &delta)
                        .map_err(model_persistence_failure)?;
                    self.emit_turn_event(
                        ChatEventKind::TokenDelta,
                        context,
                        ChatEventDetail::TokenDelta { delta },
                    )
                    .map_err(model_persistence_failure)?;
                }
                ParsedStreamEvent::ToolCallStarted { index, id, name } => {
                    let partial = partial_calls.entry(index).or_default();
                    merge_fragment(&mut partial.id, &id);
                    merge_fragment(&mut partial.name, &name);
                }
                ParsedStreamEvent::ToolArgumentsDelta { index, delta } => {
                    partial_calls
                        .entry(index)
                        .or_default()
                        .arguments
                        .push_str(&delta);
                }
                ParsedStreamEvent::Usage {
                    input_tokens: input,
                    output_tokens: output,
                } => {
                    if input.is_some() {
                        *input_tokens = input;
                    }
                    if output.is_some() {
                        *output_tokens = output;
                    }
                }
                ParsedStreamEvent::ToolCallCompleted { .. } | ParsedStreamEvent::Completed => {}
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn merge_fragment(target: &mut String, fragment: &str) {
    if fragment.is_empty() || target == fragment || target.ends_with(fragment) {
        return;
    }
    target.push_str(fragment);
}

fn model_persistence_failure(error: ChatCommandError) -> ModelFailure {
    ModelFailure {
        code: error.code,
        message: error.message,
        retryable: false,
    }
}

fn system_prompt() -> &'static str {
    "You are the Hard Vibe Code Chat Companion. Analyse architecture, explain code and recommend CLI and model settings in text, using only general knowledge and the read-only local tools provided. Consult agent outputs only on demand through a tool; they are never automatic context. Never ask for or try to use a shell, Git, the network, the web, writes, renames, plugins, external MCP or remote tools. Never change settings and never claim to have applied changes. When you propose code changes, use a copyable unified diff and say explicitly that it is a proposal that was not applied. Distinguish observed evidence, catalog data, uncertainty and opinion. Answer in the developer's language."
}

fn empty_json_object() -> Value {
    json!({})
}

fn parse_companion_control_envelope(
    payload: &str,
) -> Result<CompanionControlEnvelope, ChatCommandError> {
    let envelope: CompanionControlEnvelope = serde_json::from_str(payload).map_err(|_| {
        ChatCommandError::new(
            "invalid_control_envelope",
            "The Companion CLI returned an invalid control envelope.",
        )
    })?;
    match &envelope {
        CompanionControlEnvelope::Final { text } => {
            if text.trim().is_empty() || text.chars().count() > 100_000 {
                return Err(ChatCommandError::new(
                    "invalid_control_envelope",
                    "The Companion CLI returned invalid final text.",
                ));
            }
        }
        CompanionControlEnvelope::ToolCalls { calls } => {
            if calls.is_empty() || calls.len() > MAX_TOOL_CALLS {
                return Err(ChatCommandError::new(
                    "invalid_control_envelope",
                    "The Companion CLI returned an invalid tool call count.",
                ));
            }
            for call in calls {
                if call.id.trim().is_empty()
                    || call.id.len() > 256
                    || !is_companion_tool(&call.name)
                    || !call.arguments.is_object()
                {
                    return Err(ChatCommandError::new(
                        "invalid_control_envelope",
                        "The Companion CLI returned an invalid read-only tool call.",
                    ));
                }
            }
        }
    }
    Ok(envelope)
}

fn companion_control_envelope_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "type": { "const": "final" },
                    "text": { "type": "string", "minLength": 1, "maxLength": 100000 }
                },
                "required": ["type", "text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "tool_calls" },
                    "calls": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 24,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "minLength": 1, "maxLength": 256 },
                                "name": {
                                    "type": "string",
                                    "enum": companion_tool_names()
                                },
                                "arguments": { "type": "object" }
                            },
                            "required": ["id", "name", "arguments"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["type", "calls"],
                "additionalProperties": false
            }
        ]
    })
}

// Codex Structured Outputs requires a closed root object and fully required
// properties. Keep the host envelope unchanged behind a provider-only wrapper.
// Claude Code turns `--json-schema` into a tool whose input schema must be an
// object at the root: the bare `oneOf` envelope was rejected by the API
// (`tools.0.custom.input_schema.type: Field required`) on every call. The
// envelope extractor finds the inner `final`/`tool_calls` object either way.
fn companion_schema_for_backend(backend: CliBackend) -> Value {
    let schema = companion_control_envelope_schema();
    if backend == CliBackend::Claude {
        return json!({
            "type": "object",
            "properties": {"response": {"anyOf": schema["oneOf"].clone()}},
            "required": ["response"],
            "additionalProperties": false
        });
    }
    if backend != CliBackend::Codex {
        return schema;
    }
    let calls = repository_tool_schemas()
        .into_iter()
        .map(|(name, _, mut arguments)| {
            let required = arguments
                .get("required")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let properties = arguments["properties"]
                .as_object_mut()
                .expect("tool properties");
            for (name, property) in properties.iter_mut() {
                if !required
                    .as_array()
                    .is_some_and(|keys| keys.contains(&json!(name)))
                {
                    *property = json!({"anyOf": [property.clone(), {"type": "null"}]});
                }
            }
            let keys = properties.keys().cloned().collect::<Vec<_>>();
            arguments["required"] = json!(keys);
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1, "maxLength": 256},
                    "name": {"type": "string", "enum": [name]},
                    "arguments": arguments
                },
                "required": ["id", "name", "arguments"],
                "additionalProperties": false
            })
        })
        .collect::<Vec<_>>();
    let mut tools = schema["oneOf"][1].clone();
    tools["properties"]["type"] = json!({"type": "string", "enum": ["tool_calls"]});
    tools["properties"]["calls"]["items"] = json!({"anyOf": calls});
    let mut final_response = schema["oneOf"][0].clone();
    final_response["properties"]["type"] = json!({"type": "string", "enum": ["final"]});
    json!({
        "type": "object",
        "properties": {"response": {"anyOf": [final_response, tools]}},
        "required": ["response"],
        "additionalProperties": false
    })
}

fn parse_cli_companion_envelope(
    payload: &str,
    backend: CliBackend,
) -> Result<CompanionControlEnvelope, ChatCommandError> {
    if backend != CliBackend::Codex {
        return parse_companion_control_envelope(payload);
    }
    let mut envelope: Value = serde_json::from_str(payload).map_err(|_| {
        ChatCommandError::new(
            "invalid_control_envelope",
            "The Companion CLI returned an invalid control envelope.",
        )
    })?;
    // The terminal adapter extracts the inner envelope. Only optional fields
    // declared by the host may use null to represent omission on the wire.
    if let Some(calls) = envelope.get_mut("calls").and_then(Value::as_array_mut) {
        let schemas = repository_tool_schemas();
        for call in calls {
            let schema = schemas
                .iter()
                .find(|(name, _, _)| call.get("name").and_then(Value::as_str) == Some(name));
            if let Some((_, _, schema)) = schema
                && let Some(arguments) = call.get_mut("arguments").and_then(Value::as_object_mut)
            {
                arguments.retain(|name, value| {
                    !value.is_null()
                        || schema["properties"].get(name).is_none()
                        || schema["required"]
                            .as_array()
                            .is_some_and(|keys| keys.contains(&json!(name)))
                });
            }
        }
    }
    parse_companion_control_envelope(&envelope.to_string())
}

fn build_cli_companion_prompt(
    messages: &[ProtocolMessage],
    exchanges: &[ToolExchange],
    repair: bool,
    backend: CliBackend,
    remaining_reads: usize,
) -> String {
    let messages = messages
        .iter()
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect::<Vec<_>>();
    let exchanges = exchanges
        .iter()
        .map(|exchange| {
            json!({
                "toolCalls": exchange.calls.iter().map(|call| json!({
                    "id": call.id,
                    "name": call.name,
                    "arguments": call.arguments
                })).collect::<Vec<_>>(),
                "toolResults": exchange.results.iter().map(|result| json!({
                    "id": result.call.id,
                    "name": result.call.name,
                    "content": result.content,
                    "isError": result.is_error
                })).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let repair_instruction = if repair {
        "Your previous response exceeded the control envelope or remaining read budget. Return exactly one valid envelope within the remaining budget, or a final answer using evidence already gathered."
    } else {
        ""
    };
    // Claude Code delivers `--json-schema` through a StructuredOutput tool.
    // Listed like tool definitions, the repository operations were called as
    // native tools: "No such tool available" five to nine times per round, and
    // some rounds never reached StructuredOutput ("invalid control envelope",
    // 2026-09-26; reproduced 3/3, fixed 3/3 by this wording). Left alone, Haiku
    // also wrote the whole answer as text and again inside the tool (1713 →
    // 860 tokens and 16.6 → 9.2 s on the same question).
    let delivery = if backend == CliBackend::Claude {
        "\nYour only callable tool is StructuredOutput; any other tool call fails. Call it once per reply with `response` set to the envelope: {\"type\":\"tool_calls\",\"calls\":[...]} to run operations, or {\"type\":\"final\",\"text\":\"...\"} to answer. Do not write the answer as plain text before or after that call: text outside it is discarded."
    } else {
        ""
    };
    format!(
        "<system>\n{}\nYou have no native tools. The repository operations in <operations> are not tools you can call: to run them, return a tool_calls envelope naming them. This turn has {remaining_reads} reads remaining in total; never request more than that number. If none remain, return a final answer from the evidence already gathered and state any limit. The host runs requested operations and returns the results in <tool_exchanges>. When enough evidence is available, return a final envelope. Never include reasoning, traces, Markdown fences, or text outside the envelope.{}\n<operations>\n{}\n</operations>\n</system>\n<history>\n{}\n</history>\n<tool_exchanges>\n{}\n</tool_exchanges>\n<control_envelope_schema>\n{}\n</control_envelope_schema>\n{}",
        system_prompt(),
        delivery,
        serde_json::to_string(&repository_tool_schemas()).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(&messages).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(&exchanges).unwrap_or_else(|_| "[]".into()),
        serde_json::to_string(&companion_schema_for_backend(backend))
            .unwrap_or_else(|_| "{}".into()),
        repair_instruction,
    )
}

fn terminal_termination_reason(error: &TerminalError) -> &'static str {
    match error {
        TerminalError::Cancelled => "cancelled",
        TerminalError::Timeout => "timeout",
        TerminalError::OutputLimit => "output_limit",
        _ => "failed",
    }
}

fn chat_cli_error(error: TerminalError) -> ChatCommandError {
    let code = error.code().to_string();
    let message = match error {
        TerminalError::AuthRequired(backend) => format!(
            "{backend} requires authentication in its own CLI. Sign in there and try again."
        ),
        TerminalError::VersionUnsupported(backend) => {
            format!("The installed {backend} version is not supported by this adapter.")
        }
        TerminalError::Timeout => "The Companion CLI reached the 120 second timeout.".into(),
        TerminalError::Cancelled => "Resposta cancelada.".into(),
        TerminalError::OutputLimit => "The Companion CLI exceeded the safe output limit.".into(),
        TerminalError::NotFound(_) => "The selected CLI is not installed or discoverable.".into(),
        TerminalError::Validation(message) => {
            let message = format!(
                "{message}. Select an available model/profile (or CLI default) and create a new conversation. Existing conversations keep their original model."
            );
            redact_secrets(&message).map_or_else(
                |_| "The Companion CLI configuration is invalid.".into(),
                |v| v.text,
            )
        }
        TerminalError::Process(message) => {
            redact_secrets(&message).map_or_else(|_| "The Companion CLI failed.".into(), |v| v.text)
        }
        _ => "The Companion CLI could not complete this turn.".into(),
    };
    ChatCommandError::new(code, message)
}

fn tool_definitions_openai_responses() -> Vec<Value> {
    repository_tool_schemas()
        .into_iter()
        .map(|(name, description, parameters)| {
            json!({
                "type": "function",
                "name": name,
                "description": description,
                "parameters": parameters
            })
        })
        .collect()
}

fn tool_definitions_openai_chat() -> Vec<Value> {
    repository_tool_schemas()
        .into_iter()
        .map(|(name, description, parameters)| {
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters
                }
            })
        })
        .collect()
}

fn tool_definitions_anthropic() -> Vec<Value> {
    repository_tool_schemas()
        .into_iter()
        .map(|(name, description, parameters)| {
            json!({
                "name": name,
                "description": description,
                "input_schema": parameters
            })
        })
        .collect()
}

fn companion_tool_names() -> Vec<&'static str> {
    vec![
        "list_repository_tree",
        "search_repository",
        "read_repository_file",
        "list_agent_runs",
        "search_agent_outputs",
        "read_agent_output",
        "search_cli_models",
        "summarize_agent_model_performance",
    ]
}

fn is_companion_tool(name: &str) -> bool {
    companion_tool_names().contains(&name)
}

fn repository_tool_schemas() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "list_repository_tree",
            "List the safe repository tree using relative paths.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 8 },
                    "maxEntries": { "type": "integer", "minimum": 1, "maximum": 1000 }
                },
                "additionalProperties": false
            }),
        ),
        (
            "search_repository",
            "Search literal text or a regular expression only in allowed repository text files.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "path": { "type": "string" },
                    "mode": { "type": "string", "enum": ["literal", "regex"] },
                    "maxResults": { "type": "integer", "minimum": 1, "maximum": 50 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        (
            "read_repository_file",
            "Read up to 400 lines or 64 KiB from an allowed text file with numbered lines.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "startLine": { "type": "integer", "minimum": 1 },
                    "endLine": { "type": "integer", "minimum": 1 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        (
            "list_agent_runs",
            "List project-scoped canonical and FreeForm agent runs with status, CLI, requested model, observed model, duration, and known cost.",
            json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string" },
                    "status": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "additionalProperties": false
            }),
        ),
        (
            "search_agent_outputs",
            "Search the project-scoped FTS index of persisted API, CLI, manual, FreeForm, and artifact outputs. Results identify non-authoritative failed, cancelled, or partial runs.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "agent": { "type": "string" },
                    "backend": { "type": "string" },
                    "model": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        ),
        (
            "read_agent_output",
            "Read a bounded line range from one stable output reference returned by search_agent_outputs.",
            json!({
                "type": "object",
                "properties": {
                    "reference": { "type": "string", "minLength": 1, "maxLength": 512 },
                    "startLine": { "type": "integer", "minimum": 1 },
                    "endLine": { "type": "integer", "minimum": 1 }
                },
                "required": ["reference"],
                "additionalProperties": false
            }),
        ),
        (
            "search_cli_models",
            "Search the normalized project-scoped CLI model catalog by backend, provider, ID, name, or capability.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "maxLength": 256 },
                    "backend": { "type": "string", "enum": ["codex", "claude", "gemini", "opencode", "antigravity"] },
                    "provider": { "type": "string" },
                    "capability": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500 }
                },
                "additionalProperties": false
            }),
        ),
        (
            "summarize_agent_model_performance",
            "Aggregate project-scoped run count, success, failure, timeout, average latency, and known cost by agent, backend, and model.",
            json!({
                "type": "object",
                "properties": {
                    "agent": { "type": "string" },
                    "backend": { "type": "string" },
                    "model": { "type": "string" }
                },
                "additionalProperties": false
            }),
        ),
    ]
}

fn build_openai_responses_payload(
    model: &str,
    messages: &[ProtocolMessage],
    exchanges: &[ToolExchange],
) -> Value {
    let mut input: Vec<Value> = messages
        .iter()
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect();
    for exchange in exchanges {
        if !exchange.assistant_content.is_empty() {
            input.push(json!({ "role": "assistant", "content": exchange.assistant_content }));
        }
        for call in &exchange.calls {
            input.push(json!({
                "type": "function_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into())
            }));
        }
        for result in &exchange.results {
            input.push(json!({
                "type": "function_call_output",
                "call_id": result.call.id,
                "output": result.content
            }));
        }
    }
    json!({
        "model": model,
        "instructions": system_prompt(),
        "input": input,
        "tools": tool_definitions_openai_responses(),
        "parallel_tool_calls": true,
        "max_output_tokens": 4096,
        "stream": true
    })
}

fn build_openai_chat_payload(
    model: &str,
    messages: &[ProtocolMessage],
    exchanges: &[ToolExchange],
) -> Value {
    let mut wire_messages = vec![json!({ "role": "system", "content": system_prompt() })];
    wire_messages.extend(
        messages
            .iter()
            .map(|message| json!({ "role": message.role, "content": message.content })),
    );
    for exchange in exchanges {
        wire_messages.push(json!({
            "role": "assistant",
            "content": exchange.assistant_content,
            "tool_calls": exchange.calls.iter().map(|call| json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into())
                }
            })).collect::<Vec<_>>()
        }));
        wire_messages.extend(exchange.results.iter().map(|result| {
            json!({
                "role": "tool",
                "tool_call_id": result.call.id,
                "content": result.content
            })
        }));
    }
    json!({
        "model": model,
        "messages": wire_messages,
        "tools": tool_definitions_openai_chat(),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "max_tokens": 4096,
        "stream_options": { "include_usage": true },
        "stream": true
    })
}

fn build_anthropic_payload(
    model: &str,
    messages: &[ProtocolMessage],
    exchanges: &[ToolExchange],
) -> Value {
    let mut wire_messages: Vec<Value> = messages
        .iter()
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect();
    for exchange in exchanges {
        let mut assistant_blocks = Vec::new();
        if !exchange.assistant_content.is_empty() {
            assistant_blocks.push(json!({ "type": "text", "text": exchange.assistant_content }));
        }
        assistant_blocks.extend(exchange.calls.iter().map(|call| {
            json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.arguments
            })
        }));
        wire_messages.push(json!({ "role": "assistant", "content": assistant_blocks }));
        wire_messages.push(json!({
            "role": "user",
            "content": exchange.results.iter().map(|result| json!({
                "type": "tool_result",
                "tool_use_id": result.call.id,
                "content": result.content,
                "is_error": result.is_error
            })).collect::<Vec<_>>()
        }));
    }
    json!({
        "model": model,
        "system": system_prompt(),
        "messages": wire_messages,
        "tools": tool_definitions_anthropic(),
        "max_tokens": 4096,
        "stream": true
    })
}

fn parse_openai_responses(value: &Value) -> Result<ModelResponse, ModelFailure> {
    let mut content = value
        .get("output_text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut tool_calls = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("function_call") => {
                    let arguments = parse_arguments(item.get("arguments"))?;
                    tool_calls.push(NormalizedToolCall {
                        id: item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool-call")
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        arguments,
                    });
                }
                Some("message") if content.is_empty() => {
                    content = item
                        .get("content")
                        .and_then(Value::as_array)
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter_map(|block| block.get("text").and_then(Value::as_str))
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
    }
    Ok(ModelResponse {
        content,
        tool_calls,
        input_tokens: value.pointer("/usage/input_tokens").and_then(Value::as_i64),
        output_tokens: value
            .pointer("/usage/output_tokens")
            .and_then(Value::as_i64),
        streamed: false,
    })
}

fn parse_openai_chat(value: &Value) -> Result<ModelResponse, ModelFailure> {
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| invalid_response("OpenAI-compatible response without a message."))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            tool_calls.push(NormalizedToolCall {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .to_string(),
                name: call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arguments: parse_arguments(call.pointer("/function/arguments"))?,
            });
        }
    }
    Ok(ModelResponse {
        content,
        tool_calls,
        input_tokens: value
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_i64),
        output_tokens: value
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_i64),
        streamed: false,
    })
}

fn parse_anthropic(value: &Value) -> Result<ModelResponse, ModelFailure> {
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("Anthropic response without content."))?;
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push_str(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            Some("tool_use") => tool_calls.push(NormalizedToolCall {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("tool-call")
                    .to_string(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arguments: block.get("input").cloned().unwrap_or_else(|| json!({})),
            }),
            _ => {}
        }
    }
    Ok(ModelResponse {
        content,
        tool_calls,
        input_tokens: value.pointer("/usage/input_tokens").and_then(Value::as_i64),
        output_tokens: value
            .pointer("/usage/output_tokens")
            .and_then(Value::as_i64),
        streamed: false,
    })
}

fn parse_arguments(value: Option<&Value>) -> Result<Value, ModelFailure> {
    match value {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::String(arguments)) => {
            serde_json::from_str(arguments).map_err(|_| ModelFailure {
                code: "invalid_tool_arguments".into(),
                message: "The provider returned invalid tool arguments.".into(),
                retryable: false,
            })
        }
        Some(arguments @ Value::Object(_)) => Ok(arguments.clone()),
        _ => Err(ModelFailure {
            code: "invalid_tool_arguments".into(),
            message: "The provider returned invalid tool arguments.".into(),
            retryable: false,
        }),
    }
}

fn invalid_response(message: &str) -> ModelFailure {
    ModelFailure {
        code: "invalid_response".into(),
        message: message.into(),
        retryable: false,
    }
}

fn provider_error_message(value: &Value) -> String {
    value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("The provider returned an error without a message.")
        .chars()
        .take(500)
        .collect()
}

fn provider_error_code(value: &Value) -> Option<String> {
    value
        .pointer("/error/code")
        .or_else(|| value.get("code"))
        .and_then(|code| {
            code.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| Some(code.to_string()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::error::LlmError;
    use crate::persistence_service::PersistenceService;

    #[derive(Default)]
    struct TestCredentials;

    impl CredentialStore for TestCredentials {
        fn get(&self, _credential_ref: &str) -> Result<Option<String>, LlmError> {
            Ok(Some("test-secret".into()))
        }

        fn set(&self, _credential_ref: &str, _secret: &str) -> Result<(), LlmError> {
            Ok(())
        }

        fn delete(&self, _credential_ref: &str) -> Result<bool, LlmError> {
            Ok(true)
        }
    }

    fn prepare_chat() -> (tempfile::TempDir, PathBuf, Arc<ChatService>) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let persistence = PersistenceService::initialize(temp.path()).unwrap();
        let database = Connection::open(persistence.database_path()).unwrap();
        database.pragma_update(None, "foreign_keys", "ON").unwrap();
        let now = now_iso();
        database
            .execute(
                "INSERT INTO projects (id, name, workspace_path, is_archived, created_at, updated_at)
                 VALUES ('project-a', 'Project A', ?1, 0, ?2, ?2)",
                params![workspace.to_string_lossy(), now],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO projects (id, name, workspace_path, is_archived, created_at, updated_at)
                 VALUES ('project-b', 'Project B', ?1, 0, ?2, ?2)",
                params![temp.path().join("other").to_string_lossy(), now],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO provider_connections (
                   id, provider_id, display_name, credential_ref, default_model, enabled,
                   state, max_concurrency, version, created_at, updated_at
                 ) VALUES ('connection-a', 'openai', 'OpenAI test', 'test-ref',
                           'gpt-4.1-mini', 1, 'active', 1, 1, ?1, ?1)",
                params![now],
            )
            .unwrap();
        let service = Arc::new(ChatService::new(
            persistence.database_path(),
            Arc::new(TestCredentials),
            Arc::new(NoopChatEventSink),
        ));
        (temp, workspace, service)
    }

    #[test]
    fn conversation_crud_is_project_scoped_and_snapshots_model() {
        let (_temp, _workspace, service) = prepare_chat();
        let created = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        assert_eq!(created.model_id, "gpt-4.1-mini");
        assert!(created.connection_available);
        assert_eq!(
            service
                .list_conversations(ListChatConversationsRequest {
                    project_id: "project-a".into()
                })
                .unwrap()
                .conversations
                .len(),
            1
        );
        assert!(
            service
                .list_conversations(ListChatConversationsRequest {
                    project_id: "project-b".into()
                })
                .unwrap()
                .conversations
                .is_empty()
        );

        service
            .connection()
            .unwrap()
            .execute(
                "UPDATE provider_connections SET default_model = 'unknown-new-model' WHERE id = 'connection-a'",
                [],
            )
            .unwrap();
        let loaded = service
            .get_conversation(GetChatConversationRequest {
                conversation_id: created.id.clone(),
            })
            .unwrap();
        assert_eq!(loaded.conversation.model_id, "gpt-4.1-mini");

        assert!(
            service
                .delete_conversation(DeleteChatConversationRequest {
                    conversation_id: created.id.clone(),
                })
                .unwrap()
                .deleted
        );
        assert!(
            service
                .get_conversation(GetChatConversationRequest {
                    conversation_id: created.id
                })
                .is_err()
        );
    }

    #[test]
    fn removal_keeps_history_and_startup_recovers_interrupted_messages() {
        let (_temp, _workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        let database = service.connection().unwrap();
        let now = now_iso();
        database
            .execute(
                "INSERT INTO chat_messages (
                   id, conversation_id, ordinal, role, content, status, generation_id,
                   created_at, updated_at
                 ) VALUES ('assistant-a', ?1, 0, 'assistant', 'parcial', 'streaming',
                           'generation-a', ?2, ?2)",
                params![conversation.id, now],
            )
            .unwrap();
        assert_eq!(service.recover_interrupted_turns().unwrap(), 1);
        database
            .execute(
                "DELETE FROM provider_connections WHERE id = 'connection-a'",
                [],
            )
            .unwrap();
        let loaded = service
            .get_conversation(GetChatConversationRequest {
                conversation_id: conversation.id,
            })
            .unwrap();
        assert_eq!(loaded.messages[0].status, "interrupted");
        assert_eq!(loaded.messages[0].content, "parcial");
        assert!(loaded.conversation.connection_id.is_none());
        assert!(!loaded.conversation.connection_available);
    }

    /// 2026-09-26: a CLI turn timed out during its seventh round, and that
    /// round's call stayed `running`; other messages' rounds are untouched.
    #[test]
    fn a_turn_that_ends_early_closes_its_round_in_flight() {
        let (_temp, _workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        let database = service.connection().unwrap();
        let now = now_iso();
        for (ordinal, message) in ["assistant-timed-out", "assistant-other"]
            .iter()
            .enumerate()
        {
            database
                .execute(
                    "INSERT INTO chat_messages (
                       id, conversation_id, ordinal, role, content, status, generation_id,
                       created_at, updated_at
                     ) VALUES (?1, ?2, ?3, 'assistant', '', 'streaming', ?1, ?4, ?4)",
                    params![
                        message,
                        conversation.id.as_str(),
                        ordinal as i64,
                        now.as_str()
                    ],
                )
                .unwrap();
            database
                .execute(
                    "INSERT INTO chat_model_calls (
                       id, assistant_message_id, round_index, provider_id, model_id, started_at, status
                     ) VALUES (?1, ?2, 6, 'claude', 'haiku', ?3, 'running')",
                    params![format!("call-{message}"), message, now.as_str()],
                )
                .unwrap();
        }
        service
            .close_running_model_calls("assistant-timed-out", "turn_timeout")
            .unwrap();
        let state = |id: &str| -> (String, Option<String>, Option<String>) {
            database
                .query_row(
                    "SELECT status, error_code, completed_at FROM chat_model_calls WHERE id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        };
        let closed = state("call-assistant-timed-out");
        assert_eq!(closed.0, "failed");
        assert_eq!(closed.1.as_deref(), Some("turn_timeout"));
        assert!(closed.2.is_some());
        assert_eq!(state("call-assistant-other").0, "running");
    }

    #[test]
    fn reopened_service_recovers_pending_messages_and_running_child_records() {
        let (_temp, _workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        let database = service.connection().unwrap();
        let now = now_iso();
        database
            .execute(
                "INSERT INTO chat_messages (
                   id, conversation_id, ordinal, role, content, status, generation_id,
                   created_at, updated_at
                 ) VALUES ('assistant-pending', ?1, 0, 'assistant', '', 'pending',
                           'generation-pending', ?2, ?2)",
                params![conversation.id.as_str(), now.as_str()],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO chat_messages (
                   id, conversation_id, ordinal, role, content, status, generation_id,
                   created_at, updated_at
                 ) VALUES ('assistant-streaming', ?1, 1, 'assistant', 'resposta parcial',
                           'streaming', 'generation-streaming', ?2, ?2)",
                params![conversation.id.as_str(), now.as_str()],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO chat_model_calls (
                   id, assistant_message_id, round_index, provider_id, model_id, started_at, status
                 ) VALUES ('model-running','assistant-streaming',0,'openai','gpt-4.1-mini',?1,'running')",
                params![now.as_str()],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO chat_tool_calls (
                   id, assistant_message_id, model_call_id, round_index, tool_call_id,
                   tool_name, sanitized_arguments, status, started_at
                 ) VALUES (
                   'tool-running','assistant-streaming','model-running',0,'provider-call-1',
                   'read_repository_file','{\"path\":\"README.md\"}','running',?1
                 )",
                params![now],
            )
            .unwrap();
        let db_path = service.db_path.clone();
        drop(database);
        drop(service);

        let reopened = ChatService::new(
            db_path,
            Arc::new(TestCredentials),
            Arc::new(NoopChatEventSink),
        );
        assert_eq!(reopened.recover_interrupted_turns().unwrap(), 2);
        let loaded = reopened
            .get_conversation(GetChatConversationRequest {
                conversation_id: conversation.id,
            })
            .unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].status, "interrupted");
        assert_eq!(loaded.messages[0].content, "");
        assert_eq!(loaded.messages[1].status, "interrupted");
        assert_eq!(loaded.messages[1].content, "resposta parcial");
        assert_eq!(loaded.tool_calls.len(), 1);
        assert_eq!(loaded.tool_calls[0].status, "interrupted");

        let database = reopened.connection().unwrap();
        let model_status: String = database
            .query_row(
                "SELECT status FROM chat_model_calls WHERE id = 'model-running'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(model_status, "interrupted");
    }

    #[test]
    fn deleting_conversation_cascades_history_and_clears_saved_selection() {
        let (_temp, workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        let database = service.connection().unwrap();
        let now = now_iso();
        database
            .execute(
                "INSERT INTO ui_sessions (
                   project_id, workspace_root, workspace_view_mode, active_view,
                   sidebar_collapsed, context_drawer_open, active_chat_conversation_id, updated_at
                 ) VALUES ('project-a',?1,'editor','chat',0,1,?2,?3)",
                params![
                    workspace.to_string_lossy(),
                    conversation.id.as_str(),
                    now.as_str()
                ],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO chat_messages (
                   id, conversation_id, ordinal, role, content, status, created_at, updated_at
                 ) VALUES ('assistant-delete',?1,0,'assistant','done','completed',?2,?2)",
                params![conversation.id.as_str(), now.as_str()],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO chat_model_calls (
                   id, assistant_message_id, round_index, provider_id, model_id, started_at, status
                 ) VALUES ('model-delete','assistant-delete',0,'openai','gpt-4.1-mini',?1,'completed')",
                params![now.as_str()],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO chat_tool_calls (
                   id, assistant_message_id, model_call_id, round_index, tool_call_id,
                   tool_name, sanitized_arguments, status, started_at
                 ) VALUES (
                   'tool-delete','assistant-delete','model-delete',0,'provider-delete',
                   'search_repository','{}','completed',?1
                 )",
                params![now],
            )
            .unwrap();
        drop(database);

        assert!(
            service
                .delete_conversation(DeleteChatConversationRequest {
                    conversation_id: conversation.id.clone(),
                })
                .unwrap()
                .deleted
        );

        let database = service.connection().unwrap();
        for table in [
            "chat_conversations",
            "chat_messages",
            "chat_model_calls",
            "chat_tool_calls",
        ] {
            let count: i64 = database
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "{table} should be empty after cascade delete");
        }
        let selected: Option<String> = database
            .query_row(
                "SELECT active_chat_conversation_id FROM ui_sessions WHERE project_id = 'project-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(selected.is_none());
    }

    #[test]
    fn starting_turn_rolls_back_user_and_assistant_messages_as_one_unit() {
        let (_temp, workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        let database = service.connection().unwrap();
        database
            .execute_batch(
                "CREATE TRIGGER fail_assistant_insert
                 BEFORE INSERT ON chat_messages
                 WHEN NEW.role = 'assistant'
                 BEGIN
                   SELECT RAISE(ABORT, 'forced assistant insert failure');
                 END;",
            )
            .unwrap();
        drop(database);

        let failed = service.start_turn(
            SendChatMessageRequest {
                conversation_id: conversation.id.clone(),
                content: "Persist both messages".into(),
                expected_target: None,
            },
            &workspace,
        );
        assert!(failed.is_err());

        let database = service.connection().unwrap();
        let message_count: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE conversation_id = ?1",
                params![conversation.id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let title: String = database
            .query_row(
                "SELECT title FROM chat_conversations WHERE id = ?1",
                params![conversation.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(message_count, 0);
        assert_eq!(title, "Nova conversa");
    }

    #[test]
    fn cancellation_finishes_the_turn_and_wait_for_idle_observes_it() {
        let (_temp, workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Api {
                    connection_id: "connection-a".into(),
                },
            })
            .unwrap()
            .conversation;
        let started = service
            .start_turn(
                SendChatMessageRequest {
                    conversation_id: conversation.id.clone(),
                    content: "Cancele antes de iniciar a rede".into(),
                    expected_target: None,
                },
                &workspace,
            )
            .unwrap();

        assert!(
            service
                .cancel_turn(CancelChatTurnRequest {
                    execution_id: started.execution_id,
                })
                .unwrap()
                .cancelled
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(runtime.block_on(service.wait_for_idle(Duration::from_secs(2))));

        let snapshot = service
            .get_turn_snapshot(GetChatTurnSnapshotRequest {
                generation_id: started.generation_id,
            })
            .unwrap();
        assert_eq!(snapshot.message.status, "cancelled");
        assert_eq!(snapshot.message.error_code.as_deref(), Some("cancelled"));
        assert_eq!(
            snapshot.message.target_label.as_deref(),
            Some("openai / gpt-4.1-mini")
        );
        service
            .update_conversation_target(UpdateChatConversationTargetRequest {
                conversation_id: conversation.id.clone(),
                target: ChatExecutionTargetRequest::Cli {
                    backend: CliBackend::Codex,
                    model: None,
                    model_profile: None,
                },
            })
            .unwrap();
        let historical = service
            .get_conversation(GetChatConversationRequest {
                conversation_id: conversation.id,
            })
            .unwrap();
        assert_eq!(
            historical.messages[1].target_label,
            snapshot.message.target_label
        );
    }

    #[test]
    fn complete_protocol_parsers_normalize_tool_calls() {
        let responses = parse_openai_responses(&json!({
            "output": [{"type":"function_call","call_id":"c1","name":"read_repository_file","arguments":"{\"path\":\"src/lib.rs\"}"}],
            "usage":{"input_tokens":10,"output_tokens":2}
        }))
        .unwrap();
        let chat = parse_openai_chat(&json!({
            "choices":[{"message":{"content":null,"tool_calls":[{"id":"c2","function":{"name":"search_repository","arguments":"{\"query\":\"AppView\"}"}}]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":8,"completion_tokens":3}
        }))
        .unwrap();
        let anthropic = parse_anthropic(&json!({
            "content":[{"type":"tool_use","id":"c3","name":"list_repository_tree","input":{"depth":2}}],
            "usage":{"input_tokens":6,"output_tokens":4}
        }))
        .unwrap();
        assert_eq!(responses.tool_calls[0].name, "read_repository_file");
        assert_eq!(chat.tool_calls[0].name, "search_repository");
        assert_eq!(anthropic.tool_calls[0].name, "list_repository_tree");
    }

    #[test]
    fn codex_companion_schema_accepts_final_and_typed_tools_with_strict_objects() {
        fn assert_strict(schema: &Value) {
            if schema["type"] == "object" {
                assert_eq!(schema["additionalProperties"], false);
                let properties = schema["properties"].as_object().unwrap();
                let required = schema["required"].as_array().unwrap();
                assert_eq!(properties.len(), required.len());
                assert!(properties.keys().all(|key| required.contains(&json!(key))));
                for property in properties.values() {
                    assert_strict(property);
                }
            }
            assert!(schema.get("oneOf").is_none());
            if let Some(branches) = schema["anyOf"].as_array() {
                for branch in branches {
                    assert_strict(branch);
                }
            }
            if let Some(items) = schema.get("items") {
                assert_strict(items);
            }
        }
        let schema = companion_schema_for_backend(CliBackend::Codex);
        assert_eq!(schema["type"], "object");
        assert!(schema.get("anyOf").is_none());
        assert_strict(&schema);
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&json!({"response":{"type":"final","text":"Ready"}})));
        let tool = json!({"type":"tool_calls","calls":[{
            "id":"read-1", "name":"read_repository_file",
            "arguments":{"path":"src/lib.rs","startLine":null,"endLine":null}
        }]});
        assert!(validator.is_valid(&json!({"response":tool})));
        let CompanionControlEnvelope::ToolCalls { calls } =
            parse_cli_companion_envelope(&tool.to_string(), CliBackend::Codex).unwrap()
        else {
            panic!("expected tool call")
        };
        assert_eq!(calls[0].arguments, json!({"path":"src/lib.rs"}));
        for invalid in [
            json!({"response":{"type":"final","text":"Ready","reasoning":"hidden"}}),
            json!({"response":{"type":"tool_calls","calls":[{"id":"1","name":"write_file","arguments":{}}]}}),
            json!({"response":{"type":"tool_calls","calls":[{"id":"1","name":"read_repository_file","arguments":{"path":null,"startLine":null,"endLine":null}}]}}),
        ] {
            assert!(!validator.is_valid(&invalid));
        }
        // Claude Code rejects a tool input schema without a root object type
        // (`tools.0.custom.input_schema.type: Field required`).
        let claude = companion_schema_for_backend(CliBackend::Claude);
        assert_eq!(claude["type"], "object");
        assert_eq!(claude["required"], json!(["response"]));
        assert!(claude.get("oneOf").is_none() && claude.get("$schema").is_none());
        let claude_validator = jsonschema::validator_for(&claude).unwrap();
        assert!(claude_validator.is_valid(&json!({"response":{"type":"final","text":"Ready"}})));
        assert!(!claude_validator.is_valid(&json!({"type":"final","text":"Ready"})));
        let claude_result = r#"{"type":"result","is_error":false,"structured_output":{"response":{"type":"final","text":"Ready"}}}"#;
        let inner = crate::terminals::extract_companion_envelope_json(claude_result)
            .expect("the inner envelope is found under `response`");
        assert!(matches!(
            parse_cli_companion_envelope(&inner, CliBackend::Claude).unwrap(),
            CompanionControlEnvelope::Final { text } if text == "Ready"
        ));
        for backend in [
            CliBackend::Gemini,
            CliBackend::Opencode,
            CliBackend::Antigravity,
        ] {
            assert_eq!(
                companion_schema_for_backend(backend),
                companion_control_envelope_schema()
            );
        }
    }

    #[test]
    fn cli_control_envelope_is_strict_and_allows_only_read_only_tools() {
        let unavailable = chat_cli_error(TerminalError::Validation(
            "model legacy-model is not present in the project-scoped Codex catalog".into(),
        ));
        assert_eq!(unavailable.code, "cli_configuration_invalid");
        assert!(unavailable.message.contains("legacy-model"));
        assert!(unavailable.message.contains("create a new conversation"));
        assert!(matches!(
            parse_companion_control_envelope(r#"{"type":"final","text":"Ready"}"#).unwrap(),
            CompanionControlEnvelope::Final { text } if text == "Ready"
        ));
        assert!(matches!(
            parse_companion_control_envelope(
                r#"{"type":"tool_calls","calls":[{"id":"call-1","name":"search_agent_outputs","arguments":{"query":"latency"}}]}"#
            )
            .unwrap(),
            CompanionControlEnvelope::ToolCalls { calls } if calls.len() == 1
        ));
        for invalid in [
            r#"{"type":"final","text":""}"#,
            r#"{"type":"final","text":"Ready","reasoning":"hidden"}"#,
            r#"{"type":"tool_calls","calls":[]}"#,
            r#"{"type":"tool_calls","calls":[{"id":"call-1","name":"write_file","arguments":{}}]}"#,
            "not-json",
        ] {
            assert!(
                parse_companion_control_envelope(invalid).is_err(),
                "invalid envelope was accepted: {invalid}"
            );
        }
    }

    #[test]
    fn history_projection_keeps_the_current_message_and_marks_omissions() {
        let projected = project_history(
            vec![
                ProtocolMessage {
                    role: "user".into(),
                    content: "old-message-that-does-not-fit".into(),
                },
                ProtocolMessage {
                    role: "assistant".into(),
                    content: "recent".into(),
                },
                ProtocolMessage {
                    role: "user".into(),
                    content: "current-message-is-always-complete".into(),
                },
            ],
            8,
        );
        assert_eq!(projected.len(), 2);
        assert!(projected[0].content.contains("were omitted"));
        assert_eq!(projected[1].content, "current-message-is-always-complete");
    }

    #[test]
    fn cli_conversation_target_survives_restart_and_invalid_targets_are_refused() {
        let (_temp, _workspace, service) = prepare_chat();
        let created = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: ChatExecutionTargetRequest::Cli {
                    backend: CliBackend::Claude,
                    model: Some("unverified-model".into()),
                    model_profile: None,
                },
            })
            .unwrap()
            .conversation;
        assert_eq!(created.transport, ChatTransportKind::Cli);
        assert_eq!(created.cli_backend, Some(CliBackend::Claude));
        assert_eq!(created.model_id, "unverified-model");
        assert_eq!(created.model_source, CliModelSelectionSource::Manual);

        let database_path = service.db_path.clone();
        drop(service);
        let reopened = ChatService::new(
            database_path,
            Arc::new(TestCredentials),
            Arc::new(NoopChatEventSink),
        );
        let loaded = reopened
            .get_conversation(GetChatConversationRequest {
                conversation_id: created.id,
            })
            .unwrap()
            .conversation;
        assert_eq!(loaded.transport, ChatTransportKind::Cli);
        assert_eq!(loaded.cli_backend, Some(CliBackend::Claude));
        assert_eq!(loaded.model_id, "unverified-model");
        assert_eq!(loaded.model_profile, None);
        assert_eq!(loaded.model_source, CliModelSelectionSource::Manual);

        let invalid_profile = reopened.create_conversation(CreateChatConversationRequest {
            project_id: "project-a".into(),
            target: ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Claude,
                model: Some("unverified-model".into()),
                model_profile: Some("high".into()),
            },
        });
        assert!(invalid_profile.is_err());
        let invalid_model = reopened.create_conversation(CreateChatConversationRequest {
            project_id: "project-a".into(),
            target: ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Claude,
                model: Some("model with spaces".into()),
                model_profile: None,
            },
        });
        assert!(invalid_model.is_err());
    }

    /// ADR-014 amendment (2026-09-22): the Companion panel applies to the open
    /// conversation, and the project remembers the last chosen target across
    /// restarts. A running reply and an invalid target leave it unchanged.
    #[test]
    fn v43_backfills_historical_assistant_target_without_changing_user_messages() {
        let database = Connection::open_in_memory().unwrap();
        database
            .execute_batch(
                "CREATE TABLE chat_messages(id TEXT PRIMARY KEY, role TEXT NOT NULL);
                 CREATE TABLE chat_model_calls(
                   id TEXT PRIMARY KEY, assistant_message_id TEXT, provider_id TEXT,
                   model_id TEXT, model_profile TEXT, started_at TEXT
                 );
                 INSERT INTO chat_messages VALUES('user-1','user'),('assistant-1','assistant');
                 INSERT INTO chat_model_calls VALUES(
                   'call-1','assistant-1','codex','gpt-5.6-luna','low','2026-09-26T00:00:00Z'
                 );",
            )
            .unwrap();
        database
            .execute_batch(include_str!(
                "../migrations/V43__companion_turn_target_and_model_evidence.sql"
            ))
            .unwrap();
        let historical: Option<String> = database
            .query_row(
                "SELECT target_label FROM chat_messages WHERE id='assistant-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let user: Option<String> = database
            .query_row(
                "SELECT target_label FROM chat_messages WHERE id='user-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let verification: String = database
            .query_row(
                "SELECT model_verification FROM chat_model_calls WHERE id='call-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(historical.as_deref(), Some("codex / gpt-5.6-luna / low"));
        assert_eq!(user, None);
        assert_eq!(verification, "unverified");
    }

    #[test]
    fn a_conversation_is_retargeted_and_the_project_remembers_the_target() {
        let (_temp, _workspace, service) = prepare_chat();
        assert_eq!(service.default_target("project-a").unwrap(), None);
        let claude = ChatExecutionTargetRequest::Cli {
            backend: CliBackend::Claude,
            model: Some("haiku".into()),
            model_profile: None,
        };
        let created = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                target: claude.clone(),
            })
            .unwrap()
            .conversation;
        assert_eq!(
            service.default_target("project-a").unwrap(),
            Some(claude.clone())
        );

        let codex = ChatExecutionTargetRequest::Cli {
            backend: CliBackend::Codex,
            model: None,
            model_profile: None,
        };
        let retargeted = service
            .update_conversation_target(UpdateChatConversationTargetRequest {
                conversation_id: created.id.clone(),
                target: codex.clone(),
            })
            .unwrap()
            .conversation;
        assert_eq!(retargeted.cli_backend, Some(CliBackend::Codex));
        assert_eq!(retargeted.model_id, "default");
        assert_eq!(retargeted.model_source, CliModelSelectionSource::Default);

        // The remembered target survives a restart of the service.
        let database_path = service.db_path.clone();
        drop(service);
        let reopened = Arc::new(ChatService::new(
            database_path,
            Arc::new(TestCredentials),
            Arc::new(NoopChatEventSink),
        ));
        assert_eq!(
            reopened.default_target("project-a").unwrap(),
            Some(codex.clone())
        );

        // An invalid target is refused and changes nothing.
        let invalid = reopened.update_conversation_target(UpdateChatConversationTargetRequest {
            conversation_id: created.id.clone(),
            target: ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Claude,
                model: Some("model with spaces".into()),
                model_profile: None,
            },
        });
        assert!(invalid.is_err());
        assert_eq!(reopened.default_target("project-a").unwrap(), Some(codex));

        // A stale panel selection cannot start a turn on the retargeted model.
        let stale_send = reopened.start_turn(
            SendChatMessageRequest {
                conversation_id: created.id.clone(),
                content: "Use the model shown in the panel".into(),
                expected_target: Some(claude.clone()),
            },
            _workspace.as_path(),
        );
        assert_eq!(stale_send.unwrap_err().code, "target_changed");
        let message_count: i64 = reopened
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE conversation_id=?1",
                [&created.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(message_count, 0);

        // A reply in progress keeps the conversation on its current target.
        reopened
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO chat_messages (
                   id, conversation_id, ordinal, role, content, status, generation_id,
                   created_at, updated_at
                 ) VALUES ('assistant-a', ?1, 0, 'assistant', 'parcial', 'streaming',
                           'generation-a', ?2, ?2)",
                params![created.id, now_iso()],
            )
            .unwrap();
        let busy = reopened
            .update_conversation_target(UpdateChatConversationTargetRequest {
                conversation_id: created.id.clone(),
                target: claude,
            })
            .unwrap_err();
        assert_eq!(busy.code, "conversation_busy");
        let loaded = reopened
            .get_conversation(GetChatConversationRequest {
                conversation_id: created.id,
            })
            .unwrap()
            .conversation;
        assert_eq!(loaded.cli_backend, Some(CliBackend::Codex));
    }

    #[test]
    fn companion_read_budget_rejects_an_overlarge_round_before_execution() {
        let calls = (0..21)
            .map(|index| CompanionControlCall {
                id: format!("read-{index}"),
                name: "list_repository_tree".into(),
                arguments: json!({}),
            })
            .collect();
        let envelope = CompanionControlEnvelope::ToolCalls { calls };
        assert!(exceeds_remaining_read_budget(&envelope, 20));
        assert!(!exceeds_remaining_read_budget(&envelope, 21));
        assert!(exceeds_remaining_read_budget(&envelope, 0));
        assert!(!exceeds_remaining_read_budget(
            &CompanionControlEnvelope::Final {
                text: "Done".into()
            },
            0,
        ));
    }

    #[test]
    fn agent_outputs_are_project_scoped_and_only_enter_prompts_after_a_tool_call() {
        let (_temp, _workspace, service) = prepare_chat();
        let database = service.connection().unwrap();
        let now = now_iso();
        for (project, reference, content) in [
            ("project-a", "artifact:project-a", "alpha-private-evidence"),
            ("project-b", "artifact:project-b", "beta-private-evidence"),
        ] {
            database
                .execute(
                    "INSERT INTO companion_agent_output_documents(
                       project_id,stable_reference,source_kind,source_id,status,authoritative,
                       title,content,created_at,updated_at
                     ) VALUES(?1,?2,'artifact',?2,'accepted',1,'Agent evidence',?3,?4,?4)",
                    params![project, reference, content, now],
                )
                .unwrap();
        }
        drop(database);

        let prompt = build_cli_companion_prompt(
            &[ProtocolMessage {
                role: "user".into(),
                content: "Which model should I use?".into(),
            }],
            &[],
            false,
            CliBackend::Codex,
            20,
        );
        assert!(prompt.contains("20 reads remaining in total"));
        assert!(!prompt.contains("alpha-private-evidence"));
        assert!(!prompt.contains("beta-private-evidence"));
        // Only Claude answers through a StructuredOutput tool.
        assert!(!prompt.contains("StructuredOutput"));
        let claude_prompt = build_cli_companion_prompt(
            &[ProtocolMessage {
                role: "user".into(),
                content: "Which model should I use?".into(),
            }],
            &[],
            false,
            CliBackend::Claude,
            0,
        );
        assert!(claude_prompt.contains("0 reads remaining in total"));
        assert!(claude_prompt.contains("Your only callable tool is StructuredOutput"));
        // Operations are framed as requests, never as callable tools.
        for rendered in [&prompt, &claude_prompt] {
            assert!(rendered.contains("are not tools you can call"));
            assert!(rendered.contains("<operations>\n[[\"list_repository_tree\""));
        }

        let matches = search_agent_outputs_tool(
            &service.db_path,
            "project-a",
            SearchAgentOutputsArgs {
                query: "private evidence".into(),
                agent: None,
                backend: None,
                model: None,
                limit: None,
            },
        )
        .unwrap();
        assert!(matches.contains("artifact:project-a"));
        assert!(!matches.contains("artifact:project-b"));
        assert!(
            read_agent_output_tool(
                &service.db_path,
                "project-a",
                ReadAgentOutputArgs {
                    reference: "artifact:project-b".into(),
                    start_line: None,
                    end_line: None,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn exposes_only_the_eight_read_only_companion_tools() {
        let names = repository_tool_schemas()
            .into_iter()
            .map(|(name, _, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "list_repository_tree",
                "search_repository",
                "read_repository_file",
                "list_agent_runs",
                "search_agent_outputs",
                "read_agent_output",
                "search_cli_models",
                "summarize_agent_model_performance"
            ]
        );
        let serialized = serde_json::to_string(&tool_definitions_openai_chat()).unwrap();
        assert!(!serialized.contains("apply"));
        assert!(!serialized.contains("shell"));
        assert!(!serialized.contains("write"));
    }
}
