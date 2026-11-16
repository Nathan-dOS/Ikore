//! Shared domain values for the host and its terminal subsystem.
//!
//! This crate contains no persistence, process execution or authorization logic.

pub mod env;
pub mod exchange;
pub mod external_changes;
pub mod line_diff;
pub mod llm;
pub mod redaction;
pub mod stages;
pub mod timeout;
pub mod usage;

pub use env::{env_flag_with_legacy, var_os_with_legacy, var_with_legacy};
pub use llm::{AgentExecutionTarget, AgentRole, ResponseFormat, UnitContextScope, UnitDispatch};
pub use stages::{PulsationStage, STAGES, canonical_stage, parse_stage, stage_role};
pub use timeout::{AgentTimeoutPolicy, CliTimeoutBudget, TimeoutReason};
pub use usage::{TokenCounts, UsageContext, UsageMeasurement, UsageTransport};
