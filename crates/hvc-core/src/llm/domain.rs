use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::error::LlmError;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

string_id!(ProviderId);
string_id!(ConnectionId);
string_id!(RouteId);
string_id!(ExecutionId);
string_id!(GenerationId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenaiResponses,
    OpenaiChat,
    AnthropicMessages,
}

impl ProviderProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiResponses => "openai_responses",
            Self::OpenaiChat => "openai_chat",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    FeatureAgent,
    IntakeRouter,
    TaskSpecifier,
    ContractAgent,
    BackendAgent,
    TestAgent,
    Integrator,
    LearningAgent,
    /// ADR-027 M5: optional notes on a checkpoint's Review Brief, written
    /// through a closed, host-validated contract. Off unless a project
    /// enables it.
    Visualizer,
}

impl AgentRole {
    pub const ALL: [Self; 9] = [
        Self::IntakeRouter,
        Self::TaskSpecifier,
        Self::ContractAgent,
        Self::BackendAgent,
        Self::TestAgent,
        Self::Integrator,
        Self::LearningAgent,
        Self::FeatureAgent,
        Self::Visualizer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FeatureAgent => "feature_agent",
            Self::IntakeRouter => "intake_router",
            Self::TaskSpecifier => "task_specifier",
            Self::ContractAgent => "contract_agent",
            Self::BackendAgent => "backend_agent",
            Self::TestAgent => "test_agent",
            Self::Integrator => "integrator",
            Self::LearningAgent => "learning_agent",
            Self::Visualizer => "visualizer",
        }
    }

    pub fn for_stage(stage: &str) -> Self {
        crate::orchestration::catalog::stage_role(stage).unwrap_or(Self::TaskSpecifier)
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == value.trim())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentExecutionTarget {
    Canonical {
        agent_role: AgentRole,
    },
    /// Internal durable slot for one composed work unit. It inherits routing
    /// and configuration from `agent_role`, but has its own terminal lifecycle.
    Ephemeral {
        slot_id: String,
        agent_role: AgentRole,
    },
    FreeForm {
        slot_id: String,
    },
}

impl AgentExecutionTarget {
    pub fn canonical(agent_role: AgentRole) -> Self {
        Self::Canonical { agent_role }
    }

    pub fn free_form(slot_id: impl Into<String>) -> Self {
        Self::FreeForm {
            slot_id: slot_id.into(),
        }
    }

    pub fn ephemeral(slot_id: impl Into<String>, agent_role: AgentRole) -> Self {
        Self::Ephemeral {
            slot_id: slot_id.into(),
            agent_role,
        }
    }

    pub fn agent_role(&self) -> Option<AgentRole> {
        match self {
            Self::Canonical { agent_role } => Some(*agent_role),
            Self::Ephemeral { agent_role, .. } => Some(*agent_role),
            Self::FreeForm { .. } => None,
        }
    }

    pub fn free_form_slot_id(&self) -> Option<&str> {
        match self {
            Self::Canonical { .. } | Self::Ephemeral { .. } => None,
            Self::FreeForm { slot_id } => Some(slot_id),
        }
    }

    pub fn stable_key(&self) -> String {
        match self {
            Self::Canonical { agent_role } => format!("canonical:{}", agent_role.as_str()),
            Self::Ephemeral { slot_id, .. } => format!("ephemeral:{slot_id}"),
            Self::FreeForm { slot_id } => format!("free_form:{slot_id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    Off,
    Shadow,
    On,
}

impl RoutingMode {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "shadow" => Self::Shadow,
            "on" => Self::On,
            _ => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::On => "on",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { name: String, schema: Value },
}

impl ResponseFormat {
    pub fn requires_structured_output(&self) -> bool {
        matches!(self, Self::JsonObject | Self::JsonSchema { .. })
    }
}

/// The response format a transport actually sends and validates: the strict
/// lowering carried by `dispatch.strict_schema` when the profile enforces
/// OpenAI structured outputs natively (`strict: true`), the request's own
/// format otherwise. A provider that only offers `json_object` never sees
/// the strict grammar, so its model answers the canonical shape and is
/// validated against it.
pub fn effective_response_format(
    request: &LlmRequest,
    profile: &super::profiles::ProviderProfile,
) -> ResponseFormat {
    match (&request.response_format, &request.dispatch.strict_schema) {
        (ResponseFormat::JsonSchema { name, .. }, Some(strict))
            if enforces_strict_schema(profile) =>
        {
            ResponseFormat::JsonSchema {
                name: name.clone(),
                schema: strict.clone(),
            }
        }
        (format, _) => format.clone(),
    }
}

/// Whether a provider profile applies OpenAI structured-output rules to the
/// schema it receives.
pub fn enforces_strict_schema(profile: &super::profiles::ProviderProfile) -> bool {
    matches!(
        profile.protocol,
        ProviderProtocol::OpenaiResponses | ProviderProtocol::OpenaiChat
    ) && profile.capabilities.json_schema
}

/// Composition identity of a work-unit attempt (ADR-022), carried by the
/// request so usage attribution and specialty configuration can see it.
/// Absent for every v1-v3 request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitDispatch {
    pub stage_execution_id: String,
    pub unit_id: String,
    pub attempt_id: String,
    pub executor_kind: String,
    /// Transport and model persisted in the stage profile before this attempt.
    pub frozen_transport: String,
    pub frozen_backend: Option<String>,
    pub frozen_model: String,
}

/// Per-request dispatch controls that the router and transports must honour.
#[derive(Debug, Clone, Default)]
pub struct DispatchPolicy {
    /// Explicit CLI initial/progressive/absolute budget. Absent callers retain
    /// `LlmRequest::deadline` as their hard ceiling, including shorter test budgets.
    /// API routing deliberately ignores this transport-specific override.
    pub cli_timeout: Option<crate::agent_timeout::CliTimeoutBudget>,
    /// Caps the router's own target attempts. `None` keeps the route's limit;
    /// composition sends `Some(1)` so that every real invocation holds a
    /// durable permit and no hidden application-level retry bypasses it.
    pub attempt_limit: Option<u8>,
    pub unit: Option<UnitDispatch>,
    /// Strict (OpenAI structured-output) lowering of the schema carried by
    /// `response_format`. Transports that enforce `strict: true` send and
    /// validate this variant instead; every other transport ignores it.
    pub strict_schema: Option<Value>,
    /// The caller parses and validates the answer itself and needs the raw
    /// bytes back even when they violate the schema: the router returns the
    /// output without its own schema gate. Composition sets it so a malformed
    /// reply becomes a bounded correction with a diagnostic instead of a
    /// blind resample.
    pub host_validated: bool,
}

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub execution_id: ExecutionId,
    pub generation_id: GenerationId,
    pub project_id: String,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub job_chain: String,
    pub target: AgentExecutionTarget,
    pub stage: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub response_format: ResponseFormat,
    pub max_output_tokens: u32,
    pub temperature: f32,
    pub deadline: Instant,
    pub stream: bool,
    pub legacy_provider: String,
    pub legacy_model: String,
    pub dispatch: DispatchPolicy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub execution_id: ExecutionId,
    pub generation_id: GenerationId,
    pub route_id: RouteId,
    pub connection_id: ConnectionId,
    pub provider_id: ProviderId,
    pub model: String,
    pub request_payload: Value,
    pub response_payload: Value,
    pub content: String,
    pub usage: LlmUsage,
    pub estimated_cost_usd: Option<f64>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConnection {
    pub id: String,
    pub provider_id: String,
    pub display_name: String,
    pub endpoint_url: Option<String>,
    pub default_model: String,
    pub enabled: bool,
    pub state: String,
    pub blocked_reason: Option<String>,
    pub cooldown_until: Option<String>,
    pub max_concurrency: u32,
    pub version: u64,
    pub has_api_key: bool,
    pub chat_eligible: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedConnection {
    pub id: ConnectionId,
    pub provider_id: ProviderId,
    pub credential_ref: String,
    pub endpoint_url: String,
    pub default_model: String,
    pub enabled: bool,
    pub state: String,
    pub cooldown_until: Option<String>,
    pub backoff_level: u32,
    pub max_concurrency: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteTarget {
    pub position: u32,
    pub connection_id: String,
    pub model_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRoute {
    pub id: String,
    pub name: String,
    pub strategy: String,
    pub max_attempts: u32,
    pub attempt_timeout_ms: u64,
    pub route_timeout_ms: u64,
    pub enabled: bool,
    pub version: u64,
    pub targets: Vec<RouteTarget>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRouteBinding {
    pub project_id: String,
    pub agent_role: String,
    pub stage: String,
    pub route_id: String,
    pub enabled: bool,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeFormProviderRouteBinding {
    pub project_id: String,
    pub slot_id: String,
    pub route_id: String,
    pub enabled: bool,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDecision {
    pub position: u32,
    pub connection_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub eligible: bool,
    pub gate_reason: Option<String>,
    pub score: Option<f64>,
    pub score_factors: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteDecision {
    pub route_id: String,
    pub strategy: String,
    pub affinity_hash: String,
    pub candidates: Vec<CandidateDecision>,
    pub ordered_targets: Vec<RouteTarget>,
    pub selected_connection_id: Option<String>,
    pub selected_model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LlmStreamEvent {
    RouteStarted {
        route_id: String,
    },
    AttemptStarted {
        ordinal: u32,
        connection_id: String,
        provider_id: String,
        model: String,
    },
    TokenDelta {
        text: String,
    },
    AttemptFailed {
        ordinal: u32,
        error_code: String,
    },
    RouteCompleted {
        provider_id: String,
        connection_id: String,
        model: String,
    },
    RouteFailed {
        error_code: String,
        next_eligible_at: Option<String>,
    },
    ResilienceChanged {
        scope: String,
        scope_id: String,
        from_state: String,
        to_state: String,
        retry_at: Option<String>,
    },
}

impl LlmStreamEvent {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::RouteStarted { .. } => "llm://route-started",
            Self::AttemptStarted { .. } => "llm://attempt-started",
            Self::TokenDelta { .. } => "llm://token-delta",
            Self::AttemptFailed { .. } => "llm://attempt-failed",
            Self::RouteCompleted { .. } => "llm://route-completed",
            Self::RouteFailed { .. } => "llm://route-failed",
            Self::ResilienceChanged { .. } => "llm://resilience-changed",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExecutionContext {
    pub project_id: String,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub target: AgentExecutionTarget,
    pub stage: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmExecutionListItem {
    pub execution_id: String,
    pub generation_id: String,
    pub target: AgentExecutionTarget,
    pub route_id: String,
    pub stage: String,
    pub status: String,
    pub visible_output: Option<String>,
    pub visible_output_complete: bool,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmEventEnvelope {
    pub protocol_version: u8,
    pub execution_id: String,
    pub generation_id: String,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<AgentExecutionContext>,
    #[serde(flatten)]
    pub event: LlmStreamEvent,
}

pub trait LlmEventSink: Send + Sync {
    fn emit(&self, envelope: &LlmEventEnvelope);
}

pub trait Clock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
}

pub trait JitterSource: Send + Sync {
    fn factor(&self, key: &str, attempt: u32) -> f64;
}

pub type GatewayFuture<'a> =
    Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>>;

pub trait LlmGateway: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: &'a LlmRequest,
        cancellation: &'a CancellationToken,
    ) -> GatewayFuture<'a>;
}
