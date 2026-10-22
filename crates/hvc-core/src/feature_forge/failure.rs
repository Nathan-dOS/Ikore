//! What a failed Feature Forge turn means, in words a developer can act on.
//!
//! A turn used to fail with whatever string the failing layer produced, and
//! the Incubation chat printed it verbatim ("Suggested dependency was not
//! supplied by the host"). Here the failure keeps its type instead: the layer
//! that knows what went wrong classifies it once, the UI explains it from
//! [`TurnFailure::explain`], and the next discovery prompt reports it to the
//! agent so a follow-up message can be a conversation about it.
//!
//! Explanations are host-authored English (ADR-017); only the agent's own
//! chat message may follow the developer's language.

use super::{FeatureTurn, TurnKind};
use crate::{
    llm::{LlmError, LlmErrorKind},
    persistence_service::PersistenceServiceError,
    secret_redactor::redact_secrets,
    terminals::TerminalError,
};
use serde::{Deserialize, Serialize};

/// Cap on the raw error kept for the collapsed "Technical detail" line.
pub(crate) const TECHNICAL_DETAIL_LIMIT: usize = 1024;
/// Cap on the repair notes attached to one turn.
pub(crate) const MAX_TURN_NOTICES: usize = 5;
/// Two discovery turns in a row failing the same way means asking the agent
/// again would spend a paid call to repeat the outcome.
const REPEATED_FAILURE_LIMIT: usize = 2;

/// Stable, serialized classification. Keep the names: they are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailureKind {
    /// Specification text was not English.
    ReplyLanguage,
    /// More than the three allowed questions.
    TooManyQuestions,
    /// Truncated, not JSON, or not the required structure.
    ReplyUnreadable,
    /// A cited source is foreign or changed on disk.
    SourceRejected,
    /// A suggested prerequisite matched no feature. Written only for rows
    /// that predate the host repairing this slip.
    DependencyUnresolved,
    /// The request would not fit the configured context budget.
    ContextTooLarge,
    /// The host could not qualify the input before external dispatch.
    Sanitization,
    /// Credentials, CLI installation or sign-in, route or model selection.
    ProviderSetup,
    /// Timeout, network, rate limit, process crash.
    ProviderTransient,
    /// The feature changed while the reply was being generated.
    StaleFeature,
    /// The app restarted mid-reply.
    Interrupted,
    /// The developer cancelled. Not an error.
    Cancelled,
    Unclassified,
}

impl TurnFailureKind {
    /// Resending the same message is a reasonable next step.
    pub const fn retry_allowed(self) -> bool {
        !matches!(
            self,
            Self::ContextTooLarge | Self::Sanitization | Self::Cancelled
        )
    }

    /// The reply reached the host and was wrong, so the agent can be told what
    /// was wrong and asked to fix it. Never for failures where the agent's own
    /// route is the suspect: asking a model that cannot be reached to explain
    /// itself only produces a second failure.
    pub const fn ask_agent_allowed(self) -> bool {
        matches!(
            self,
            Self::ReplyLanguage
                | Self::TooManyQuestions
                | Self::ReplyUnreadable
                | Self::SourceRejected
                | Self::DependencyUnresolved
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnFailure {
    pub kind: TurnFailureKind,
    /// The provider or CLI the failure names, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The raw error, redacted and bounded. Shown collapsed; never prompted.
    pub technical: String,
}

/// The three lines the failure card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureExplanation {
    pub summary: String,
    pub cause: String,
    pub remedy: String,
}

impl TurnFailure {
    pub fn new(kind: TurnFailureKind, technical: impl AsRef<str>) -> Self {
        Self {
            kind,
            subject: None,
            technical: bounded_technical(technical.as_ref()),
        }
    }

    pub(crate) fn with_subject(mut self, subject: impl Into<String>) -> Self {
        let subject = subject.into();
        if !subject.trim().is_empty() {
            self.subject = Some(subject);
        }
        self
    }

    pub fn retry_allowed(&self) -> bool {
        self.kind.retry_allowed()
    }

    pub fn ask_agent_allowed(&self) -> bool {
        self.kind.ask_agent_allowed()
    }

    pub(crate) fn from_llm(error: &LlmError) -> Self {
        use LlmErrorKind::*;
        let kind = match error.kind {
            MissingApiKey | Authentication | Billing | Permission | ModelNotFound
            | RouteConflict | ModelMismatch | ModelUnverified => TurnFailureKind::ProviderSetup,
            Timeout | Network | RateLimited | ProviderUnavailable | StreamFailed
            | BackgroundDeferred | AllTargetsExhausted | InvalidResponse => {
                TurnFailureKind::ProviderTransient
            }
            OutputSchema => TurnFailureKind::ReplyUnreadable,
            Cancelled => TurnFailureKind::Cancelled,
            InvalidRequest | Persistence => TurnFailureKind::Unclassified,
            Sanitization => TurnFailureKind::Sanitization,
        };
        let mut technical = error.safe_cause();
        if !error.causes.is_empty() {
            technical = format!("{technical} ({})", error.causes.join("; "));
        }
        let failure = Self::new(kind, technical);
        match error.provider_id.as_deref() {
            Some(provider) => failure.with_subject(provider),
            None => failure,
        }
    }

    pub(crate) fn from_terminal(error: &TerminalError) -> Self {
        let kind = match error {
            TerminalError::Redaction(_) => TurnFailureKind::Sanitization,
            TerminalError::AuthRequired(_)
            | TerminalError::VersionUnsupported(_)
            | TerminalError::NotFound(_)
            | TerminalError::Validation(_)
            | TerminalError::ModelMismatch { .. }
            | TerminalError::ModelUnverified { .. } => TurnFailureKind::ProviderSetup,
            TerminalError::Timeout
            | TerminalError::InactivityTimeout
            | TerminalError::Process(_)
            | TerminalError::RateLimited(_)
            | TerminalError::Io(_) => TurnFailureKind::ProviderTransient,
            TerminalError::OutputTruncated { .. }
            | TerminalError::OutputLimit
            | TerminalError::Json(_) => TurnFailureKind::ReplyUnreadable,
            TerminalError::Cancelled => TurnFailureKind::Cancelled,
            _ => TurnFailureKind::Unclassified,
        };
        let failure = Self::new(kind, format!("{}: {error}", error.code()));
        match error {
            TerminalError::AuthRequired(backend) | TerminalError::VersionUnsupported(backend) => {
                failure.with_subject(backend.clone())
            }
            _ => failure,
        }
    }

    /// Classifies a stored or host-authored message.
    ///
    /// Turns written before failures were typed carry only their error text,
    /// in English or in the Portuguese the CLI layer used to emit. Anything
    /// unrecognized stays [`TurnFailureKind::Unclassified`] with the text kept
    /// as technical detail rather than guessed at.
    pub fn from_legacy(text: &str) -> Self {
        let lower = text.to_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
        let kind = if has(&[
            "dependency was not supplied by the host",
            "prerequisite was not supplied",
        ]) {
            TurnFailureKind::DependencyUnresolved
        } else if has(&[
            "sanitization_failed",
            "cli_redaction_failed",
            "sanitization_input_limit",
        ]) {
            TurnFailureKind::Sanitization
        } else if has(&["exceeds the context budget"]) {
            TurnFailureKind::ContextTooLarge
        } else if has(&["changed while the", "no longer active"]) {
            TurnFailureKind::StaleFeature
        } else if has(&["application restarted"]) {
            TurnFailureKind::Interrupted
        } else if has(&["was cancelled", "cancelled"]) {
            TurnFailureKind::Cancelled
        } else if has(&["not english", "english prose", "non-english"]) {
            TurnFailureKind::ReplyLanguage
        } else if has(&["too many questions"]) {
            TurnFailureKind::TooManyQuestions
        } else if has(&["source belongs to a different project", "source changed;"]) {
            TurnFailureKind::SourceRejected
        } else if has(&[
            "invalid discovery language",
            "expected value at line",
            "eof while parsing",
            "missing field",
            "unknown field",
            "invalid type",
            "key must be a string",
            "schema",
            "is a required property",
            "additional properties",
            "output limit",
            "token limit",
        ]) {
            TurnFailureKind::ReplyUnreadable
        } else if has(&[
            "requires authentication",
            "not installed",
            "api key",
            "version is incompatible",
            "modelo esperado",
            "model_mismatch",
        ]) {
            TurnFailureKind::ProviderSetup
        } else if has(&[
            "timed out",
            "timeout",
            "idle past the limit",
            "excedeu",
            "cli run failed",
            "execução cli falhou",
            "encerrou com código",
            "rate limit",
            "network",
        ]) {
            TurnFailureKind::ProviderTransient
        } else {
            TurnFailureKind::Unclassified
        };
        Self::new(kind, text)
    }

    pub fn explain(&self) -> FailureExplanation {
        let text = |summary: &str, cause: &str, remedy: &str| FailureExplanation {
            summary: summary.into(),
            cause: cause.into(),
            remedy: remedy.into(),
        };
        match self.kind {
            TurnFailureKind::ReplyLanguage => text(
                "The agent's reply broke the English-only rule for specification text.",
                "Everything the agent writes into the specification (brief, evaluation, task names) must be in English. Only its chat message and questions may follow your language. Part of this reply was not, so nothing from it was applied.",
                "Try again, or ask the agent to redo it in English.",
            ),
            TurnFailureKind::TooManyQuestions => text(
                "The agent asked more than three questions at once.",
                "Each reply is limited to three questions so they can be answered together. This one had more, so it was not applied.",
                "Try again, or ask the agent to keep to three questions.",
            ),
            TurnFailureKind::ReplyUnreadable => text(
                "The agent's reply could not be used by the app.",
                "The answer was cut off, was not valid JSON, or did not match the structure the host requires (readable JSON, English specification text, at most three questions), so nothing from it was applied.",
                "Try again. If it keeps happening, ask the agent to answer more briefly.",
            ),
            TurnFailureKind::SourceRejected => text(
                "The agent cited a repository file the app could not verify.",
                "A source in the reply belongs to another project or changed on disk after it was read, so the host refused to attach it and did not apply the reply.",
                "Try again so the agent reads the current files.",
            ),
            TurnFailureKind::DependencyUnresolved => text(
                "The agent named a prerequisite feature the app does not have.",
                "It used an identifier that is not one of this project's features (for example a task identifier), so the suggestion could not be matched and the reply was not applied.",
                "Try again, or ask the agent to name the prerequisite by its feature title.",
            ),
            TurnFailureKind::Sanitization => text(
                "The host could not sanitize this input.",
                "The call stopped before sending unqualified content.",
                "Review the input size and local sanitization configuration, then submit a new turn.",
            ),
            TurnFailureKind::ContextTooLarge => text(
                "The conversation no longer fits the agent's context limit.",
                "The scope, brief and recent messages together are larger than the configured context budget, so the request was not sent.",
                "Shorten the feature scope, or raise the context limit in Settings, then send your message again.",
            ),
            TurnFailureKind::ProviderSetup
                if self.technical.contains("model_unverified")
                    && self.technical.contains("CLI default has no fixed model ID") => text(
                "The agent needs an explicit CLI model.",
                "This agent is configured to use the CLI default, which does not identify a fixed model the app can verify. Your message is saved.",
                "Open Agents, choose and save an explicit model for the agent handling this reply, then select Try again. No app restart is needed.",
            ),
            TurnFailureKind::ProviderSetup => FailureExplanation {
                summary: match &self.subject {
                    Some(subject) => format!("{subject} could not be used for this reply."),
                    None => "The agent's model could not be used for this reply.".into(),
                },
                cause: "The provider or CLI refused the request or is not set up: a missing or invalid key, a CLI that is not installed or signed in, or a route that points at an unavailable model. Your message is saved.".into(),
                remedy: "Open Agents (or Settings → Routes), fix the Feature Agent's route or sign in to its CLI, then try again.".into(),
            },
            TurnFailureKind::ProviderTransient => text(
                "The model could not finish this reply.",
                "The provider or CLI timed out, lost the connection, hit a rate limit or stopped unexpectedly. Your message is saved and nothing was applied.",
                "Try again in a moment.",
            ),
            TurnFailureKind::StaleFeature => text(
                "The feature changed while the agent was answering.",
                "The feature was edited or moved after the request started, so the reply no longer matches it and was not applied.",
                "Try again; the agent will use the current version.",
            ),
            TurnFailureKind::Interrupted => text(
                "The app was closed before the agent finished.",
                "The reply was still being generated when the app restarted. Your message is saved.",
                "Try again.",
            ),
            TurnFailureKind::Cancelled => text("You cancelled this response.", "", ""),
            TurnFailureKind::Unclassified => text(
                "This reply could not be completed.",
                "The app hit an error it has no specific explanation for. The technical detail shows what it reported.",
                "Try again. If it keeps failing, share the technical detail.",
            ),
        }
    }
}

/// Redacts and bounds a raw error for storage.
pub(crate) fn bounded_technical(raw: &str) -> String {
    let redacted = match redact_secrets(raw) {
        Ok(result) => result.text,
        Err(_) => "(technical detail withheld: redaction unavailable)".to_string(),
    };
    let trimmed = redacted.trim();
    if trimmed.chars().count() <= TECHNICAL_DETAIL_LIMIT {
        return trimmed.to_string();
    }
    trimmed
        .chars()
        .take(TECHNICAL_DETAIL_LIMIT)
        .collect::<String>()
        + "…"
}

/// Keeps the repair notes to their cap.
pub(crate) fn cap_notices(mut notices: Vec<String>) -> Vec<String> {
    notices.truncate(MAX_TURN_NOTICES);
    notices
}

/// What the developer may do about the newest failed turn of a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureActions {
    pub retry: bool,
    pub ask_agent: bool,
}

/// The newest turn of `turn_kind`, when it failed and something can be done.
///
/// Only that turn is actionable: an older failure has already been followed by
/// another attempt, so offering to redo it would replay a stale message.
/// Asking the agent is a discussion of a discovery reply, so analyses never
/// offer it, and it stops being offered after two identical failures in a row.
pub fn failure_actions(
    history: &[FeatureTurn],
    turn_kind: TurnKind,
) -> Option<(String, FailureActions)> {
    let turns: Vec<&FeatureTurn> = history
        .iter()
        .filter(|turn| turn.kind == turn_kind)
        .collect();
    let latest = turns.last()?;
    let failure = latest.failure.as_ref()?;
    if !matches!(latest.status.as_str(), "failed" | "interrupted")
        || failure.kind == TurnFailureKind::Cancelled
    {
        return None;
    }
    let repeated = turns
        .iter()
        .rev()
        .take(REPEATED_FAILURE_LIMIT)
        .filter(|turn| turn.failure.as_ref().map(|f| f.kind) == Some(failure.kind))
        .count()
        >= REPEATED_FAILURE_LIMIT;
    Some((
        latest.id.clone(),
        FailureActions {
            retry: failure.retry_allowed(),
            ask_agent: turn_kind == TurnKind::Discovery && failure.ask_agent_allowed() && !repeated,
        },
    ))
}

/// A failure that has not been typed yet, or a database error.
#[derive(Debug)]
pub(crate) enum TurnError {
    Failure(TurnFailure),
    Other(PersistenceServiceError),
}

impl TurnError {
    pub(crate) fn into_failure(self) -> TurnFailure {
        match self {
            Self::Failure(failure) => failure,
            Self::Other(error) => TurnFailure::from_legacy(&error.to_string()),
        }
    }
}

impl<E: Into<PersistenceServiceError>> From<E> for TurnError {
    fn from(error: E) -> Self {
        Self::Other(error.into())
    }
}

/// Why a model request produced no usable reply, before the reply is judged.
#[derive(Debug)]
pub(crate) enum RequestError {
    Cli(TerminalError),
    Llm(LlmError),
    /// The output was not JSON, or did not satisfy the response schema.
    Unreadable(String),
    ContextTooLarge(String),
    Other(PersistenceServiceError),
}

impl From<PersistenceServiceError> for RequestError {
    fn from(error: PersistenceServiceError) -> Self {
        Self::Other(error)
    }
}

impl From<RequestError> for TurnError {
    fn from(error: RequestError) -> Self {
        match error {
            RequestError::Cli(error) => Self::Failure(TurnFailure::from_terminal(&error)),
            RequestError::Llm(error) => Self::Failure(TurnFailure::from_llm(&error)),
            RequestError::Unreadable(detail) => {
                Self::Failure(TurnFailure::new(TurnFailureKind::ReplyUnreadable, detail))
            }
            RequestError::ContextTooLarge(detail) => {
                Self::Failure(TurnFailure::new(TurnFailureKind::ContextTooLarge, detail))
            }
            RequestError::Other(error) => Self::Other(error),
        }
    }
}

impl From<TurnFailure> for TurnError {
    fn from(failure: TurnFailure) -> Self {
        Self::Failure(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: &str, status: &str, kind: TurnKind, failure: Option<TurnFailure>) -> FeatureTurn {
        FeatureTurn {
            id: id.into(),
            feature_id: "feature".into(),
            feature_revision: 1,
            status: status.into(),
            user_message: "message".into(),
            assistant_message: None,
            error: failure.as_ref().map(|f| f.technical.clone()),
            suggested_dependencies: Vec::new(),
            kind,
            failure,
            notices: Vec::new(),
        }
    }

    fn failed(id: &str, kind: TurnFailureKind) -> FeatureTurn {
        turn(
            id,
            "failed",
            TurnKind::Discovery,
            Some(TurnFailure::new(kind, "raw")),
        )
    }

    #[test]
    fn every_kind_explains_itself_without_leaking_the_raw_error() {
        for kind in [
            TurnFailureKind::ReplyLanguage,
            TurnFailureKind::TooManyQuestions,
            TurnFailureKind::ReplyUnreadable,
            TurnFailureKind::SourceRejected,
            TurnFailureKind::DependencyUnresolved,
            TurnFailureKind::ContextTooLarge,
            TurnFailureKind::ProviderSetup,
            TurnFailureKind::ProviderTransient,
            TurnFailureKind::StaleFeature,
            TurnFailureKind::Interrupted,
            TurnFailureKind::Unclassified,
        ] {
            let explanation = TurnFailure::new(kind, "secret-raw-detail").explain();
            assert!(!explanation.summary.is_empty(), "{kind:?}");
            assert!(!explanation.cause.is_empty(), "{kind:?}");
            assert!(!explanation.remedy.is_empty(), "{kind:?}");
            let all = format!(
                "{} {} {}",
                explanation.summary, explanation.cause, explanation.remedy
            );
            assert!(!all.contains("secret-raw-detail"), "{kind:?}");
        }
        assert_eq!(
            TurnFailure::new(TurnFailureKind::Cancelled, "x")
                .explain()
                .summary,
            "You cancelled this response."
        );
    }

    #[test]
    fn only_reply_faults_let_the_developer_ask_the_agent() {
        let asks: Vec<_> = [
            TurnFailureKind::ReplyLanguage,
            TurnFailureKind::TooManyQuestions,
            TurnFailureKind::ReplyUnreadable,
            TurnFailureKind::SourceRejected,
            TurnFailureKind::DependencyUnresolved,
            TurnFailureKind::ContextTooLarge,
            TurnFailureKind::ProviderSetup,
            TurnFailureKind::ProviderTransient,
            TurnFailureKind::StaleFeature,
            TurnFailureKind::Interrupted,
            TurnFailureKind::Cancelled,
            TurnFailureKind::Unclassified,
        ]
        .into_iter()
        .filter(|kind| kind.ask_agent_allowed())
        .collect();
        assert_eq!(
            asks,
            [
                TurnFailureKind::ReplyLanguage,
                TurnFailureKind::TooManyQuestions,
                TurnFailureKind::ReplyUnreadable,
                TurnFailureKind::SourceRejected,
                TurnFailureKind::DependencyUnresolved,
            ]
        );
        assert!(!TurnFailureKind::ContextTooLarge.retry_allowed());
        assert!(TurnFailureKind::ProviderSetup.retry_allowed());
    }

    #[test]
    fn llm_and_cli_errors_are_typed_at_the_source() {
        let cases = [
            (LlmErrorKind::MissingApiKey, TurnFailureKind::ProviderSetup),
            (LlmErrorKind::Authentication, TurnFailureKind::ProviderSetup),
            (LlmErrorKind::Billing, TurnFailureKind::ProviderSetup),
            (LlmErrorKind::ModelNotFound, TurnFailureKind::ProviderSetup),
            (LlmErrorKind::RouteConflict, TurnFailureKind::ProviderSetup),
            (LlmErrorKind::Timeout, TurnFailureKind::ProviderTransient),
            (LlmErrorKind::Network, TurnFailureKind::ProviderTransient),
            (
                LlmErrorKind::RateLimited,
                TurnFailureKind::ProviderTransient,
            ),
            (
                LlmErrorKind::AllTargetsExhausted,
                TurnFailureKind::ProviderTransient,
            ),
            (LlmErrorKind::OutputSchema, TurnFailureKind::ReplyUnreadable),
            (LlmErrorKind::Cancelled, TurnFailureKind::Cancelled),
            (LlmErrorKind::InvalidRequest, TurnFailureKind::Unclassified),
            (LlmErrorKind::Sanitization, TurnFailureKind::Sanitization),
        ];
        for (llm, expected) in cases {
            let error = LlmError::new(llm, "boom").with_target("openai", "conn", "model");
            let failure = TurnFailure::from_llm(&error);
            assert_eq!(failure.kind, expected, "{llm:?}");
            assert_eq!(failure.subject.as_deref(), Some("openai"));
        }

        let cli = [
            (
                TerminalError::AuthRequired("Claude".into()),
                TurnFailureKind::ProviderSetup,
            ),
            (
                TerminalError::NotFound("claude".into()),
                TurnFailureKind::ProviderSetup,
            ),
            (
                TerminalError::VersionUnsupported("Codex".into()),
                TurnFailureKind::ProviderSetup,
            ),
            (TerminalError::Timeout, TurnFailureKind::ProviderTransient),
            (
                TerminalError::InactivityTimeout,
                TurnFailureKind::ProviderTransient,
            ),
            (
                TerminalError::Process("exit 1".into()),
                TurnFailureKind::ProviderTransient,
            ),
            (TerminalError::OutputLimit, TurnFailureKind::ReplyUnreadable),
            (TerminalError::Cancelled, TurnFailureKind::Cancelled),
        ];
        for (error, expected) in cli {
            assert_eq!(TurnFailure::from_terminal(&error).kind, expected, "{error}");
        }
        let auth = TurnFailure::from_terminal(&TerminalError::AuthRequired("Claude".into()));
        assert_eq!(auth.subject.as_deref(), Some("Claude"));
        assert!(auth.explain().summary.starts_with("Claude "));
    }

    #[test]
    fn an_unverifiable_cli_default_explains_how_to_choose_a_model_without_restarting() {
        for backend in crate::terminals::CliBackend::ALL {
            let failure = TurnFailure::from_terminal(&TerminalError::ModelUnverified {
                detail: format!("{} CLI default has no fixed model ID", backend.label()),
            });
            assert_eq!(failure.kind, TurnFailureKind::ProviderSetup);
            assert_eq!(
                failure.explain(),
                FailureExplanation {
                    summary: "The agent needs an explicit CLI model.".into(),
                    cause: "This agent is configured to use the CLI default, which does not identify a fixed model the app can verify. Your message is saved.".into(),
                    remedy: "Open Agents, choose and save an explicit model for the agent handling this reply, then select Try again. No app restart is needed.".into(),
                },
                "{backend:?}",
            );
            assert!(failure.retry_allowed());
            assert!(!failure.ask_agent_allowed());
        }
    }

    #[test]
    fn other_setup_and_model_identity_errors_keep_their_existing_explanations() {
        let generic = TurnFailure::new(TurnFailureKind::ProviderSetup, "raw").explain();
        for error in [
            TerminalError::NotFound("Codex".into()),
            TerminalError::ModelUnverified {
                detail: "completed CLI run has no persisted generation model".into(),
            },
            TerminalError::ModelMismatch {
                expected: "selected-model".into(),
                observed: "another-model".into(),
            },
            TerminalError::Validation("Codex CLI default has no fixed model ID".into()),
        ] {
            assert_eq!(
                TurnFailure::from_terminal(&error).explain(),
                generic,
                "{error}"
            );
        }
        let auth = TurnFailure::from_terminal(&TerminalError::AuthRequired("Claude".into()));
        assert_eq!(
            auth.explain(),
            FailureExplanation {
                summary: "Claude could not be used for this reply.".into(),
                cause: generic.cause,
                remedy: generic.remedy,
            }
        );
        let transient = TurnFailure::new(
            TurnFailureKind::ProviderTransient,
            "model_unverified: Codex CLI default has no fixed model ID",
        );
        assert_eq!(
            transient.explain().summary,
            "The model could not finish this reply."
        );
    }

    #[test]
    fn rows_written_before_typing_are_classified_from_their_text() {
        let cases = [
            (
                "Suggested dependency was not supplied by the host",
                TurnFailureKind::DependencyUnresolved,
            ),
            (
                "Proposed dependency was not supplied by the host",
                TurnFailureKind::DependencyUnresolved,
            ),
            (
                "Invalid discovery language or question count",
                TurnFailureKind::ReplyUnreadable,
            ),
            (
                "expected value at line 1 column 1",
                TurnFailureKind::ReplyUnreadable,
            ),
            (
                "execução CLI excedeu o tempo limite",
                TurnFailureKind::ProviderTransient,
            ),
            (
                "execução CLI falhou: Codex encerrou com código Some(1)",
                TurnFailureKind::ProviderTransient,
            ),
            (
                "execução CLI falhou: A verificação de versão excedeu 10 segundos.",
                TurnFailureKind::ProviderTransient,
            ),
            (
                "Application restarted during feature discovery",
                TurnFailureKind::Interrupted,
            ),
            (
                "Feature changed while the response was generated",
                TurnFailureKind::StaleFeature,
            ),
            ("Feature conversation cancelled", TurnFailureKind::Cancelled),
            (
                "Something nobody wrote a rule for",
                TurnFailureKind::Unclassified,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(TurnFailure::from_legacy(text).kind, expected, "{text}");
        }
        assert_eq!(
            TurnFailure::from_legacy("Something nobody wrote a rule for").technical,
            "Something nobody wrote a rule for"
        );
    }

    #[test]
    fn technical_detail_is_redacted_and_bounded() {
        let failure = TurnFailure::new(
            TurnFailureKind::Unclassified,
            format!(
                "Authorization: Bearer abcdefghijklmnop {}",
                "x".repeat(4000)
            ),
        );
        assert!(!failure.technical.contains("abcdefghijklmnop"));
        assert!(failure.technical.chars().count() <= TECHNICAL_DETAIL_LIMIT + 1);
    }

    #[test]
    fn only_the_newest_failed_turn_is_actionable() {
        let history = [
            failed("a", TurnFailureKind::ReplyLanguage),
            turn("b", "completed", TurnKind::Discovery, None),
        ];
        assert!(failure_actions(&history, TurnKind::Discovery).is_none());

        let history = [
            turn("a", "completed", TurnKind::Discovery, None),
            failed("b", TurnFailureKind::ReplyUnreadable),
        ];
        let (id, actions) = failure_actions(&history, TurnKind::Discovery).unwrap();
        assert_eq!(id, "b");
        assert_eq!(
            actions,
            FailureActions {
                retry: true,
                ask_agent: true
            }
        );

        let running = [
            failed("a", TurnFailureKind::ReplyUnreadable),
            turn("b", "running", TurnKind::Discovery, None),
        ];
        assert!(failure_actions(&running, TurnKind::Discovery).is_none());

        let cancelled = [turn(
            "a",
            "cancelled",
            TurnKind::Discovery,
            Some(TurnFailure::new(TurnFailureKind::Cancelled, "x")),
        )];
        assert!(failure_actions(&cancelled, TurnKind::Discovery).is_none());
    }

    #[test]
    fn asking_stops_after_two_identical_failures_and_never_applies_to_analyses() {
        let repeated = [
            failed("a", TurnFailureKind::ReplyLanguage),
            failed("b", TurnFailureKind::ReplyLanguage),
        ];
        let (_, actions) = failure_actions(&repeated, TurnKind::Discovery).unwrap();
        assert!(actions.retry);
        assert!(!actions.ask_agent);

        let different = [
            failed("a", TurnFailureKind::ReplyLanguage),
            failed("b", TurnFailureKind::ReplyUnreadable),
        ];
        assert!(
            failure_actions(&different, TurnKind::Discovery)
                .unwrap()
                .1
                .ask_agent
        );

        let analysis = [turn(
            "a",
            "failed",
            TurnKind::Dependencies,
            Some(TurnFailure::new(TurnFailureKind::ReplyUnreadable, "raw")),
        )];
        let (_, actions) = failure_actions(&analysis, TurnKind::Dependencies).unwrap();
        assert!(actions.retry);
        assert!(!actions.ask_agent);

        let setup = [failed("a", TurnFailureKind::ProviderSetup)];
        assert!(
            !failure_actions(&setup, TurnKind::Discovery)
                .unwrap()
                .1
                .ask_agent
        );
    }
}
