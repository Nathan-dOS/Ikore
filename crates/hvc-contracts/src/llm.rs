//! Agent identities and request values shared by host and transports.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// ADR-029: turns a developer's loose capture into a structured
    /// Companion note, through a closed, host-validated contract. It has no
    /// tools and never sees the repository.
    NotesScribe,
}

impl AgentRole {
    pub const ALL: [Self; 10] = [
        Self::IntakeRouter,
        Self::TaskSpecifier,
        Self::ContractAgent,
        Self::BackendAgent,
        Self::TestAgent,
        Self::Integrator,
        Self::LearningAgent,
        Self::FeatureAgent,
        Self::Visualizer,
        Self::NotesScribe,
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
            Self::NotesScribe => "notes_scribe",
        }
    }

    pub fn for_stage(stage: &str) -> Self {
        crate::stages::stage_role(stage).unwrap_or(Self::TaskSpecifier)
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
