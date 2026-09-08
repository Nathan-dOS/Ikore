use hvc_core::persistence_service::{PersistenceCommandError, PersistenceErrorCode};
use hvc_core::workspace_service::{WorkspaceCommandError, WorkspaceErrorCode};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UiCommandError {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) details: Option<Value>,
    pub(crate) remediation: Option<String>,
}

impl UiCommandError {
    pub(crate) fn worker(message: impl Into<String>) -> Self {
        Self {
            code: "io_error".to_string(),
            message: message.into(),
            details: None,
            remediation: Some(
                "Please try again. If the problem persists, restart the application.".to_string(),
            ),
        }
    }

    pub(crate) fn from_chat_parts(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            remediation: None,
        }
    }

    pub(crate) fn is_code(&self, code: &str) -> bool {
        self.code == code
    }

    pub(crate) fn presentable(&self) -> String {
        match self.remediation.as_deref() {
            Some(remediation) if !remediation.is_empty() => {
                format!("{} {}", self.message, remediation)
            }
            _ => self.message.clone(),
        }
    }
}

impl From<WorkspaceCommandError> for UiCommandError {
    fn from(error: WorkspaceCommandError) -> Self {
        Self {
            code: workspace_code(error.code).to_string(),
            message: error.message,
            details: error.details,
            remediation: error.remediation,
        }
    }
}

impl From<PersistenceCommandError> for UiCommandError {
    fn from(error: PersistenceCommandError) -> Self {
        Self {
            code: persistence_code(error.code).to_string(),
            message: error.message,
            details: error.details,
            remediation: error.remediation,
        }
    }
}

fn workspace_code(code: WorkspaceErrorCode) -> &'static str {
    match code {
        WorkspaceErrorCode::ValidationError => "validation_error",
        WorkspaceErrorCode::NotFound => "not_found",
        WorkspaceErrorCode::InvalidRoot => "invalid_root",
        WorkspaceErrorCode::PathForbidden => "path_forbidden",
        WorkspaceErrorCode::InvalidExtension => "invalid_extension",
        WorkspaceErrorCode::InvalidQuery => "invalid_query",
        WorkspaceErrorCode::Conflict => "conflict",
        WorkspaceErrorCode::Cancelled => "cancelled",
        WorkspaceErrorCode::IoError => "io_error",
    }
}

fn persistence_code(code: PersistenceErrorCode) -> &'static str {
    match code {
        PersistenceErrorCode::ValidationError => "validation_error",
        PersistenceErrorCode::NotFound => "not_found",
        PersistenceErrorCode::DbError => "db_error",
        PersistenceErrorCode::MigrationError => "migration_error",
        PersistenceErrorCode::IoError => "io_error",
        PersistenceErrorCode::RuntimeError => "runtime_error",
        PersistenceErrorCode::MissingApiKey => "missing_api_key",
        PersistenceErrorCode::ProviderTimeout => "provider_timeout",
        PersistenceErrorCode::ProviderBlocked => "provider_blocked",
        PersistenceErrorCode::AllTargetsExhausted => "all_targets_exhausted",
        PersistenceErrorCode::SchemaViolation => "schema_violation",
        PersistenceErrorCode::RouteConflict => "route_conflict",
        PersistenceErrorCode::StreamFailed => "stream_failed",
        PersistenceErrorCode::CliExecutionFailed => "cli_execution_failed",
    }
}

#[cfg(test)]
mod tests {
    use hvc_core::workspace_service::{WorkspaceCommandError, WorkspaceErrorCode};
    use serde_json::json;

    use super::UiCommandError;

    #[test]
    fn maps_untyped_failures_to_safe_io_error() {
        let error = UiCommandError::worker("worker interrupted");

        assert_eq!(error.code, "io_error");
        assert_eq!(error.message, "worker interrupted");
        assert!(error.details.is_none());
        assert!(error.remediation.is_some());
    }

    #[test]
    fn renders_workspace_command_error_without_losing_fields() {
        let error = UiCommandError::from(WorkspaceCommandError {
            code: WorkspaceErrorCode::Conflict,
            message: "divergent revision".to_string(),
            details: Some(json!({"expectedRevision": "1", "actualRevision": "2"})),
            remediation: Some("reload".to_string()),
        });

        assert_eq!(error.code, "conflict");
        assert_eq!(
            error.details,
            Some(json!({"expectedRevision": "1", "actualRevision": "2"}))
        );
        assert_eq!(error.remediation.as_deref(), Some("reload"));
        assert_eq!(error.presentable(), "divergent revision reload");
    }
}
