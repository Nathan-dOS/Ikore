use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::chat_service::{
    CancelChatTurnRequest, ChatCommandError, ChatConversation, ChatEventDetail, ChatEventEnvelope,
    ChatEventKind, ChatExecutionTargetRequest, ChatMessage, ChatToolCall, ChatTransportKind,
    CreateChatConversationRequest, DeleteChatConversationRequest, GetChatConversationRequest,
    GetChatConversationResponse, GetChatTurnSnapshotRequest, GetChatTurnSnapshotResponse,
    ListChatConversationsRequest, SendChatMessageRequest, SendChatMessageResponse,
};
use hvc_core::llm::ProviderConnection;
use hvc_core::terminals::{
    CliAvailability, CliAvailabilityState, CliBackend, CliModelSelectionSource,
};

use crate::errors::UiCommandError;

fn terminal_error_for_chat(error: hvc_core::terminals::TerminalError) -> UiCommandError {
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
    ) -> ChatFuture<(SendChatMessageResponse, GetChatConversationResponse)>;
    fn cancel_turn(&self, execution_id: String) -> ChatFuture<bool>;
    fn turn_snapshot(&self, generation_id: String) -> ChatFuture<GetChatTurnSnapshotResponse>;
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
                    Ok(ChatHydration {
                        project_id,
                        connections,
                        cli_availability,
                        conversations,
                        selected,
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

#[derive(Debug, Default)]
pub struct ChatModel {
    pub(crate) tool_activity_open: bool,
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
    pub streaming: bool,
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<UiCommandError>,
    pub(crate) pending_delete_conversation_id: Option<SharedString>,
    acknowledged_disclosures: HashSet<String>,
    last_sequences: HashMap<String, u64>,
    hydration_generation: u64,
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
        self.project_id = Some(project_id.into());
        self.connections.clear();
        self.cli_availability.clear();
        self.conversations.clear();
        self.messages_by_conversation.clear();
        self.tool_calls_by_conversation.clear();
        self.last_sequences.clear();
        self.active_conversation = restored_conversation_id.map(Into::into);
        self.selected_connection_id = None;
        self.selected_transport = ChatTransportKind::Api;
        self.selected_cli_backend = CliBackend::Codex;
        self.selected_cli_model = None;
        self.selected_cli_model_profile = None;
        self.selected_cli_model_source = CliModelSelectionSource::Default;
        self.pending_delete_conversation_id = None;
        self.loading = true;
        self.busy = false;
        self.streaming = false;
        self.error = None;
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
        self.conversations = hydration.conversations;
        let selected_connection_id = self
            .eligible_connections()
            .next()
            .map(|connection| SharedString::from(connection.id.clone()));
        self.selected_connection_id = selected_connection_id;
        self.active_conversation = hydration
            .selected
            .as_ref()
            .map(|snapshot| SharedString::from(snapshot.conversation.id.clone()));
        if let Some(snapshot) = hydration.selected {
            self.store_conversation_snapshot(snapshot);
        }
        self.loading = false;
        self.error = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn fail_hydration(&mut self, generation: u64, error: UiCommandError) -> bool {
        if generation != self.hydration_generation {
            return false;
        }
        self.loading = false;
        self.error = Some(error);
        true
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

    pub(crate) fn active_messages(&self) -> &[ChatMessage] {
        self.active_conversation
            .as_ref()
            .and_then(|id| self.messages_by_conversation.get(id.as_ref()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn active_tool_calls(&self) -> &[ChatToolCall] {
        self.active_conversation
            .as_ref()
            .and_then(|id| self.tool_calls_by_conversation.get(id.as_ref()))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn composer_available(&self) -> bool {
        self.project_id.is_some() && self.active_conversation_record().is_some()
    }

    pub(crate) fn disclosure_required(&self) -> bool {
        self.active_conversation_record().is_some() && self.active_messages().is_empty()
    }

    pub(crate) fn disclosure_accepted(&self) -> bool {
        if !self.disclosure_required() {
            return true;
        }
        self.active_conversation
            .as_ref()
            .is_some_and(|id| self.acknowledged_disclosures.contains(id.as_ref()))
    }

    pub(crate) fn acknowledge_disclosure(&mut self) {
        if let Some(id) = &self.active_conversation {
            self.acknowledged_disclosures.insert(id.to_string());
        }
    }

    pub(crate) fn active_execution_id(&self) -> Option<String> {
        self.active_messages()
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
        self.pending_delete_conversation_id = None;
        self.loading = true;
        self.error = None;
        true
    }

    pub(crate) fn apply_selected_conversation(
        &mut self,
        project_id: &str,
        conversation_id: &str,
        snapshot: GetChatConversationResponse,
    ) -> bool {
        if self.project_id.as_ref().map(AsRef::as_ref) != Some(project_id)
            || self.active_conversation.as_ref().map(AsRef::as_ref) != Some(conversation_id)
            || snapshot.conversation.id != conversation_id
        {
            return false;
        }
        self.store_conversation_snapshot(snapshot);
        self.loading = false;
        self.error = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn begin_busy(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.error = None;
        true
    }

    pub(crate) fn finish_busy(&mut self) {
        self.busy = false;
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
        messages.sort_by_key(|message| message.ordinal);
        for message in &messages {
            if let Some(generation_id) = &message.generation_id {
                self.last_sequences
                    .insert(generation_id.clone(), message.last_sequence);
            }
        }
        self.messages_by_conversation
            .insert(conversation_id.clone(), messages);
        self.tool_calls_by_conversation
            .insert(conversation_id, snapshot.tool_calls);
        self.recompute_streaming();
    }

    pub(crate) fn apply_started_snapshot(
        &mut self,
        conversation_id: &str,
        generation_id: &str,
        snapshot: GetChatConversationResponse,
    ) -> bool {
        if self.active_conversation.as_ref().map(AsRef::as_ref) != Some(conversation_id) {
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
                messages
                    .iter_mut()
                    .find(|message| message.id == envelope.assistant_message_id)
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
        self.error = None;
        self.recompute_streaming();
        true
    }

    pub(crate) fn set_error(&mut self, error: UiCommandError) {
        self.loading = false;
        self.busy = false;
        self.error = Some(error);
    }

    fn recompute_streaming(&mut self) {
        self.streaming = self.active_messages().iter().any(|message| {
            message.role == "assistant"
                && matches!(message.status.as_str(), "pending" | "streaming")
        });
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
    use hvc_core::chat_service::{ChatEventDetail, ChatEventError};
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
            transport: hvc_core::chat_service::ChatTransportKind::Api,
            cli_backend: None,
            model_profile: None,
            model_source: hvc_core::terminals::CliModelSelectionSource::Default,
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
        }
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
        assert!(!model.composer_available());
        let generation = model.begin_hydration("project-1".into(), None);
        assert!(model.apply_hydration(generation, snapshot("project-1", "", 0)));
        assert!(model.composer_available());
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
                capabilities: hvc_core::terminals::CliCapabilities {
                    output_format: hvc_core::terminals::CliOutputFormat::JsonLines,
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
        cancelled: Mutex<Vec<String>>,
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
        ) -> ChatFuture<(SendChatMessageResponse, GetChatConversationResponse)> {
            self.sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((conversation_id.clone(), content));
            let mut assistant = message("", 0);
            assistant.status = "pending".into();
            assistant.execution_id = Some("execution-started".into());
            assistant.generation_id = Some("generation-started".into());
            Box::pin(async move {
                Ok((
                    SendChatMessageResponse {
                        execution_id: "execution-started".into(),
                        generation_id: "generation-started".into(),
                        user_message_id: "user-started".into(),
                        assistant_message_id: assistant.id.clone(),
                    },
                    GetChatConversationResponse {
                        conversation: conversation("conversation-1", "project-1"),
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
