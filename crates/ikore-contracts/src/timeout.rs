//! Agent timeout values; scheduling and process monitoring live in the runtime.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::llm::AgentRole;

/// How much longer a reasoning turn may stay silent than an ordinary one. The
/// absolute ceiling still applies, so this widens the window without ever
/// letting a genuinely stuck process run indefinitely.
const REASONING_SILENCE_FACTOR: u64 = 3;

/// CLI-specific budget. The API route keeps its independently configured deadline.
#[derive(Debug, Clone, Copy)]
pub struct CliTimeoutBudget {
    pub policy: AgentTimeoutPolicy,
    pub deadline: Instant,
}

impl CliTimeoutBudget {
    pub fn for_role(role: AgentRole, initial_timeout_ms: u64) -> Self {
        let mut policy = AgentTimeoutPolicy::for_role(role);
        policy.base_timeout_ms = policy
            .base_timeout_ms
            .max(initial_timeout_ms)
            .min(policy.max_timeout_ms);
        Self {
            deadline: Instant::now() + policy.max_timeout(),
            policy,
        }
    }
}

/// Role-specific time limits for progressively streaming agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTimeoutPolicy {
    /// Initial time allowed before the first response or activity.
    pub base_timeout_ms: u64,
    /// Maximum silence allowed between ordinary output events.
    pub inactivity_window_ms: u64,
    /// Absolute ceiling that progress cannot extend.
    pub max_timeout_ms: u64,
    /// Time added to the active deadline for each progress event.
    pub extension_increment_ms: u64,
    /// Silence allowed while the provider reports that the model is reasoning.
    ///
    /// Reasoning may consume tokens without delivering text. This wider
    /// window keeps an active reasoning turn from looking like a stuck run.
    pub reasoning_silence_window_ms: u64,
}

impl Default for AgentTimeoutPolicy {
    fn default() -> Self {
        Self::new(
            180_000, // 3 min base
            90_000,  // 90s silence window
            900_000, // 15 min ceiling
            30_000,  // +30s per delivery
        )
    }
}

impl AgentTimeoutPolicy {
    pub const fn new(
        base_timeout_ms: u64,
        inactivity_window_ms: u64,
        max_timeout_ms: u64,
        extension_increment_ms: u64,
    ) -> Self {
        Self {
            base_timeout_ms,
            inactivity_window_ms,
            max_timeout_ms,
            extension_increment_ms,
            reasoning_silence_window_ms: inactivity_window_ms
                .saturating_mul(REASONING_SILENCE_FACTOR),
        }
    }

    pub fn base_timeout(&self) -> Duration {
        Duration::from_millis(self.base_timeout_ms)
    }

    pub fn inactivity_window(&self) -> Duration {
        Duration::from_millis(self.inactivity_window_ms)
    }

    pub fn reasoning_silence_window(&self) -> Duration {
        Duration::from_millis(self.reasoning_silence_window_ms)
    }

    pub fn max_timeout(&self) -> Duration {
        Duration::from_millis(self.max_timeout_ms)
    }

    pub fn extension_increment(&self) -> Duration {
        Duration::from_millis(self.extension_increment_ms)
    }

    /// The existing default limits for one agent role.
    pub fn for_role(role: AgentRole) -> Self {
        match role {
            AgentRole::FeatureAgent => Self::new(
                300_000, // 5 min base for dense planning, schemas and reasoning
                90_000,  // 90s silence window
                900_000, // 15 min ceiling
                30_000,  // +30s per frame or delta
            ),
            AgentRole::BackendAgent => Self::new(
                360_000,   // 6 min base for code generation
                90_000,    // 90s silence
                1_200_000, // 20 min ceiling
                30_000,
            ),
            AgentRole::TestAgent => Self::new(
                360_000,   // 6 min base for test generation
                90_000,    // 90s silence
                1_200_000, // 20 min ceiling
                30_000,
            ),
            AgentRole::TaskSpecifier => Self::new(
                240_000, // 4 min base for batch specification
                90_000,  // 90s silence
                900_000, // 15 min ceiling
                30_000,
            ),
            AgentRole::ContractAgent => Self::new(
                240_000, // 4 min base for contracts and interfaces
                90_000,  // 90s silence
                900_000, // 15 min ceiling
                30_000,
            ),
            AgentRole::Integrator => Self::new(
                240_000, // 4 min base for reconciliation and publication
                90_000,  // 90s silence
                900_000, // 15 min ceiling
                30_000,
            ),
            AgentRole::LearningAgent => Self::new(
                240_000, // 4 min base for knowledge graph synthesis
                90_000,  // 90s silence
                900_000, // 15 min ceiling
                30_000,
            ),
            AgentRole::Visualizer => Self::new(
                120_000, // 2 min base (a short JSON note over a delivered brief)
                60_000,  // 60s silence
                300_000, // 5 min ceiling: the Brief never waits for it
                30_000,
            ),
            AgentRole::NotesScribe => Self::new(
                180_000, // 3 min base (one note from the developer's own words)
                90_000,  // 90s silence, as IntakeRouter
                600_000, // 10 min ceiling
                30_000,
            ),
            AgentRole::DocumentationReconciler => Self::new(
                240_000, // 4 min base (one note and the diffs of its files)
                90_000,  // 90s silence
                600_000, // 10 min ceiling: the developer is waiting on it
                30_000,
            ),
            AgentRole::IntakeRouter => Self::new(
                180_000, // 3 min base for initial triage
                90_000,  // 90s silence, allowing upstream 5xx backoff beyond 60s
                600_000, // 10 min ceiling
                30_000,
            ),
        }
    }

    /// Limits for Companion chat interactions.
    pub fn companion() -> Self {
        Self::new(
            120_000, // 2 min base
            60_000,  // 60s silence
            600_000, // 10 min ceiling
            30_000,
        )
    }

    /// Limits for manual or free-form runs.
    pub fn manual() -> Self {
        Self::new(
            1_800_000, // 30 min base
            120_000,   // 2 min silence
            3_600_000, // 60 min ceiling
            60_000,
        )
    }
}

/// The deadline condition that stopped a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutReason {
    /// The process stayed silent beyond the allowed window.
    Inactivity,
    /// The process exceeded its absolute role-specific ceiling.
    MaxCeiling,
}

impl TimeoutReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inactivity => "cli_inactivity_timeout",
            Self::MaxCeiling => "cli_timeout",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Inactivity => "The CLI process stayed idle past the limit without output.",
            Self::MaxCeiling => {
                "The CLI process exceeded the maximum time allowed for the operation."
            }
        }
    }
}
