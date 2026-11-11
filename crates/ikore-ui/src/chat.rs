use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use ikore_core::AppCore;
use ikore_core::chat_service::{
    CancelChatTurnRequest, ChatCommandError, ChatConversation, ChatEventDetail, ChatEventEnvelope,
    ChatEventKind, ChatExecutionTargetRequest, ChatMessage, ChatToolCall, ChatTransportKind,
    CreateChatConversationRequest, DeleteChatConversationRequest, GetChatConversationRequest,
    GetChatConversationResponse, GetChatTurnSnapshotRequest, GetChatTurnSnapshotResponse,
    ListChatConversationsRequest, SendChatMessageRequest, SendChatMessageResponse,
    UpdateChatConversationTargetRequest,
};
use ikore_core::llm::ProviderConnection;
use ikore_core::terminals::{
    CliAvailability, CliAvailabilityState, CliBackend, CliModelSelectionSource,
};

use crate::errors::UiCommandError;

fn terminal_error_for_chat(error: ikore_core::terminals::TerminalError) -> UiCommandError {
    UiCommandError::from_chat_parts("chat_cli", error.to_string())
}

pub(crate) type ChatFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, UiCommandError>> + Send + 'static>>;

#[derive(Debug, Clone)]
pub(crate) struct ChatHydration {
    pub(crate) project_id: String,
    pub(crate) connections: Vec<ProviderConnection>,
    pub(crate) cli_availability: Vec<CliAvailability>,
    pub(crate) conversations: Vec<ChatConversation>,
    pub(crate) selected: Option<GetChatConversationResponse>,
    /// The project's last chosen Companion target, restored into the panel
    /// when no conversation is open.
    pub(crate) default_target: Option<ChatExecutionTargetRequest>,
}

pub(crate) trait ChatBackend: Send + Sync {
    fn hydrate(
        &self,
        project_id: String,
        restored_conversation_id: Option<String>,
    ) -> ChatFuture<ChatHydration>;
    fn load_conversation(&self, conversation_id: String)
    -> ChatFuture<GetChatConversationResponse>;
    fn create_conversation(
        &self,
        project_id: String,
        target: ChatExecutionTargetRequest,
    ) -> ChatFuture<GetChatConversationResponse>;
    fn delete_conversation(&self, conversation_id: String) -> ChatFuture<bool>;
    fn send_message(
        &self,
        conversation_id: String,
        content: String,
        expected_target: Option<ChatExecutionTargetRequest>,
    ) -> ChatFuture<(SendChatMessageResponse, GetChatConversationResponse)>;
    fn cancel_turn(&self, execution_id: String) -> ChatFuture<bool>;
    fn turn_snapshot(&self, generation_id: String) -> ChatFuture<GetChatTurnSnapshotResponse>;
    /// Applies the panel's target to an open conversation (and remembers it).
    fn update_conversation_target(
        &self,
        conversation_id: String,
        target: ChatExecutionTargetRequest,
    ) -> ChatFuture<GetChatConversationResponse>;
    /// Remembers the panel's target for the project when no conversation is open.
    fn save_default_target(
        &self,
        project_id: String,
        target: ChatExecutionTargetRequest,
    ) -> ChatFuture<()>;
}

pub(crate) struct CoreChatBackend {
    core: Arc<AppCore>,
}

impl CoreChatBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }
}

impl ChatBackend for CoreChatBackend {
    fn hydrate(
        &self,
        project_id: String,
        restored_conversation_id: Option<String>,
    ) -> ChatFuture<ChatHydration> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            let terminal_service = core.terminals();
            let terminal_project_id = project_id.clone();
            let cli_availability = terminal_service
                .snapshot(&terminal_project_id)
                .await
                .map_err(terminal_error_for_chat)?
                .discovery;
            runtime
                .spawn_blocking(move || {
                    let connections = core
                        .persistence()
                        .list_provider_connections()
                        .map_err(|error| {
                            UiCommandError::from_chat_parts("chat_persistence", error.to_string())
                        })?
                        .connections;
                    let chat = core.chat();
                    let conversations = chat
                        .list_conversations(ListChatConversationsRequest {
                            project_id: project_id.clone(),
                        })
                        .map_err(UiCommandError::from)?
                        .conversations;
                    let selected_id = restored_conversation_id
                        .filter(|id| {
                            conversations
                                .iter()
                                .any(|conversation| &conversation.id == id)
                        })
                        .or_else(|| {
                            conversations
                                .first()
                                .map(|conversation| conversation.id.clone())
                        });
                    let selected = selected_id
                        .map(|conversation_id| {
                            chat.get_conversation(GetChatConversationRequest { conversation_id })
                        })
                        .transpose()
                        .map_err(UiCommandError::from)?;
                    let default_target = chat.default_target(&project_id).ok().flatten();
                    Ok(ChatHydration {
                        project_id,
                        connections,
                        cli_availability,
                        conversations,
                        selected,
                        default_target,
                    })
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("Chat hydration worker interrupted: {error}"))
                })?
        })
    }

    fn load_conversation(
        &self,
        conversation_id: String,
    ) -> ChatFuture<GetChatConversationResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .get_conversation(GetChatConversationRequest { conversation_id })
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de conversa interrompido: {error}"))
                })?
        })
    }

    fn create_conversation(
        &self,
        project_id: String,
        target: ChatExecutionTargetRequest,
    ) -> ChatFuture<GetChatConversationResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let chat = core.chat();
                    let conversation = chat
                        .create_conversation(CreateChatConversationRequest { project_id, target })
                        .map_err(UiCommandError::from)?
                        .conversation;
                    chat.get_conversation(GetChatConversationRequest {
                        conversation_id: conversation.id,
                    })
                    .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("Chat creation worker interrupted: {error}"))
                })?
        })
    }

    fn delete_conversation(&self, conversation_id: String) -> ChatFuture<bool> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .delete_conversation(DeleteChatConversationRequest { conversation_id })
                        .map(|response| response.deleted)
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("Chat deletion worker interrupted: {error}"))
                })?
        })
    }

    fn send_message(
        &self,
        conversation_id: String,
        content: String,
        expected_target: Option<ChatExecutionTargetRequest>,
    ) -> ChatFuture<(SendChatMessageResponse, GetChatConversationResponse)> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let workspace_root = core.workspace_root().map_err(UiCommandError::from)?;
                    let chat = core.chat();
                    let started = chat
                        .start_turn(
                            SendChatMessageRequest {
                                conversation_id: conversation_id.clone(),
                                content,
                                expected_target,
                            },
                            &workspace_root,
                        )
                        .map_err(UiCommandError::from)?;
                    let snapshot = chat
                        .get_conversation(GetChatConversationRequest { conversation_id })
                        .map_err(UiCommandError::from)?;
                    Ok((started, snapshot))
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de envio do Chat interrompido: {error}"))
                })?
        })
    }

    fn cancel_turn(&self, execution_id: String) -> ChatFuture<bool> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .cancel_turn(CancelChatTurnRequest { execution_id })
                        .map(|response| response.cancelled)
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!(
                        "worker de cancelamento do Chat interrompido: {error}"
                    ))
                })?
        })
    }

    fn turn_snapshot(&self, generation_id: String) -> ChatFuture<GetChatTurnSnapshotResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .get_turn_snapshot(GetChatTurnSnapshotRequest { generation_id })
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("Chat resync worker interrupted: {error}"))
                })?
        })
    }

    fn update_conversation_target(
        &self,
        conversation_id: String,
        target: ChatExecutionTargetRequest,
    ) -> ChatFuture<GetChatConversationResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .update_conversation_target(UpdateChatConversationTargetRequest {
                            conversation_id,
                            target,
                        })
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("Chat target worker interrupted: {error}"))
                })?
        })
    }

    fn save_default_target(
        &self,
        project_id: String,
        target: ChatExecutionTargetRequest,
    ) -> ChatFuture<()> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    core.chat()
                        .save_default_target(&project_id, target)
                        .map_err(UiCommandError::from)
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("Chat target worker interrupted: {error}"))
                })?
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatSequenceClassification {
    Next,
    Stale,
    Gap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChatEventOutcome {
    Ignored,
    Applied,
    Reconcile {
        generation_id: String,
        conversation_id: String,
    },
}

/// Messages of the open conversation rendered by default.
pub(crate) const CHAT_RENDER_WINDOW: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChatOperation {
    Create,
    Load,
    Send,
    Delete,
    Cancel,
}

#[derive(Debug, Default)]
struct ConversationState {
    operations: HashMap<ChatOperation, u64>,
    error: Option<UiCommandError>,
}

#[derive(Debug, Default)]
pub struct ChatModel {
    pub(crate) tool_activity_open: bool,
    /// The conversation whose full history the developer asked to see; every other
    /// conversation renders only its last `CHAT_RENDER_WINDOW` messages.
    pub(crate) history_expanded: Option<SharedString>,
    /// Collapses the transport/connection picker so the sidebar has room to
    /// show conversations instead; the CLI form (backend + model + profile +
    /// explanatory text) is tall enough to crowd out the list entirely.
    pub(crate) config_collapsed: bool,
    pub(crate) project_id: Option<SharedString>,
    pub(crate) connections: Vec<ProviderConnection>,
    pub(crate) cli_availability: Vec<CliAvailability>,
    pub(crate) conversations: Vec<ChatConversation>,
    pub active_conversation: Option<SharedString>,
    pub(crate) messages_by_conversation: HashMap<String, Vec<ChatMessage>>,
    pub(crate) tool_calls_by_conversation: HashMap<String, Vec<ChatToolCall>>,
    pub(crate) selected_connection_id: Option<SharedString>,
    pub(crate) selected_transport: ChatTransportKind,
    pub(crate) selected_cli_backend: CliBackend,
    pub(crate) selected_cli_model: Option<String>,
    pub(crate) selected_cli_model_profile: Option<String>,
    pub(crate) selected_cli_model_source: CliModelSelectionSource,
    pub(crate) target_change_pending: bool,
    pub(crate) target_change_generation: u64,
    pub streaming: bool,
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<UiCommandError>,
    pub(crate) pending_delete_conversation_id: Option<SharedString>,
    acknowledged_disclosures: HashSet<String>,
    last_sequences: HashMap<String, u64>,
    hydration_generation: u64,
    hydration_conversation_ids: HashSet<String>,
    conversation_states: HashMap<String, ConversationState>,
    operation_generation: u64,
    project_loading: bool,
    project_error: Option<UiCommandError>,
    creating: bool,
}

impl ChatModel {
    pub(crate) fn classify_sequence(
        previous_sequence: u64,
        next_sequence: u64,
    ) -> ChatSequenceClassification {
        if next_sequence <= previous_sequence {
            ChatSequenceClassification::Stale
        } else if next_sequence > previous_sequence.saturating_add(1) {
            ChatSequenceClassification::Gap
        } else {
            ChatSequenceClassification::Next
        }
    }

    pub(crate) fn begin_hydration(
        &mut self,
        project_id: String,
        restored_conversation_id: Option<String>,
    ) -> u64 {
        self.hydration_generation = self.hydration_generation.saturating_add(1);
        if self.project_id.as_ref().map(AsRef::as_ref) != Some(project_id.as_str()) {
            self.project_id = Some(project_id.into());
            self.connections.clear();
            self.cli_availability.clear();
            self.conversations.clear();
            self.messages_by_conversation.clear();
            self.tool_calls_by_conversation.clear();
            self.conversation_states.clear();
            self.last_sequences.clear();
            self.acknowledged_disclosures.clear();
            self.active_conversation = restored_conversation_id.map(Into::into);
            self.selected_connection_id = None;
            self.selected_transport = ChatTransportKind::Api;
            self.selected_cli_backend = CliBackend::Codex;
            self.selected_cli_model = None;
            self.selected_cli_model_profile = None;
            self.selected_cli_model_source = CliModelSelectionSource::Default;
            self.target_change_pending = false;
            self.target_change_generation = self.target_change_generation.saturating_add(1);
            self.pending_delete_conversation_id = None;
            self.creating = false;
        }
        self.project_loading = true;
        self.project_error = None;
        self.hydration_conversation_ids = self
            .conversations
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect();
        self.recompute_streaming();
        self.hydration_generation
    }

    pub(crate) fn apply_hydration(&mut self, generation: u64, hydration: ChatHydration) -> bool {
        if generation != self.hydration_generation
            || self.project_id.as_ref().map(AsRef::as_ref) != Some(hydration.project_id.as_str())
        {
            return false;
        }
        self.connections = hydration.connections;
        self.cli_availability = hydration.cli_availability;
        let mut conversations = hydration.conversations;
        for existing in &self.conversations {
            if !conversations
                .iter()
                .any(|conversation| conversation.id == existing.id)
                && (!self.hydration_conversation_ids.contains(&existing.id)
                    || self.is_busy(&existing.id))
            {
                conversations.push(existing.clone());
            }
        }
        self.conversations = conversations;
        let selected_connection_id = self
            .eligible_connections()
            .next()
            .map(|connection| SharedString::from(connection.id.clone()));
        self.selected_connection_id = selected_connection_id;
        if !self.active_conversation.as_ref().is_some_and(|active| {
            self.conversations
                .iter()
                .any(|conversation| conversation.id == active.as_ref())
        }) {
            self.active_conversation = hydration
                .selected
                .as_ref()
                .map(|snapshot| SharedString::from(snapshot.conversation.id.clone()));
        }
        if let Some(target) = &hydration.default_target {
            self.apply_target_to_panel(target);
        }
        if let Some(snapshot) = hydration.selected {
            self.store_conversation_snapshot(snapshot);
        }
        self.sync_panel_with_active_conversation();
        self.project_loading = false;
        self.project_error = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn fail_hydration(&mut self, generation: u64, error: UiCommandError) -> bool {
        if generation != self.hydration_generation {
            return false;
        }
        self.project_loading = false;
        self.project_error = Some(error);
        self.recompute_streaming();
        true
    }

    pub(crate) fn finish_hydration_without_backend(&mut self) {
        self.project_loading = false;
        self.recompute_streaming();
    }

    pub(crate) fn eligible_connections(&self) -> impl Iterator<Item = &ProviderConnection> {
        self.connections
            .iter()
            .filter(|connection| connection.chat_eligible)
    }

    pub(crate) fn selected_cli_availability(&self) -> Option<&CliAvailability> {
        self.cli_availability
            .iter()
            .find(|item| item.backend == self.selected_cli_backend)
    }

    pub(crate) fn new_conversation_target_available(&self) -> bool {
        match self.selected_transport {
            ChatTransportKind::Api => self.selected_connection_id.is_some(),
            ChatTransportKind::Cli => self.selected_cli_availability().is_some_and(|item| {
                item.state == CliAvailabilityState::Available
                    && self.selected_cli_backend != CliBackend::Custom
            }),
        }
    }

    pub(crate) fn active_conversation_record(&self) -> Option<&ChatConversation> {
        let active = self.active_conversation.as_ref()?;
        self.conversations
            .iter()
            .find(|conversation| conversation.id == active.as_ref())
    }

    pub(crate) fn conversation_target(
        &self,
        conversation_id: &str,
    ) -> Option<ChatExecutionTargetRequest> {
        let conversation = self
            .conversations
            .iter()
            .find(|conversation| conversation.id == conversation_id)?;
        match conversation.transport {
            ChatTransportKind::Api => Some(ChatExecutionTargetRequest::Api {
                connection_id: conversation.connection_id.clone()?,
            }),
            ChatTransportKind::Cli => Some(ChatExecutionTargetRequest::Cli {
                backend: conversation.cli_backend?,
                model: (conversation.model_id != "default").then(|| conversation.model_id.clone()),
                model_profile: conversation.model_profile.clone(),
            }),
        }
    }

    pub(crate) fn composer_available_for(&self, conversation_id: &str) -> bool {
        self.project_id.is_some()
            && self.conversations.iter().any(|conversation| {
                conversation.id == conversation_id && conversation.connection_available
            })
            && !(self.active_conversation.as_ref().map(AsRef::as_ref) == Some(conversation_id)
                && self.target_change_pending)
    }

    pub(crate) fn streaming_count(&self) -> usize {
        self.conversations
            .iter()
            .filter(|conversation| self.is_streaming(&conversation.id))
            .count()
    }

    /// The tail of the active conversation that is rendered, and how many earlier messages it
    /// hides. Every frame re-parses each rendered message (markdown, diffs), and streaming
    /// notifies on every token batch, so a long conversation renders a bounded window.
    pub(crate) fn rendered_messages(&self) -> (&[ChatMessage], usize) {
        self.active_conversation
            .as_ref()
            .map(AsRef::as_ref)
            .map(|id| self.rendered_messages_for(id))
            .unwrap_or_default()
    }

    pub(crate) fn rendered_messages_for(&self, conversation_id: &str) -> (&[ChatMessage], usize) {
        let messages = self.messages_for(conversation_id);
        let expanded = self.history_expanded.as_ref().map(AsRef::as_ref) == Some(conversation_id);
        let hidden = if expanded {
            0
        } else {
            messages.len().saturating_sub(CHAT_RENDER_WINDOW)
        };
        (&messages[hidden..], hidden)
    }

    pub(crate) fn active_messages(&self) -> &[ChatMessage] {
        self.active_conversation
            .as_ref()
            .map(|id| self.messages_for(id))
            .unwrap_or_default()
    }

    pub(crate) fn messages_for(&self, conversation_id: &str) -> &[ChatMessage] {
        self.messages_by_conversation
            .get(conversation_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn is_streaming(&self, conversation_id: &str) -> bool {
        self.messages_for(conversation_id).iter().any(|message| {
            message.role == "assistant"
                && matches!(message.status.as_str(), "pending" | "streaming")
        })
    }

    pub(crate) fn is_loading(&self, conversation_id: &str) -> bool {
        self.conversation_states
            .get(conversation_id)
            .is_some_and(|state| state.operations.contains_key(&ChatOperation::Load))
    }

    pub(crate) fn is_busy(&self, conversation_id: &str) -> bool {
        self.conversation_states
            .get(conversation_id)
            .is_some_and(|state| {
                state
                    .operations
                    .keys()
                    .any(|operation| *operation != ChatOperation::Load)
            })
    }

    pub(crate) fn error_for(&self, conversation_id: &str) -> Option<&UiCommandError> {
        self.conversation_states
            .get(conversation_id)
            .and_then(|state| state.error.as_ref())
    }

    pub(crate) fn begin_operation(
        &mut self,
        conversation_id: &str,
        operation: ChatOperation,
    ) -> Option<u64> {
        let state = self
            .conversation_states
            .entry(conversation_id.into())
            .or_default();
        if state.operations.contains_key(&operation)
            || (matches!(operation, ChatOperation::Send | ChatOperation::Delete)
                && state
                    .operations
                    .keys()
                    .any(|pending| matches!(pending, ChatOperation::Send | ChatOperation::Delete)))
        {
            return None;
        }
        self.operation_generation = self.operation_generation.saturating_add(1);
        let generation = self.operation_generation;
        state.operations.insert(operation, generation);
        state.error = None;
        self.recompute_streaming();
        Some(generation)
    }

    pub(crate) fn finish_operation(
        &mut self,
        project_id: &str,
        conversation_id: &str,
        operation: ChatOperation,
        generation: u64,
    ) -> bool {
        if self.project_id.as_ref().map(AsRef::as_ref) != Some(project_id) {
            return false;
        }
        let Some(state) = self.conversation_states.get_mut(conversation_id) else {
            return false;
        };
        if state.operations.get(&operation) != Some(&generation) {
            return false;
        }
        state.operations.remove(&operation);
        self.recompute_streaming();
        true
    }

    pub(crate) fn set_conversation_error(&mut self, conversation_id: &str, error: UiCommandError) {
        self.conversation_states
            .entry(conversation_id.into())
            .or_default()
            .error = Some(error);
        self.recompute_streaming();
    }

    pub(crate) fn active_tool_calls(&self) -> &[ChatToolCall] {
        self.active_conversation
            .as_ref()
            .and_then(|id| self.tool_calls_by_conversation.get(id.as_ref()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn disclosure_required(&self) -> bool {
        self.active_conversation_record().is_some() && self.active_messages().is_empty()
    }

    pub(crate) fn disclosure_accepted(&self) -> bool {
        self.active_conversation
            .as_ref()
            .is_none_or(|id| self.disclosure_accepted_for(id))
    }

    pub(crate) fn disclosure_accepted_for(&self, conversation_id: &str) -> bool {
        !self.messages_for(conversation_id).is_empty()
            || self.acknowledged_disclosures.contains(conversation_id)
    }

    pub(crate) fn acknowledge_disclosure(&mut self) {
        if let Some(id) = self.active_conversation.clone() {
            self.acknowledge_disclosure_for(&id);
        }
    }

    pub(crate) fn acknowledge_disclosure_for(&mut self, conversation_id: &str) {
        self.acknowledged_disclosures.insert(conversation_id.into());
    }

    pub(crate) fn execution_id_for(&self, conversation_id: &str) -> Option<String> {
        self.messages_for(conversation_id)
            .iter()
            .rev()
            .find(|message| {
                message.role == "assistant"
                    && matches!(message.status.as_str(), "pending" | "streaming")
            })
            .and_then(|message| message.execution_id.clone())
    }

    pub(crate) fn select_conversation_locally(&mut self, conversation_id: &str) -> bool {
        if !self
            .conversations
            .iter()
            .any(|conversation| conversation.id == conversation_id)
        {
            return false;
        }
        self.active_conversation = Some(conversation_id.to_string().into());
        self.target_change_generation = self.target_change_generation.saturating_add(1);
        self.target_change_pending = false;
        self.pending_delete_conversation_id = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn apply_selected_conversation(
        &mut self,
        project_id: &str,
        conversation_id: &str,
        snapshot: GetChatConversationResponse,
    ) -> bool {
        if self.project_id.as_ref().map(AsRef::as_ref) != Some(project_id)
            || snapshot.conversation.id != conversation_id
            || snapshot.conversation.project_id != project_id
        {
            return false;
        }
        self.store_conversation_snapshot(snapshot);
        self.sync_panel_with_active_conversation();
        self.recompute_streaming();
        true
    }

    /// The panel's current selection as a target, when it names one.
    pub(crate) fn panel_target(&self) -> Option<ChatExecutionTargetRequest> {
        match self.selected_transport {
            ChatTransportKind::Api => self.selected_connection_id.as_ref().map(|connection_id| {
                ChatExecutionTargetRequest::Api {
                    connection_id: connection_id.to_string(),
                }
            }),
            ChatTransportKind::Cli => Some(ChatExecutionTargetRequest::Cli {
                backend: self.selected_cli_backend,
                model: self.selected_cli_model.clone(),
                model_profile: self.selected_cli_model_profile.clone(),
            }),
        }
    }

    pub(crate) fn apply_target_to_panel(&mut self, target: &ChatExecutionTargetRequest) {
        match target {
            ChatExecutionTargetRequest::Api { connection_id } => {
                self.selected_transport = ChatTransportKind::Api;
                if self
                    .eligible_connections()
                    .any(|connection| &connection.id == connection_id)
                {
                    self.selected_connection_id = Some(connection_id.clone().into());
                }
            }
            ChatExecutionTargetRequest::Cli {
                backend,
                model,
                model_profile,
            } => {
                self.selected_transport = ChatTransportKind::Cli;
                self.selected_cli_backend = *backend;
                self.selected_cli_model = model.clone();
                self.selected_cli_model_profile = model_profile.clone();
                self.selected_cli_model_source = if model.is_some() {
                    CliModelSelectionSource::Catalog
                } else {
                    CliModelSelectionSource::Default
                };
            }
        }
    }

    /// The panel always shows the open conversation's real target: before,
    /// it reset to API/Codex on every visit while the conversation kept
    /// sending to its own stored backend (`ToDoListWebView`, 2026-09-22).
    pub(crate) fn sync_panel_with_active_conversation(&mut self) {
        let Some(conversation) = self.active_conversation_record().cloned() else {
            return;
        };
        match conversation.transport {
            ChatTransportKind::Api => {
                self.selected_transport = ChatTransportKind::Api;
                if let Some(connection_id) = conversation.connection_id {
                    self.selected_connection_id = Some(connection_id.into());
                }
            }
            ChatTransportKind::Cli => {
                let Some(backend) = conversation.cli_backend else {
                    return;
                };
                self.selected_transport = ChatTransportKind::Cli;
                self.selected_cli_backend = backend;
                self.selected_cli_model =
                    (conversation.model_id != "default").then_some(conversation.model_id);
                self.selected_cli_model_profile = conversation.model_profile;
                self.selected_cli_model_source = conversation.model_source;
            }
        }
    }

    /// Stores a retargeted conversation; the panel already shows its target.
    pub(crate) fn apply_retargeted(&mut self, snapshot: GetChatConversationResponse) {
        let conversation_id = snapshot.conversation.id.clone();
        self.store_conversation_snapshot(snapshot);
        self.sync_panel_with_active_conversation();
        self.conversation_states
            .entry(conversation_id)
            .or_default()
            .error = None;
        self.recompute_streaming();
    }

    pub(crate) fn begin_busy(&mut self) -> bool {
        if self.creating {
            return false;
        }
        self.creating = true;
        self.project_error = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn finish_busy(&mut self) {
        self.creating = false;
        self.recompute_streaming();
    }

    pub(crate) fn apply_created(&mut self, snapshot: GetChatConversationResponse) {
        self.conversations
            .retain(|conversation| conversation.id != snapshot.conversation.id);
        self.conversations.insert(0, snapshot.conversation.clone());
        self.active_conversation = Some(snapshot.conversation.id.clone().into());
        self.store_conversation_snapshot(snapshot);
        self.pending_delete_conversation_id = None;
        self.error = None;
        self.recompute_streaming();
    }

    pub(crate) fn apply_deleted(&mut self, conversation_id: &str) -> Option<String> {
        self.conversations
            .retain(|conversation| conversation.id != conversation_id);
        self.messages_by_conversation.remove(conversation_id);
        self.tool_calls_by_conversation.remove(conversation_id);
        self.conversation_states.remove(conversation_id);
        self.acknowledged_disclosures.remove(conversation_id);
        self.pending_delete_conversation_id = None;
        if self.active_conversation.as_ref().map(AsRef::as_ref) == Some(conversation_id) {
            self.active_conversation = self
                .conversations
                .first()
                .map(|conversation| SharedString::from(conversation.id.clone()));
        }
        self.recompute_streaming();
        self.active_conversation.as_ref().map(ToString::to_string)
    }

    pub(crate) fn store_conversation_snapshot(&mut self, snapshot: GetChatConversationResponse) {
        if self.project_id.as_ref().map(AsRef::as_ref)
            != Some(snapshot.conversation.project_id.as_str())
        {
            return;
        }
        let conversation_id = snapshot.conversation.id.clone();
        if let Some(existing) = self
            .conversations
            .iter_mut()
            .find(|conversation| conversation.id == conversation_id)
        {
            *existing = snapshot.conversation;
        } else {
            self.conversations.insert(0, snapshot.conversation);
        }
        let mut messages = snapshot.messages;
        // A load or send acknowledgement may race token events or another load. Never
        // discard a newer generation, message or sequence already observed in the UI.
        let mut keep_existing_tools = false;
        if let Some(existing) = self.messages_by_conversation.get(&conversation_id) {
            for current in existing {
                match messages.iter_mut().find(|message| message.id == current.id) {
                    Some(message)
                        if message.last_sequence < current.last_sequence
                            || (message.last_sequence == current.last_sequence
                                && matches!(message.status.as_str(), "pending" | "streaming")
                                && !matches!(current.status.as_str(), "pending" | "streaming")) =>
                    {
                        *message = current.clone();
                        keep_existing_tools = true;
                    }
                    Some(_) => {}
                    None => {
                        messages.push(current.clone());
                        keep_existing_tools = true;
                    }
                }
            }
        }
        messages.sort_by_key(|message| message.ordinal);
        for message in &messages {
            if let Some(generation_id) = &message.generation_id {
                self.last_sequences
                    .entry(generation_id.clone())
                    .and_modify(|sequence| *sequence = (*sequence).max(message.last_sequence))
                    .or_insert(message.last_sequence);
            }
        }
        self.messages_by_conversation
            .insert(conversation_id.clone(), messages);
        if !keep_existing_tools
            || !self
                .tool_calls_by_conversation
                .contains_key(&conversation_id)
        {
            self.tool_calls_by_conversation
                .insert(conversation_id, snapshot.tool_calls);
        }
        self.recompute_streaming();
    }

    pub(crate) fn apply_started_snapshot(
        &mut self,
        conversation_id: &str,
        generation_id: &str,
        snapshot: GetChatConversationResponse,
    ) -> bool {
        if snapshot.conversation.id != conversation_id
            || self.project_id.as_ref().map(AsRef::as_ref)
                != Some(snapshot.conversation.project_id.as_str())
            || !snapshot
                .messages
                .iter()
                .any(|message| message.generation_id.as_deref() == Some(generation_id))
        {
            return false;
        }
        self.last_sequences
            .entry(generation_id.to_string())
            .or_insert(0);
        self.store_conversation_snapshot(snapshot);
        true
    }

    pub(crate) fn consume_event(&mut self, envelope: ChatEventEnvelope) -> ChatEventOutcome {
        if !self
            .conversations
            .iter()
            .any(|conversation| conversation.id == envelope.conversation_id)
            && !self
                .messages_by_conversation
                .contains_key(&envelope.conversation_id)
        {
            return ChatEventOutcome::Ignored;
        }
        let previous = self
            .last_sequences
            .get(&envelope.generation_id)
            .copied()
            .unwrap_or_default();
        match Self::classify_sequence(previous, envelope.sequence) {
            ChatSequenceClassification::Stale => return ChatEventOutcome::Ignored,
            ChatSequenceClassification::Gap => {
                return ChatEventOutcome::Reconcile {
                    generation_id: envelope.generation_id,
                    conversation_id: envelope.conversation_id,
                };
            }
            ChatSequenceClassification::Next => {}
        }
        self.last_sequences
            .insert(envelope.generation_id.clone(), envelope.sequence);

        let message = self
            .messages_by_conversation
            .get_mut(&envelope.conversation_id)
            .and_then(|messages| {
                messages.iter_mut().find(|message| {
                    message.id == envelope.assistant_message_id
                        && message.generation_id.as_deref() == Some(envelope.generation_id.as_str())
                })
            });
        let requires_snapshot = match (&envelope.kind, &envelope.detail, message) {
            (ChatEventKind::TokenDelta, ChatEventDetail::TokenDelta { delta }, Some(message)) => {
                message.content.push_str(delta);
                message.status = "streaming".into();
                message.last_sequence = envelope.sequence;
                false
            }
            (ChatEventKind::ContentReset, ChatEventDetail::ContentReset { .. }, Some(message)) => {
                message.content.clear();
                message.status = "streaming".into();
                message.last_sequence = envelope.sequence;
                false
            }
            (ChatEventKind::TurnStarted, _, Some(message)) => {
                message.status = "streaming".into();
                message.last_sequence = envelope.sequence;
                false
            }
            _ => true,
        };
        self.recompute_streaming();
        if requires_snapshot {
            ChatEventOutcome::Reconcile {
                generation_id: envelope.generation_id,
                conversation_id: envelope.conversation_id,
            }
        } else {
            ChatEventOutcome::Applied
        }
    }

    pub(crate) fn apply_turn_snapshot(
        &mut self,
        project_id: &str,
        generation_id: &str,
        conversation_id: &str,
        snapshot: GetChatTurnSnapshotResponse,
    ) -> bool {
        if self.project_id.as_ref().map(AsRef::as_ref) != Some(project_id)
            || snapshot.message.conversation_id != conversation_id
            || snapshot.message.generation_id.as_deref() != Some(generation_id)
        {
            return false;
        }
        let previous = self
            .last_sequences
            .get(generation_id)
            .copied()
            .unwrap_or_default();
        if snapshot.message.last_sequence < previous {
            return false;
        }
        let messages = self
            .messages_by_conversation
            .entry(conversation_id.to_string())
            .or_default();
        messages.retain(|message| message.id != snapshot.message.id);
        messages.push(snapshot.message);
        messages.sort_by_key(|message| message.ordinal);
        self.tool_calls_by_conversation
            .insert(conversation_id.to_string(), snapshot.tool_calls);
        self.last_sequences.insert(
            generation_id.to_string(),
            messages
                .iter()
                .find(|message| message.generation_id.as_deref() == Some(generation_id))
                .map(|message| message.last_sequence)
                .unwrap_or(previous),
        );
        self.conversation_states
            .entry(conversation_id.into())
            .or_default()
            .error = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn set_error(&mut self, error: UiCommandError) {
        if let Some(id) = self.active_conversation.clone() {
            self.set_conversation_error(&id, error);
        } else {
            self.project_loading = false;
            self.creating = false;
            self.project_error = Some(error);
            self.recompute_streaming();
        }
    }

    fn recompute_streaming(&mut self) {
        let active = self.active_conversation.as_ref().map(AsRef::as_ref);
        self.streaming = active.is_some_and(|id| self.is_streaming(id));
        self.loading = self.project_loading || active.is_some_and(|id| self.is_loading(id));
        self.busy = self.creating || active.is_some_and(|id| self.is_busy(id));
        self.error = active
            .and_then(|id| self.error_for(id))
            .cloned()
            .or_else(|| self.project_error.clone());
    }
}

impl From<ChatCommandError> for UiCommandError {
    fn from(error: ChatCommandError) -> Self {
        Self::from_chat_parts(error.code, error.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use ikore_core::chat_service::{ChatEventDetail, ChatEventError};
    use std::path::PathBuf;
    use std::sync::Mutex;

    use crate::AppModel;

    fn connection(id: &str, eligible: bool) -> ProviderConnection {
        ProviderConnection {
            id: id.into(),
            provider_id: "openai".into(),
            display_name: id.into(),
            endpoint_url: None,
            default_model: "gpt-4.1-mini".into(),
            enabled: true,
            state: "active".into(),
            blocked_reason: None,
            cooldown_until: None,
            max_concurrency: 1,
            version: 1,
            has_api_key: true,
            chat_eligible: eligible,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn conversation(id: &str, project_id: &str) -> ChatConversation {
        ChatConversation {
            id: id.into(),
            project_id: project_id.into(),
            connection_id: Some("eligible".into()),
            provider_id: "openai".into(),
            model_id: "gpt-4.1-mini".into(),
            transport: ikore_core::chat_service::ChatTransportKind::Api,
            cli_backend: None,
            model_profile: None,
            model_source: ikore_core::terminals::CliModelSelectionSource::Default,
            title: id.into(),
            connection_available: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn message(content: &str, sequence: u64) -> ChatMessage {
        ChatMessage {
            id: "assistant-1".into(),
            conversation_id: "conversation-1".into(),
            ordinal: 1,
            role: "assistant".into(),
            content: content.into(),
            status: "streaming".into(),
            execution_id: Some("execution-1".into()),
            generation_id: Some("generation-1".into()),
            last_sequence: sequence,
            input_tokens: None,
            output_tokens: None,
            estimated_cost_usd: None,
            error_code: None,
            error_message: None,
            target_label: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn snapshot(project_id: &str, content: &str, sequence: u64) -> ChatHydration {
        ChatHydration {
            project_id: project_id.into(),
            connections: vec![connection("eligible", true), connection("blocked", false)],
            cli_availability: Vec::new(),
            conversations: vec![conversation("conversation-1", project_id)],
            selected: Some(GetChatConversationResponse {
                conversation: conversation("conversation-1", project_id),
                messages: vec![message(content, sequence)],
                tool_calls: Vec::new(),
            }),
            default_target: None,
        }
    }

    #[test]
    fn a_long_conversation_renders_its_tail_until_the_history_is_expanded() {
        let mut model = ChatModel {
            active_conversation: Some("conversation-1".into()),
            ..Default::default()
        };
        let messages: Vec<_> = (0..CHAT_RENDER_WINDOW as u64 + 25)
            .map(|index| message(&format!("m{index}"), index))
            .collect();
        model
            .messages_by_conversation
            .insert("conversation-1".into(), messages);

        let (rendered, hidden) = model.rendered_messages();
        assert_eq!(hidden, 25);
        assert_eq!(rendered.len(), CHAT_RENDER_WINDOW);
        assert_eq!(
            rendered.last().unwrap().content,
            format!("m{}", CHAT_RENDER_WINDOW + 24)
        );

        model.history_expanded = Some("conversation-1".into());
        assert_eq!(model.rendered_messages().1, 0);
        // Expanding one conversation does not expand another.
        model.active_conversation = Some("conversation-2".into());
        model.messages_by_conversation.insert(
            "conversation-2".into(),
            (0..60).map(|i| message("x", i)).collect(),
        );
        assert_eq!(model.rendered_messages().1, 60 - CHAT_RENDER_WINDOW);
    }

    #[test]
    fn conversation_operations_and_errors_are_independent() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "first", 0));
        model
            .conversations
            .push(conversation("conversation-2", "project-1"));
        let first = model
            .begin_operation("conversation-1", ChatOperation::Send)
            .unwrap();
        let second = model
            .begin_operation("conversation-2", ChatOperation::Send)
            .unwrap();
        assert!(model.is_busy("conversation-1"));
        assert!(model.is_busy("conversation-2"));
        assert!(model.finish_operation("project-1", "conversation-2", ChatOperation::Send, second));
        model.set_conversation_error("conversation-2", UiCommandError::worker("second failed"));
        assert!(model.is_busy("conversation-1"));
        assert!(!model.is_busy("conversation-2"));
        assert!(model.error_for("conversation-1").is_none());
        assert!(model.error_for("conversation-2").is_some());
        assert!(model.finish_operation("project-1", "conversation-1", ChatOperation::Send, first));
    }

    #[test]
    fn a_started_turn_is_stored_after_selecting_another_conversation() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "first", 0));
        model
            .conversations
            .push(conversation("conversation-2", "project-1"));
        model.select_conversation_locally("conversation-2");
        assert!(model.apply_started_snapshot(
            "conversation-1",
            "generation-1",
            snapshot("project-1", "continued", 1).selected.unwrap()
        ));
        assert_eq!(model.messages_for("conversation-1")[0].content, "continued");
        assert_eq!(
            model.active_conversation.as_ref().map(AsRef::as_ref),
            Some("conversation-2")
        );
        assert!(model.is_streaming("conversation-1"));
        assert!(!model.is_streaming("conversation-2"));
    }

    #[test]
    fn project_refresh_preserves_other_conversations_and_active_work() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "first", 0));
        model
            .conversations
            .push(conversation("conversation-2", "project-1"));
        let mut second = message("second", 2);
        second.conversation_id = "conversation-2".into();
        model
            .messages_by_conversation
            .insert("conversation-2".into(), vec![second]);
        let pending = model
            .begin_operation("conversation-2", ChatOperation::Send)
            .unwrap();
        let generation = model.begin_hydration("project-1".into(), None);
        assert_eq!(model.messages_for("conversation-2")[0].content, "second");
        assert!(model.is_busy("conversation-2"));
        let mut refresh = snapshot("project-1", "first", 0);
        refresh
            .conversations
            .push(conversation("conversation-2", "project-1"));
        model.apply_hydration(generation, refresh);
        assert_eq!(model.messages_for("conversation-2")[0].content, "second");
        assert!(model.finish_operation(
            "project-1",
            "conversation-2",
            ChatOperation::Send,
            pending
        ));
    }

    #[test]
    fn conversation_operations_reject_stale_generation_and_project() {
        let mut model = ChatModel::default();
        model.begin_hydration("project-a".into(), None);
        let first = model
            .begin_operation("conversation-1", ChatOperation::Load)
            .unwrap();
        assert!(!model.finish_operation("project-b", "conversation-1", ChatOperation::Load, first));
        assert!(model.finish_operation("project-a", "conversation-1", ChatOperation::Load, first));
        let next = model
            .begin_operation("conversation-1", ChatOperation::Load)
            .unwrap();
        assert!(!model.finish_operation("project-a", "conversation-1", ChatOperation::Load, first));
        assert!(model.is_loading("conversation-1"));
        model.begin_hydration("project-b".into(), None);
        model.begin_hydration("project-a".into(), None);
        let newest = model
            .begin_operation("conversation-1", ChatOperation::Load)
            .unwrap();
        assert!(newest > next);
        assert!(!model.finish_operation("project-a", "conversation-1", ChatOperation::Load, next));
    }

    #[test]
    fn cancel_operation_does_not_displace_a_load_or_another_conversation() {
        let mut model = ChatModel::default();
        model.begin_hydration("project-1".into(), None);
        let load = model
            .begin_operation("conversation-1", ChatOperation::Load)
            .unwrap();
        let cancel = model
            .begin_operation("conversation-1", ChatOperation::Cancel)
            .unwrap();
        let send = model
            .begin_operation("conversation-2", ChatOperation::Send)
            .unwrap();
        assert!(model.finish_operation(
            "project-1",
            "conversation-1",
            ChatOperation::Cancel,
            cancel
        ));
        assert!(model.is_loading("conversation-1"));
        assert!(model.is_busy("conversation-2"));
        assert!(model.finish_operation("project-1", "conversation-1", ChatOperation::Load, load));
        assert!(model.finish_operation("project-1", "conversation-2", ChatOperation::Send, send));
    }

    #[test]
    fn a_stale_snapshot_cannot_rewind_streamed_content() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "current", 4));
        model.store_conversation_snapshot(snapshot("project-1", "old", 1).selected.unwrap());
        assert_eq!(model.messages_for("conversation-1")[0].content, "current");
        assert_eq!(model.messages_for("conversation-1")[0].last_sequence, 4);
    }

    /// `ToDoListWebView`, 2026-09-22: the panel showed "Codex" after every visit
    /// while the open conversation kept sending to Claude. The panel now shows
    /// the open conversation's target, and the remembered project target
    /// when none is open.
    #[test]
    fn a_refresh_preserves_a_conversation_created_after_its_list_read_started() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "original", 1));
        let refresh = model.begin_hydration("project-1".into(), None);
        model.apply_created(GetChatConversationResponse {
            conversation: conversation("conversation-2", "project-1"),
            messages: Vec::new(),
            tool_calls: Vec::new(),
        });
        assert!(model.apply_hydration(refresh, snapshot("project-1", "original", 1)));
        assert_eq!(
            model.active_conversation.as_ref().map(AsRef::as_ref),
            Some("conversation-2")
        );
        assert!(
            model
                .conversations
                .iter()
                .any(|conversation| conversation.id == "conversation-2")
        );
        assert_eq!(model.messages_for("conversation-1")[0].content, "original");
    }

    #[test]
    fn the_panel_mirrors_the_open_conversation_and_restores_the_saved_target() {
        let mut claude = conversation("conversation-1", "project-1");
        claude.transport = ChatTransportKind::Cli;
        claude.connection_id = None;
        claude.cli_backend = Some(CliBackend::Claude);
        claude.provider_id = "claude".into();
        claude.model_id = "haiku".into();
        claude.model_source = CliModelSelectionSource::Catalog;
        let mut hydration = snapshot("project-1", "", 0);
        hydration.conversations = vec![claude.clone()];
        hydration.selected.as_mut().unwrap().conversation = claude.clone();
        hydration.default_target = Some(ChatExecutionTargetRequest::Cli {
            backend: CliBackend::Codex,
            model: None,
            model_profile: None,
        });
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        assert!(model.apply_hydration(generation, hydration));
        assert_eq!(model.selected_transport, ChatTransportKind::Cli);
        assert_eq!(model.selected_cli_backend, CliBackend::Claude);
        assert_eq!(model.selected_cli_model.as_deref(), Some("haiku"));
        assert_eq!(
            model.panel_target(),
            Some(ChatExecutionTargetRequest::Cli {
                backend: CliBackend::Claude,
                model: Some("haiku".into()),
                model_profile: None,
            })
        );

        // No conversation open: the remembered target, not the API/Codex default.
        let mut empty = snapshot("project-1", "", 0);
        empty.conversations.clear();
        empty.selected = None;
        empty.default_target = Some(ChatExecutionTargetRequest::Cli {
            backend: CliBackend::Codex,
            model: Some("gpt-5.6-terra".into()),
            model_profile: None,
        });
        let generation = model.begin_hydration("project-1".into(), None);
        assert!(model.apply_hydration(generation, empty));
        assert_eq!(model.selected_transport, ChatTransportKind::Cli);
        assert_eq!(model.selected_cli_backend, CliBackend::Codex);
        assert_eq!(model.selected_cli_model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(
            model.streaming_count(),
            0,
            "removed conversations cannot retain a streaming badge"
        );

        // A retargeted snapshot updates the list row and the panel together.
        let generation = model.begin_hydration("project-1".into(), None);
        let mut hydration = snapshot("project-1", "", 0);
        hydration.conversations = vec![claude.clone()];
        hydration.selected.as_mut().unwrap().conversation = claude.clone();
        assert!(model.apply_hydration(generation, hydration));
        let mut codex = claude;
        codex.cli_backend = Some(CliBackend::Codex);
        codex.provider_id = "codex".into();
        codex.model_id = "default".into();
        codex.model_source = CliModelSelectionSource::Default;
        model.apply_retargeted(GetChatConversationResponse {
            conversation: codex,
            messages: Vec::new(),
            tool_calls: Vec::new(),
        });
        assert_eq!(model.selected_cli_backend, CliBackend::Codex);
        assert_eq!(model.selected_cli_model, None);
        assert_eq!(
            model.active_conversation_record().unwrap().cli_backend,
            Some(CliBackend::Codex)
        );
    }

    fn delta(sequence: u64, text: &str) -> ChatEventEnvelope {
        ChatEventEnvelope {
            kind: ChatEventKind::TokenDelta,
            schema_version: 1,
            sequence,
            occurred_at: "now".into(),
            conversation_id: "conversation-1".into(),
            assistant_message_id: "assistant-1".into(),
            execution_id: "execution-1".into(),
            generation_id: "generation-1".into(),
            detail: ChatEventDetail::TokenDelta { delta: text.into() },
        }
    }

    #[test]
    fn filters_connections_by_chat_tool_capability() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        assert!(model.apply_hydration(generation, snapshot("project-1", "", 0)));
        assert_eq!(
            model
                .eligible_connections()
                .map(|connection| connection.id.as_str())
                .collect::<Vec<_>>(),
            vec!["eligible"]
        );
    }

    #[test]
    fn requires_active_project_before_composer() {
        let mut model = ChatModel::default();
        assert!(!model.composer_available_for("conversation-1"));
        let generation = model.begin_hydration("project-1".into(), None);
        assert!(model.apply_hydration(generation, snapshot("project-1", "", 0)));
        assert!(model.composer_available_for("conversation-1"));
        model.target_change_pending = true;
        assert!(!model.composer_available_for("conversation-1"));
        model.target_change_pending = false;
        assert!(model.composer_available_for("conversation-1"));
    }

    #[test]
    fn cli_target_requires_a_supported_available_backend() {
        let mut model = ChatModel {
            selected_transport: ChatTransportKind::Cli,
            selected_cli_backend: CliBackend::Claude,
            cli_availability: vec![CliAvailability {
                backend: CliBackend::Claude,
                state: CliAvailabilityState::AuthRequired,
                executable_path: Some(PathBuf::from(r"C:\Tools\claude.exe")),
                version: Some("2.1.81".into()),
                diagnostic: Some("Authentication required".into()),
                install_url: "https://example.invalid/claude",
                install_command: "install claude",
                quota: Vec::new(),
                capabilities: ikore_core::terminals::CliCapabilities {
                    output_format: ikore_core::terminals::CliOutputFormat::JsonLines,
                    structured_output: true,
                    native_resume: true,
                    thinking_levels: vec![],
                    auth_probe: true,
                    read_only: true,
                    model_catalog: false,
                },
            }],
            ..ChatModel::default()
        };
        assert!(!model.new_conversation_target_available());
        model.cli_availability[0].state = CliAvailabilityState::Available;
        assert!(model.new_conversation_target_available());
        model.selected_cli_backend = CliBackend::Custom;
        assert!(!model.new_conversation_target_available());
    }

    #[test]
    fn classifies_duplicate_next_and_gap_sequences() {
        assert_eq!(
            ChatModel::classify_sequence(3, 3),
            ChatSequenceClassification::Stale
        );
        assert_eq!(
            ChatModel::classify_sequence(3, 4),
            ChatSequenceClassification::Next
        );
        assert_eq!(
            ChatModel::classify_sequence(3, 6),
            ChatSequenceClassification::Gap
        );
        assert_eq!(
            ChatModel::classify_sequence(0, 2),
            ChatSequenceClassification::Gap
        );
    }

    #[test]
    fn deduplicates_deltas_and_reconciles_gap_snapshot() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "", 0));
        assert_eq!(
            model.consume_event(delta(1, "hello")),
            ChatEventOutcome::Applied
        );
        assert_eq!(
            model.consume_event(delta(1, " duplicate")),
            ChatEventOutcome::Ignored
        );
        assert_eq!(model.active_messages()[0].content, "hello");
        assert!(matches!(
            model.consume_event(delta(3, " lost")),
            ChatEventOutcome::Reconcile { .. }
        ));
        assert!(model.apply_turn_snapshot(
            "project-1",
            "generation-1",
            "conversation-1",
            GetChatTurnSnapshotResponse {
                message: message("hello world", 3),
                tool_calls: Vec::new(),
            }
        ));
        assert_eq!(model.active_messages()[0].content, "hello world");
    }

    #[test]
    fn discards_late_response_after_project_switch() {
        let mut model = ChatModel::default();
        let old_generation = model.begin_hydration("project-a".into(), None);
        let new_generation = model.begin_hydration("project-b".into(), None);
        assert!(!model.apply_hydration(old_generation, snapshot("project-a", "late", 0)));
        assert!(model.apply_hydration(new_generation, snapshot("project-b", "current", 0)));
        assert_eq!(
            model.project_id.as_ref().map(|value| value.as_ref()),
            Some("project-b")
        );
        assert_eq!(model.active_messages()[0].content, "current");
    }

    #[test]
    fn falls_back_to_newest_existing_conversation() {
        let mut model = ChatModel::default();
        let generation =
            model.begin_hydration("project-1".into(), Some("conversation-deleted".into()));
        assert!(model.apply_hydration(generation, snapshot("project-1", "", 0)));
        assert_eq!(
            model
                .active_conversation
                .as_ref()
                .map(|value| value.as_ref()),
            Some("conversation-1")
        );
    }

    #[test]
    fn terminal_failure_event_requests_persisted_snapshot() {
        let mut model = ChatModel::default();
        let generation = model.begin_hydration("project-1".into(), None);
        model.apply_hydration(generation, snapshot("project-1", "", 0));
        let mut event = delta(1, "");
        event.kind = ChatEventKind::TurnFailed;
        event.detail = ChatEventDetail::Failed {
            error: ChatEventError {
                code: "provider_timeout".into(),
                message: "timeout".into(),
            },
        };
        assert!(matches!(
            model.consume_event(event),
            ChatEventOutcome::Reconcile { .. }
        ));
    }

    #[derive(Default)]
    struct RecordingChatBackend {
        sent: Mutex<Vec<(String, String)>>,
        sent_targets: Mutex<Vec<Option<ChatExecutionTargetRequest>>>,
        cancelled: Mutex<Vec<String>>,
        retargeted: Mutex<Vec<(String, ChatExecutionTargetRequest)>>,
        send_barriers: Mutex<HashMap<String, tokio::sync::oneshot::Receiver<()>>>,
        failed_sends: Mutex<HashSet<String>>,
    }

    impl RecordingChatBackend {
        fn unavailable<T: Send + 'static>() -> ChatFuture<T> {
            Box::pin(async { Err(UiCommandError::worker("operation not configured")) })
        }
    }

    impl ChatBackend for RecordingChatBackend {
        fn hydrate(&self, _: String, _: Option<String>) -> ChatFuture<ChatHydration> {
            Self::unavailable()
        }

        fn load_conversation(&self, _: String) -> ChatFuture<GetChatConversationResponse> {
            Self::unavailable()
        }

        fn create_conversation(
            &self,
            _: String,
            _: ChatExecutionTargetRequest,
        ) -> ChatFuture<GetChatConversationResponse> {
            Self::unavailable()
        }

        fn delete_conversation(&self, _: String) -> ChatFuture<bool> {
            Self::unavailable()
        }

        fn send_message(
            &self,
            conversation_id: String,
            content: String,
            expected_target: Option<ChatExecutionTargetRequest>,
        ) -> ChatFuture<(SendChatMessageResponse, GetChatConversationResponse)> {
            self.sent_targets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(expected_target);
            self.sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((conversation_id.clone(), content));
            let mut assistant = message("", 0);
            assistant.id = format!("assistant-started-{conversation_id}");
            assistant.conversation_id = conversation_id.clone();
            assistant.ordinal = 3;
            assistant.status = "pending".into();
            let suffix = if conversation_id == "conversation-1" {
                String::new()
            } else {
                format!("-{conversation_id}")
            };
            let execution_id = format!("execution-started{suffix}");
            let generation_id = format!("generation-started{suffix}");
            assistant.execution_id = Some(execution_id.clone());
            assistant.generation_id = Some(generation_id.clone());
            let barrier = self.send_barriers.lock().unwrap().remove(&conversation_id);
            let failed = self.failed_sends.lock().unwrap().contains(&conversation_id);
            Box::pin(async move {
                if let Some(barrier) = barrier {
                    let _ = barrier.await;
                }
                if failed {
                    return Err(UiCommandError::worker("fixture send failure"));
                }
                Ok((
                    SendChatMessageResponse {
                        execution_id,
                        generation_id,
                        user_message_id: "user-started".into(),
                        assistant_message_id: assistant.id.clone(),
                    },
                    GetChatConversationResponse {
                        conversation: conversation(&conversation_id, "project-1"),
                        messages: vec![assistant],
                        tool_calls: Vec::new(),
                    },
                ))
            })
        }

        fn cancel_turn(&self, execution_id: String) -> ChatFuture<bool> {
            self.cancelled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(execution_id);
            Box::pin(async { Ok(true) })
        }

        fn turn_snapshot(&self, _: String) -> ChatFuture<GetChatTurnSnapshotResponse> {
            Self::unavailable()
        }

        fn update_conversation_target(
            &self,
            conversation_id: String,
            target: ChatExecutionTargetRequest,
        ) -> ChatFuture<GetChatConversationResponse> {
            self.retargeted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((conversation_id.clone(), target.clone()));
            let mut retargeted = conversation(&conversation_id, "project-1");
            if let ChatExecutionTargetRequest::Cli { backend, model, .. } = target {
                retargeted.transport = ChatTransportKind::Cli;
                retargeted.connection_id = None;
                retargeted.cli_backend = Some(backend);
                retargeted.provider_id = backend.as_str().into();
                retargeted.model_id = model.unwrap_or_else(|| "default".into());
            }
            Box::pin(async move {
                Ok(GetChatConversationResponse {
                    conversation: retargeted,
                    messages: Vec::new(),
                    tool_calls: Vec::new(),
                })
            })
        }

        fn save_default_target(&self, _: String, _: ChatExecutionTargetRequest) -> ChatFuture<()> {
            Self::unavailable()
        }
    }

    #[gpui::test]
    fn concurrent_sends_keep_composers_and_cancellation_independent(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingChatBackend::default());
        let (first_release, first_barrier) = tokio::sync::oneshot::channel();
        let (second_release, second_barrier) = tokio::sync::oneshot::channel();
        backend
            .send_barriers
            .lock()
            .unwrap()
            .insert("conversation-1".into(), first_barrier);
        backend
            .send_barriers
            .lock()
            .unwrap()
            .insert("conversation-2".into(), second_barrier);
        let model_backend: Arc<dyn ChatBackend> = backend.clone();
        let model = cx.new(|cx| AppModel::new_for_test_with_chat_backend(model_backend, cx));
        model.update(cx, |model, cx| {
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                let mut hydration = snapshot("project-1", "history", 1);
                hydration.selected.as_mut().unwrap().messages[0].status = "completed".into();
                chat.apply_hydration(generation, hydration);
                chat.conversations
                    .push(conversation("conversation-2", "project-1"));
                chat.acknowledge_disclosure_for("conversation-2");
            });
            for id in ["conversation-1", "conversation-2"] {
                let input = model.ensure_chat_composer(id, cx);
                input.update(cx, |input, cx| {
                    input.set_value(format!("question-{id}"), cx)
                });
                model.send_chat_message_to(id.into(), format!("question-{id}"), cx);
            }
        });
        cx.run_until_parked();
        assert_eq!(backend.sent.lock().unwrap().len(), 2);
        model.update(cx, |model, cx| {
            assert!(model.chat.read(cx).is_busy("conversation-1"));
            assert!(model.chat.read(cx).is_busy("conversation-2"));
            let first = model.chat_composer_for("conversation-1").unwrap();
            first.update(cx, |input, cx| input.set_value("a newer draft", cx));
            model.chat.update(cx, |chat, _| {
                chat.select_conversation_locally("conversation-2");
            });
            model.ensure_chat_composer("conversation-2", cx);
        });
        second_release.send(()).unwrap();
        cx.run_until_parked();
        model.update(cx, |model, cx| {
            assert!(model.chat.read(cx).is_busy("conversation-1"));
            assert!(model.chat.read(cx).is_streaming("conversation-2"));
            assert!(
                model
                    .chat_composer_for("conversation-2")
                    .unwrap()
                    .read(cx)
                    .value()
                    .is_empty()
            );
            model.cancel_chat_turn_for("conversation-2".into(), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            backend.cancelled.lock().unwrap().as_slice(),
            &["execution-started-conversation-2"]
        );
        first_release.send(()).unwrap();
        cx.run_until_parked();
        model.read_with(cx, |model, cx| {
            assert!(model.chat.read(cx).is_streaming("conversation-1"));
            assert_eq!(
                model
                    .chat
                    .read(cx)
                    .active_conversation
                    .as_ref()
                    .map(AsRef::as_ref),
                Some("conversation-2")
            );
            assert_eq!(
                model
                    .chat_composer_for("conversation-1")
                    .unwrap()
                    .read(cx)
                    .value(),
                "a newer draft"
            );
            assert_eq!(
                model.chat_draft_input.entity_id(),
                model
                    .chat_composer_for("conversation-2")
                    .unwrap()
                    .entity_id()
            );
        });
    }

    #[gpui::test]
    fn shared_composer_restores_a_saved_draft_without_replacing_new_edits(cx: &mut TestAppContext) {
        let backend: Arc<dyn ChatBackend> = Arc::new(RecordingChatBackend::default());
        let model = cx.new(|cx| AppModel::new_for_test_with_chat_backend(backend, cx));
        model.update(cx, |model, cx| {
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                chat.apply_hydration(generation, snapshot("project-1", "history", 1));
            });
            model
                .companion_dock
                .restored_drafts
                .insert("conversation-1".into(), "saved draft 🦀".into());
            let input = model.ensure_chat_composer("conversation-1", cx);
            assert_eq!(input.read(cx).value(), "saved draft 🦀");
            assert_eq!(input.entity_id(), model.chat_draft_input.entity_id());
            assert!(
                !model
                    .companion_dock
                    .restored_drafts
                    .contains_key("conversation-1")
            );
            input.update(cx, |input, cx| input.set_value("a newer draft", cx));
            model
                .companion_dock
                .restored_drafts
                .insert("conversation-1".into(), "stale load".into());
            let same = model.ensure_chat_composer("conversation-1", cx);
            assert_eq!(input.entity_id(), same.entity_id());
            assert_eq!(same.read(cx).value(), "a newer draft");
        });
    }

    #[gpui::test]
    fn rejected_send_preserves_draft_and_does_not_set_another_conversations_error(
        cx: &mut TestAppContext,
    ) {
        let backend = Arc::new(RecordingChatBackend::default());
        backend
            .failed_sends
            .lock()
            .unwrap()
            .insert("conversation-1".into());
        let (release, barrier) = tokio::sync::oneshot::channel();
        backend
            .send_barriers
            .lock()
            .unwrap()
            .insert("conversation-1".into(), barrier);
        let model_backend: Arc<dyn ChatBackend> = backend.clone();
        let model = cx.new(|cx| AppModel::new_for_test_with_chat_backend(model_backend, cx));
        model.update(cx, |model, cx| {
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                let mut hydration = snapshot("project-1", "history", 1);
                hydration.selected.as_mut().unwrap().messages[0].status = "completed".into();
                chat.apply_hydration(generation, hydration);
                chat.conversations
                    .push(conversation("conversation-2", "project-1"));
            });
            model
                .chat_draft_input
                .update(cx, |input, cx| input.set_value("keep this question", cx));
            model.send_chat_message(cx);
            model.chat.update(cx, |chat, _| {
                chat.select_conversation_locally("conversation-2");
            });
            model.ensure_chat_composer("conversation-2", cx);
        });
        cx.run_until_parked();
        release.send(()).unwrap();
        cx.run_until_parked();
        model.read_with(cx, |model, cx| {
            assert_eq!(
                model
                    .chat_composer_for("conversation-1")
                    .unwrap()
                    .read(cx)
                    .value(),
                "keep this question"
            );
            assert!(model.chat.read(cx).error_for("conversation-1").is_some());
            assert!(model.chat.read(cx).error_for("conversation-2").is_none());
            assert!(model.chat.read(cx).error.is_none());
        });
    }

    /// Changing the panel with a conversation open retargets that
    /// conversation, so its next message uses what the panel shows.
    #[gpui::test]
    fn changing_the_panel_retargets_the_open_conversation(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingChatBackend::default());
        let model_backend: Arc<dyn ChatBackend> = backend.clone();
        let model = cx.new(|cx| AppModel::new_for_test_with_chat_backend(model_backend, cx));
        model.update(cx, |model, cx| {
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                chat.apply_hydration(generation, snapshot("project-1", "history", 1));
            });
            model.set_chat_transport(ChatTransportKind::Cli, cx);
            model.set_chat_cli_backend(CliBackend::Claude, cx);
            assert_eq!(model.chat.read(cx).selected_cli_backend, CliBackend::Codex);
        });
        cx.run_until_parked();
        model.update(cx, |model, cx| {
            model.set_chat_cli_backend(CliBackend::Claude, cx)
        });
        cx.run_until_parked();
        let calls = backend
            .retargeted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            calls.last(),
            Some(&(
                "conversation-1".to_string(),
                ChatExecutionTargetRequest::Cli {
                    backend: CliBackend::Claude,
                    model: None,
                    model_profile: None,
                }
            ))
        );
        model.update(cx, |model, cx| {
            let chat = model.chat.read(cx);
            let active = chat.active_conversation_record().unwrap();
            assert_eq!(active.cli_backend, Some(CliBackend::Claude));
            assert_eq!(chat.selected_cli_backend, CliBackend::Claude);
            assert_eq!(chat.selected_transport, ChatTransportKind::Cli);
        });
    }

    #[gpui::test]
    fn starts_and_cancels_chat_turns_without_ipc(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingChatBackend::default());
        let model_backend: Arc<dyn ChatBackend> = backend.clone();
        let model = cx.new(|cx| AppModel::new_for_test_with_chat_backend(model_backend, cx));
        model.update(cx, |model, cx| {
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                let mut hydration = snapshot("project-1", "history", 1);
                hydration.selected.as_mut().unwrap().messages[0].status = "completed".into();
                hydration.selected.as_mut().unwrap().messages[0].execution_id = None;
                chat.apply_hydration(generation, hydration);
            });
            model.chat_draft_input.update(cx, |input, cx| {
                input.set_value("Explain the architecture", cx)
            });
            model.send_chat_message(cx);
        });
        cx.run_until_parked();
        assert_eq!(
            backend
                .sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &[("conversation-1".into(), "Explain the architecture".into())]
        );
        assert_eq!(
            backend.sent_targets.lock().unwrap().as_slice(),
            &[Some(ChatExecutionTargetRequest::Api {
                connection_id: "eligible".into(),
            })]
        );

        model.update(cx, |model, cx| model.cancel_chat_turn(cx));
        cx.run_until_parked();
        assert_eq!(
            backend
                .cancelled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &["execution-started"]
        );
    }

    #[gpui::test]
    fn enter_sends_multiline_draft_once_and_keeps_draft_while_streaming(cx: &mut TestAppContext) {
        use gpui::{Focusable, px, size};
        cx.update(crate::text_input::register_text_input_keybindings);
        let backend = Arc::new(RecordingChatBackend::default());
        let model_backend: Arc<dyn ChatBackend> = backend.clone();
        let (model, visual) = cx.add_window_view(|window, cx| {
            let mut model = AppModel::new_for_test_with_chat_backend(model_backend, cx);
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some("project-1".into())
            });
            model.select_view(crate::AppView::Chat, cx);
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                let mut hydration = snapshot("project-1", "history", 1);
                hydration.selected.as_mut().unwrap().messages[0].status = "completed".into();
                hydration.selected.as_mut().unwrap().messages[0].execution_id = None;
                chat.apply_hydration(generation, hydration);
            });
            model
                .chat_draft_input
                .read(cx)
                .focus_handle(cx)
                .focus(window);
            model
        });
        visual.simulate_resize(size(px(960.0), px(640.0)));
        visual.simulate_input("First line 👋");
        visual.simulate_keystrokes("shift-enter");
        visual.simulate_input("Second line");
        visual.run_until_parked();
        assert!(backend.sent.lock().unwrap().is_empty());
        visual.simulate_keystrokes("enter enter");
        visual.run_until_parked();
        assert_eq!(
            *backend.sent.lock().unwrap(),
            vec![("conversation-1".into(), "First line 👋\nSecond line".into())]
        );
        visual.simulate_input("Next draft");
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(backend.sent.lock().unwrap().len(), 1);
        model.read_with(visual, |m, cx| {
            assert_eq!(m.chat_draft_input.read(cx).value(), "Next draft")
        });
    }

    #[gpui::test]
    fn cancels_active_turn_without_mutating_provider_state(cx: &mut TestAppContext) {
        let backend = Arc::new(RecordingChatBackend::default());
        let model_backend: Arc<dyn ChatBackend> = backend.clone();
        let model = cx.new(|cx| AppModel::new_for_test_with_chat_backend(model_backend, cx));
        model.update(cx, |model, cx| {
            let generation = model
                .chat
                .update(cx, |chat, _| chat.begin_hydration("project-1".into(), None));
            model.chat.update(cx, |chat, _| {
                chat.apply_hydration(generation, snapshot("project-1", "", 0));
            });
            model.cancel_chat_turn(cx);
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let chat = model.read(cx).chat.read(cx);
            assert_eq!(chat.connections.len(), 2);
            assert!(chat.connections[0].chat_eligible);
            assert!(!chat.connections[1].chat_eligible);
        });
        assert_eq!(
            backend
                .cancelled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            &["execution-1"]
        );
    }
}
