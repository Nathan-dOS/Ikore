//! Supplier-reported token measurements shared by accounting producers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageMeasurement {
    pub counts: TokenCounts,
    pub observed_model: Option<String>,
    pub partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageTransport {
    Api,
    Cli,
}

impl UsageTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageContext {
    pub pulsation_id: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub project_id: Option<String>,
    pub transport: UsageTransport,
    pub provider_id: String,
    pub requested_model: Option<String>,
    pub started_at: String,
}
