//! Shared domain values for the host and its terminal subsystem.
//!
//! This crate contains no persistence, process execution or authorization logic.

pub mod llm;
pub mod stages;
pub mod timeout;
pub mod usage;

pub use llm::{AgentExecutionTarget, AgentRole, ResponseFormat, UnitContextScope, UnitDispatch};
pub use stages::{PulsationStage, STAGES, canonical_stage, parse_stage, stage_role};
pub use timeout::{AgentTimeoutPolicy, CliTimeoutBudget, TimeoutReason};
pub use usage::{TokenCounts, UsageContext, UsageMeasurement, UsageTransport};

pub mod redaction;
