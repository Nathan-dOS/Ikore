use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::{StreamExt, stream};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::runtime::Handle;
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

const MAX_MODEL_ROUNDS: usize = 8;
const MAX_TOOL_CALLS: usize = 24;
const MAX_TOOL_RESULT_BYTES: usize = 256 * 1024;
const TURN_TIMEOUT: Duration = Duration::from_secs(120);
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
            format!("Falha ao persistir o chat: {error}"),
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
    pub arguments: Value,
    pub round_index: usize,
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
    pub title: String,
    pub connection_available: bool,
    pub created_at: String,
    pub updated_at: String,
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
    pub sanitized_arguments: Value,
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
    pub connection_id: String,
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
        let mut service = Self::new(db_path, credentials, event_sink);
        service.runtime_handle = Some(runtime_handle);
        service
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
                    title, created_at, updated_at
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

    pub fn create_conversation(
        &self,
        request: CreateChatConversationRequest,
    ) -> Result<CreateChatConversationResponse, ChatCommandError> {
        let project_id = normalize_id(&request.project_id, "projectId")?;
        let connection_id = normalize_id(&request.connection_id, "connectionId")?;
        let database = self.connection()?;
        let project_exists: bool = database.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1 AND is_archived = 0)",
            params![project_id],
            |row| row.get(0),
        )?;
        if !project_exists {
            return Err(ChatCommandError::new(
                "project_not_found",
                "Projeto ativo não encontrado.",
            ));
        }
        let provider_connection = self
            .llm_repository
            .find_resolved_connection(&connection_id)
            .map_err(|error| ChatCommandError::new("connection_not_found", error.message))?;
        self.ensure_connection_eligible(&provider_connection)?;

        let id = Uuid::new_v4().to_string();
        let now = now_iso();
        database.execute(
            "INSERT INTO chat_conversations (
               id, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
               title, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'Nova conversa', ?6, ?6)",
            params![
                id,
                project_id,
                connection_id,
                provider_connection.provider_id.as_str(),
                provider_connection.default_model,
                now
            ],
        )?;
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
                "Cancele a resposta em andamento antes de excluir a conversa.",
            ));
        }
        let database = self.connection()?;
        let transaction = database.unchecked_transaction()?;
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
                "A mensagem deve conter entre 1 e 20.000 caracteres.",
            ));
        }
        let conversation = self.get_conversation_record(&conversation_id)?;
        let connection_id = conversation.connection_id.clone().ok_or_else(|| {
            ChatCommandError::new(
                "connection_removed",
                "A conexão desta conversa foi removida. Crie uma nova conversa.",
            )
        })?;
        let provider_connection = self
            .llm_repository
            .find_resolved_connection(&connection_id)
            .map_err(|_| {
                ChatCommandError::new(
                    "connection_removed",
                    "A conexão desta conversa não existe mais. Crie uma nova conversa.",
                )
            })?;
        if provider_connection.provider_id.as_str() != conversation.provider_id {
            return Err(ChatCommandError::new(
                "connection_changed",
                "O provider da conexão mudou. Crie uma nova conversa.",
            ));
        }
        self.ensure_connection_eligible_for_model(&provider_connection, &conversation.model_id)?;
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
                "Esta conversa já possui uma resposta em andamento.",
            ));
        }

        let execution_id = Uuid::new_v4().to_string();
        let generation_id = Uuid::new_v4().to_string();
        let user_message_id = Uuid::new_v4().to_string();
        let assistant_message_id = Uuid::new_v4().to_string();
        let now = now_iso();
        let database = self.connection()?;
        let transaction = database.unchecked_transaction()?;
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
               generation_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, 'assistant', '', 'pending', ?4, ?5, ?6, ?6)",
            params![
                assistant_message_id,
                conversation_id,
                next_ordinal + 1,
                execution_id,
                generation_id,
                now
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
                        .expect("runtime temporário de chat deve inicializar");
                    runtime.block_on(turn);
                })
                .map_err(|error| {
                    ChatCommandError::new(
                        "runtime_unavailable",
                        format!("Falha ao iniciar turno de chat: {error}"),
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
                        estimated_cost_usd, error_code, error_message, created_at, updated_at
                 FROM chat_messages WHERE generation_id = ?1",
                params![generation_id],
                map_message,
            )
            .optional()?
            .ok_or_else(|| ChatCommandError::new("turn_not_found", "Turno não encontrado."))?;
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
            .ok_or_else(|| ChatCommandError::new("project_not_found", "Projeto não encontrado."))?;
        let expected = Path::new(&expected).canonicalize().map_err(|_| {
            ChatCommandError::new("workspace_mismatch", "Workspace do projeto não existe.")
        })?;
        let actual = workspace_root.canonicalize().map_err(|_| {
            ChatCommandError::new("workspace_mismatch", "Workspace ativo inválido.")
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
            ChatCommandError::new("provider_incompatible", "Provider sem perfil compatível.")
        })?;
        let model = model_entry(connection.provider_id.as_str(), model).ok_or_else(|| {
            ChatCommandError::new(
                "model_incompatible",
                "O modelo não está marcado como compatível com tool calling no catálogo versionado.",
            )
        })?;
        if !connection.enabled || connection.state == "blocked" {
            return Err(ChatCommandError::new(
                "connection_unavailable",
                "A conexão está desabilitada ou bloqueada.",
            ));
        }
        if !profile.capabilities.tool_calling || !model.tool_calling {
            return Err(ChatCommandError::new(
                "model_incompatible",
                "A conexão e o modelo precisam oferecer tool calling.",
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
                "A conexão não possui credencial configurada.",
            ));
        }
        Ok(())
    }

    fn get_conversation_record(&self, id: &str) -> Result<ConversationRecord, ChatCommandError> {
        self.connection()?
            .query_row(
                "SELECT id, project_id, connection_id, provider_id_snapshot, model_id_snapshot,
                        title, created_at, updated_at
                 FROM chat_conversations WHERE id = ?1",
                params![id],
                map_conversation_record,
            )
            .optional()?
            .ok_or_else(|| {
                ChatCommandError::new("conversation_not_found", "Conversa não encontrada.")
            })
    }

    fn decorate_conversation(
        &self,
        record: ConversationRecord,
    ) -> Result<ChatConversation, ChatCommandError> {
        let connection_available = record.connection_id.as_ref().is_some_and(|id| {
            self.llm_repository
                .find_resolved_connection(id)
                .ok()
                .filter(|connection| connection.provider_id.as_str() == record.provider_id)
                .and_then(|connection| {
                    self.ensure_connection_eligible_for_model(&connection, &record.model_id)
                        .ok()
                })
                .is_some()
        });
        Ok(ChatConversation {
            id: record.id,
            project_id: record.project_id,
            connection_id: record.connection_id,
            provider_id: record.provider_id,
            model_id: record.model_id,
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
                    estimated_cost_usd, error_code, error_message, created_at, updated_at
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
    title: String,
    created_at: String,
    updated_at: String,
}

fn map_conversation_record(row: &Row<'_>) -> rusqlite::Result<ConversationRecord> {
    Ok(ConversationRecord {
        id: row.get(0)?,
        project_id: row.get(1)?,
        connection_id: row.get(2)?,
        provider_id: row.get(3)?,
        model_id: row.get(4)?,
        title: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
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
    })
}

fn map_tool_call(row: &Row<'_>) -> rusqlite::Result<ChatToolCall> {
    let arguments: String = row.get(5)?;
    Ok(ChatToolCall {
        id: row.get(0)?,
        assistant_message_id: row.get(1)?,
        round_index: row.get::<_, i64>(2)?.max(0) as usize,
        tool_call_id: row.get(3)?,
        tool_name: row.get(4)?,
        sanitized_arguments: serde_json::from_str(&arguments).unwrap_or_else(|_| json!({})),
        result_summary: row.get(6)?,
        result_bytes: row.get::<_, i64>(7)?.max(0) as usize,
        status: row.get(8)?,
        error_code: row.get(9)?,
        error_message: row.get(10)?,
        started_at: row.get(11)?,
        completed_at: row.get(12)?,
    })
}

fn normalize_id(raw: &str, field: &str) -> Result<String, ChatCommandError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ChatCommandError::new(
            "invalid_request",
            format!("{field} não pode ser vazio."),
        ));
    }
    Ok(value.to_string())
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
    ChatCommandError::new("chat_runtime", "Falha ao acessar o estado interno do chat.")
}

#[derive(Clone)]
struct TurnContext {
    conversation: ConversationRecord,
    connection: ResolvedConnection,
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

impl ChatService {
    async fn run_turn(&self, context: TurnContext) {
        let _ = self.set_message_status(&context.assistant_message_id, "streaming", None, None);
        let _ = self.emit_turn_event(
            ChatEventKind::TurnStarted,
            &context,
            ChatEventDetail::Empty {},
        );

        let result = tokio::select! {
            biased;
            _ = context.cancellation.cancelled() => Err(TurnOutcome::Cancelled),
            timed = tokio::time::timeout(TURN_TIMEOUT, self.run_model_loop(&context)) => {
                match timed {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => Err(TurnOutcome::Failed(error)),
                    Err(_) => Err(TurnOutcome::Failed(ChatCommandError::new(
                        "turn_timeout",
                        "O turno atingiu o limite de 120 segundos. Restrinja a pergunta e tente novamente.",
                    ))),
                }
            }
        };

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
                    Some("Resposta cancelada pelo usuário."),
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
        if let Ok(mut sequences) = self.runtime.sequences.lock() {
            sequences.remove(&context.generation_id);
        }
    }

    async fn run_model_loop(&self, context: &TurnContext) -> Result<(), ChatCommandError> {
        let profile = profile_by_id(context.connection.provider_id.as_str()).ok_or_else(|| {
            ChatCommandError::new("provider_incompatible", "Provider sem perfil compatível.")
        })?;
        let api_key = if profile.auth_style == AuthStyle::None {
            None
        } else {
            self.credentials
                .get(&context.connection.credential_ref)
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
                let response = self
                    .call_model(
                        &profile,
                        &context.connection,
                        &context.conversation.model_id,
                        api_key.as_deref(),
                        context,
                        &messages,
                        &exchanges,
                    )
                    .await;
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
                        "O provider encerrou o turno sem conteúdo ou chamada de ferramenta.",
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
                    "O turno atingiu o limite de 24 leituras. Restrinja a pergunta e tente novamente.",
                ));
            }

            let results = self
                .execute_tool_calls(
                    context,
                    &repository,
                    &model_call_id,
                    round_index,
                    &response.tool_calls,
                )
                .await?;
            for result in &results {
                total_tool_bytes = total_tool_bytes.saturating_add(result.content.len());
            }
            if total_tool_bytes > MAX_TOOL_RESULT_BYTES {
                return Err(ChatCommandError::new(
                    "tool_result_limit",
                    "O turno atingiu 256 KiB de resultados. Restrinja a pergunta e tente novamente.",
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
                    "O turno atingiu o limite de 8 rodadas. Restrinja a pergunta e tente novamente.",
                ));
            }
        }
        Err(ChatCommandError::new(
            "model_round_limit",
            "O turno atingiu o limite de rodadas.",
        ))
    }

    async fn execute_tool_calls(
        &self,
        context: &TurnContext,
        repository: &RepositoryContext,
        model_call_id: &str,
        round_index: usize,
        calls: &[NormalizedToolCall],
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
            self.emit_turn_event(
                ChatEventKind::ToolStarted,
                context,
                ChatEventDetail::ToolStarted {
                    tool_call: ChatToolStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                        round_index,
                    },
                },
            )?;
        }

        let executions = stream::iter(calls.iter().cloned())
            .map(|call| {
                let repository = repository.clone();
                let fallback_call = call.clone();
                async move {
                    tokio::task::spawn_blocking(move || execute_repository_tool(&repository, call))
                        .await
                        .unwrap_or_else(|error| ToolResult {
                            call: fallback_call,
                            content:
                                json!({ "error": format!("Falha interna da ferramenta: {error}") })
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
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(ChatCommandError::from)
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
                .map_err(|error| format!("Argumentos inválidos: {error}"))
                .and_then(|args| repository.list_tree(args).map_err(|error| error.0))
                .and_then(|result| {
                    serde_json::to_string(&result).map_err(|error| error.to_string())
                })
        }
        "search_repository" => {
            serde_json::from_value::<SearchRepositoryArgs>(call.arguments.clone())
                .map_err(|error| format!("Argumentos inválidos: {error}"))
                .and_then(|args| repository.search(args).map_err(|error| error.0))
                .and_then(|result| {
                    serde_json::to_string(&result).map_err(|error| error.to_string())
                })
        }
        "read_repository_file" => {
            serde_json::from_value::<ReadRepositoryFileArgs>(call.arguments.clone())
                .map_err(|error| format!("Argumentos inválidos: {error}"))
                .and_then(|args| repository.read_file(args).map_err(|error| error.0))
                .and_then(|result| {
                    serde_json::to_string(&result).map_err(|error| error.to_string())
                })
        }
        _ => Err("Ferramenta não permitida. Use somente as três leituras do repositório.".into()),
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
        "list_repository_tree" => "Árvore do repositório consultada.".into(),
        "search_repository" => "Busca no repositório concluída.".into(),
        "read_repository_file" => "Trecho de arquivo consultado.".into(),
        _ => "Ferramenta concluída.".into(),
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
                    message: "A conexão não possui credencial configurada.".into(),
                    retryable: false,
                })?;
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {secret}")).map_err(|_| {
                        ModelFailure {
                            code: "invalid_credential".into(),
                            message: "A credencial contém caracteres inválidos.".into(),
                            retryable: false,
                        }
                    })?,
                );
            }
            AuthStyle::Anthropic => {
                let secret = api_key.ok_or_else(|| ModelFailure {
                    code: "credential_unavailable".into(),
                    message: "A conexão não possui credencial configurada.".into(),
                    retryable: false,
                })?;
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(secret).map_err(|_| ModelFailure {
                        code: "invalid_credential".into(),
                        message: "A credencial contém caracteres inválidos.".into(),
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
                    "O provider não respondeu dentro do prazo."
                } else {
                    "Não foi possível alcançar o provider configurado."
                }
                .into(),
                retryable: true,
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.map_err(|_| ModelFailure {
                code: "invalid_response".into(),
                message: "Não foi possível ler a resposta de erro do provider.".into(),
                retryable: false,
            })?;
            let value: Value = serde_json::from_str(&body).unwrap_or_else(|_| json!({}));
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
                .map_err(|_| invalid_response("Não foi possível ler a resposta do provider."))?;
            let value: Value = serde_json::from_str(&body)
                .map_err(|_| invalid_response("O provider retornou uma resposta JSON inválida."))?;
            return match profile.protocol {
                ProviderProtocol::OpenaiResponses => parse_openai_responses(&value),
                ProviderProtocol::OpenaiChat => parse_openai_chat(&value),
                ProviderProtocol::AnthropicMessages => parse_anthropic(&value),
            };
        }
        self.parse_streaming_response(response, profile.protocol, context)
            .await
    }

    async fn parse_streaming_response(
        &self,
        response: reqwest::Response,
        protocol: ProviderProtocol,
        context: &TurnContext,
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
                message: "O stream do provider foi interrompido.".into(),
                retryable: content.is_empty(),
            })?;
            let events = parser.push(&chunk).map_err(|error| ModelFailure {
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
        let events = parser.finish().map_err(|error| ModelFailure {
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
                        message:
                            "O provider retornou argumentos de ferramenta fragmentados inválidos."
                                .into(),
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
    "Você é o Chat Companion do Hard Vibe Code. Analise arquitetura, explique código e proponha decisões técnicas usando somente conhecimento geral e as ferramentas locais read-only fornecidas. O repositório aberto é a única fonte externa acessível. Nunca peça ou tente usar shell, Git, rede, web, escrita, rename ou ferramentas remotas. Nunca afirme ter aplicado mudanças. Quando propuser alterações, use unified diff copiável e diga explicitamente que é uma proposta não aplicada. Separe claramente evidência encontrada no repositório de inferências ou opiniões próprias. Consulte arquivos apenas quando necessário e restrinja as leituras."
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

fn repository_tool_schemas() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        (
            "list_repository_tree",
            "Lista a estrutura segura do repositório com paths relativos.",
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
            "Busca texto literal ou regex somente em arquivos de texto permitidos do repositório.",
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
            "Lê até 400 linhas/64 KiB de um arquivo de texto permitido, com linhas numeradas.",
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
        .ok_or_else(|| invalid_response("Resposta OpenAI-compatible sem message."))?;
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
        .ok_or_else(|| invalid_response("Resposta Anthropic sem content."))?;
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
                message: "O provider retornou argumentos de ferramenta inválidos.".into(),
                retryable: false,
            })
        }
        Some(arguments @ Value::Object(_)) => Ok(arguments.clone()),
        _ => Err(ModelFailure {
            code: "invalid_tool_arguments".into(),
            message: "O provider retornou argumentos de ferramenta inválidos.".into(),
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
        .unwrap_or("Provider retornou erro sem mensagem.")
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
                connection_id: "connection-a".into(),
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
                connection_id: "connection-a".into(),
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

    #[test]
    fn reopened_service_recovers_pending_messages_and_running_child_records() {
        let (_temp, _workspace, service) = prepare_chat();
        let conversation = service
            .create_conversation(CreateChatConversationRequest {
                project_id: "project-a".into(),
                connection_id: "connection-a".into(),
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
                connection_id: "connection-a".into(),
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
                    conversation_id: conversation.id,
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
                connection_id: "connection-a".into(),
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
    fn exposes_only_three_read_only_repository_tools() {
        let names = repository_tool_schemas()
            .into_iter()
            .map(|(name, _, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "list_repository_tree",
                "search_repository",
                "read_repository_file"
            ]
        );
        let serialized = serde_json::to_string(&tool_definitions_openai_chat()).unwrap();
        assert!(!serialized.contains("apply"));
        assert!(!serialized.contains("shell"));
        assert!(!serialized.contains("write"));
    }
}
