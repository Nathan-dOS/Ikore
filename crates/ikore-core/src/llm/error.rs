use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmErrorKind {
    InvalidRequest,
    MissingApiKey,
    Authentication,
    Billing,
    Permission,
    ModelNotFound,
    RateLimited,
    Timeout,
    Network,
    ProviderUnavailable,
    InvalidResponse,
    ModelMismatch,
    ModelUnverified,
    OutputSchema,
    Cancelled,
    BackgroundDeferred,
    StreamFailed,
    RouteConflict,
    AllTargetsExhausted,
    Persistence,
    Sanitization,
}

impl LlmErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingApiKey => "missing_api_key",
            Self::Timeout => "provider_timeout",
            Self::Authentication | Self::Billing | Self::Permission => "provider_blocked",
            Self::AllTargetsExhausted => "all_targets_exhausted",
            Self::OutputSchema => "schema_violation",
            Self::StreamFailed => "stream_failed",
            Self::RouteConflict => "route_conflict",
            Self::ModelMismatch => "model_mismatch",
            Self::ModelUnverified => "model_unverified",
            Self::BackgroundDeferred => "background_deferred",
            Self::InvalidRequest => "validation_error",
            Self::Persistence => "db_error",
            Self::Sanitization => "sanitization_failed",
            _ => "runtime_error",
        }
    }

    pub fn recoverable(self) -> bool {
        matches!(
            self,
            Self::MissingApiKey
                | Self::Authentication
                | Self::Billing
                | Self::Permission
                | Self::ModelNotFound
                | Self::RateLimited
                | Self::Timeout
                | Self::Network
                | Self::ProviderUnavailable
                | Self::InvalidResponse
                | Self::OutputSchema
                | Self::StreamFailed
        )
    }

    pub fn affects_health(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Network | Self::ProviderUnavailable
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmError {
    pub kind: LlmErrorKind,
    pub message: String,
    pub provider_id: Option<String>,
    pub connection_id: Option<String>,
    pub model_id: Option<String>,
    pub http_status: Option<u16>,
    pub provider_code: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub next_eligible_at: Option<String>,
    pub visible_partial: Option<String>,
    pub causes: Vec<String>,
    /// Provider-reported identities observed before a rejected generation.
    #[serde(default)]
    pub observed_models: Vec<String>,
}

impl LlmError {
    pub fn new(kind: LlmErrorKind, message: impl Into<String>) -> Self {
        let message = crate::secret_redactor::redact_secrets(&message.into())
            .map(|result| result.text)
            .unwrap_or_else(|_| "LLM details unavailable after sanitization.".into());
        Self {
            kind,
            message,
            provider_id: None,
            connection_id: None,
            model_id: None,
            http_status: None,
            provider_code: None,
            retry_after_ms: None,
            next_eligible_at: None,
            visible_partial: None,
            causes: Vec::new(),
            observed_models: Vec::new(),
        }
    }

    pub fn with_target(mut self, provider: &str, connection: &str, model: &str) -> Self {
        self.provider_id = Some(provider.to_string());
        self.connection_id = Some(connection.to_string());
        self.model_id = Some(model.to_string());
        self
    }

    pub fn with_observed_models(mut self, models: Vec<String>) -> Self {
        self.observed_models = models;
        self
    }

    pub fn safe_cause(&self) -> String {
        format!("{}: {}", self.kind.code(), self.message)
    }

    pub fn recoverable(&self) -> bool {
        self.kind.recoverable() && self.visible_partial.as_deref().unwrap_or("").is_empty()
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LlmError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_failure_is_recoverable_only_before_visible_output() {
        let mut error = LlmError::new(LlmErrorKind::StreamFailed, "stream interrupted");
        assert!(error.recoverable());
        error.visible_partial = Some("partial".to_string());
        assert!(!error.recoverable());
    }
}
