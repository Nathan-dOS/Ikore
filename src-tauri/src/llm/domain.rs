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
    IntakeRouter,
    TaskSpecifier,
    ContractAgent,
    BackendAgent,
    TestAgent,
    Integrator,
}

impl AgentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntakeRouter => "intake_router",
            Self::TaskSpecifier => "task_specifier",
            Self::ContractAgent => "contract_agent",
            Self::BackendAgent => "backend_agent",
            Self::TestAgent => "test_agent",
            Self::Integrator => "integrator",
        }
    }

    pub fn for_stage(stage: &str) -> Self {
        match stage {
            "Intake" => Self::IntakeRouter,
            "SPECIFICATION" | "Specification" => Self::TaskSpecifier,
            "Contract" => Self::ContractAgent,
            "Code" => Self::BackendAgent,
            "Test" => Self::TestAgent,
            "Integrator" => Self::Integrator,
            _ => Self::TaskSpecifier,
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

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub execution_id: ExecutionId,
    pub generation_id: GenerationId,
    pub project_id: String,
    pub pulsation_id: Option<String>,
    pub job_id: Option<String>,
    pub job_chain: String,
    pub agent_role: AgentRole,
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
pub struct LlmEventEnvelope {
    pub protocol_version: u8,
    pub execution_id: String,
    pub generation_id: String,
    pub sequence: u64,
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
