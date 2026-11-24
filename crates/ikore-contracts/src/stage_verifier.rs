//! Feature 25, M4: stage verifiers attached to default pipeline stages.
//!
//! Verifiers are strictly read-only agents that inspect a stage's accepted
//! result and return a `StageVerdictV1`. They reuse `StepAgent`,
//! `validate_step_agent` and `stage-verdict-v1` from `custom_pipeline`.

use serde::{Deserialize, Serialize};

pub use crate::custom_pipeline::{
    FindingSeverity, PipelineValidationError, StageVerdictV1, StepAgent, StepTransport, Verdict,
    VerdictFinding, parse_verdict, stage_verdict_json_schema, validate_step_agent,
    validate_verdict,
};
use crate::custom_pipeline::{MAX_INSTRUCTIONS_BYTES, MAX_LABEL_CHARS, STAGE_VALUES};

/// Maximum enabled or disabled verifiers attached to one default stage (P18).
pub const MAX_VERIFIERS_PER_STAGE: usize = 5;
/// Maximum verifiers across all six stages of a single project (P18).
pub const MAX_VERIFIERS_PER_PROJECT: usize = 15;
/// Highest automatic rejection limit before opening a failure checkpoint.
pub const MAX_REJECT_LIMIT: u8 = 5;
/// Default automatic rejection limit (`0` means advisory only).
pub const DEFAULT_REJECT_LIMIT: u8 = 2;
/// Maximum character count for optional `Continue anyway` justification (R6, P17).
pub const MAX_JUSTIFICATION_CHARS: usize = 500;
/// Byte budget for inlining accepted stage artifacts in `## Stage result` (P24).
pub const MAX_STAGE_RESULT_INLINE_BYTES: usize = 64 * 1024;

fn default_enabled() -> bool {
    true
}

fn default_reject_limit() -> u8 {
    DEFAULT_REJECT_LIMIT
}

/// Editable definition of a stage verifier attached to one default pipeline
/// stage (feature 25, M4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageVerifierDraft {
    pub stage: String,
    pub label: String,
    #[serde(default)]
    pub position: i64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_reject_limit")]
    pub reject_limit: u8,
    pub agent: StepAgent,
    pub instructions: String,
}

impl StageVerifierDraft {
    /// Advisory verifiers (`reject_limit == 0`) report findings without
    /// re-running the stage or opening `stage_failure` (P19).
    pub const fn is_advisory(&self) -> bool {
        self.reject_limit == 0
    }
}

/// Immutable snapshot of a stage verifier persisted in
/// `stage_verifier_runs.config_json` so editing a verifier never mutates
/// historical runs (feature 25, M4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageVerifierConfig {
    pub verifier_id: String,
    pub version: i64,
    pub stage: String,
    pub label: String,
    pub position: i64,
    pub reject_limit: u8,
    pub agent: StepAgent,
    pub instructions: String,
}

impl StageVerifierConfig {
    /// Captures an immutable config snapshot from a validated draft.
    pub fn from_draft(
        verifier_id: impl Into<String>,
        version: i64,
        draft: &StageVerifierDraft,
    ) -> Self {
        Self {
            verifier_id: verifier_id.into(),
            version,
            stage: draft.stage.clone(),
            label: draft.label.trim().to_owned(),
            position: draft.position,
            reject_limit: draft.reject_limit,
            agent: draft.agent.clone(),
            instructions: draft.instructions.clone(),
        }
    }

    /// Advisory verifiers (`reject_limit == 0`) report findings without
    /// re-running the stage or opening `stage_failure` (P19).
    pub const fn is_advisory(&self) -> bool {
        self.reject_limit == 0
    }
}

/// Parses a `StageVerifierDraft` from JSON, rejecting unknown fields.
pub fn parse_verifier_draft(json: &str) -> Result<StageVerifierDraft, PipelineValidationError> {
    serde_json::from_str(json).map_err(|error| {
        let detail = error.to_string().replace('`', "\"");
        PipelineValidationError::new(
            "verifier_invalid",
            None,
            format!("The verifier definition is not valid: {detail}"),
        )
    })
}

/// Parses a persisted `StageVerifierConfig` snapshot from JSON.
pub fn parse_verifier_config(json: &str) -> Result<StageVerifierConfig, PipelineValidationError> {
    serde_json::from_str(json).map_err(|error| {
        let detail = error.to_string().replace('`', "\"");
        PipelineValidationError::new(
            "verifier_config_invalid",
            None,
            format!("The verifier snapshot is not valid: {detail}"),
        )
    })
}

/// Validates a `StageVerifierDraft` against all M4 domain, transport, and
/// security constraints (E1, E2, P9, P18).
pub fn validate_verifier_draft(draft: &StageVerifierDraft) -> Vec<PipelineValidationError> {
    let mut errors = Vec::new();

    if draft.stage.trim().is_empty() {
        errors.push(PipelineValidationError::new(
            "stage_required",
            None,
            "Choose the stage this verifier checks.",
        ));
    } else if !STAGE_VALUES.contains(&draft.stage.as_str()) {
        errors.push(PipelineValidationError::new(
            "stage_unknown",
            None,
            format!("\"{}\" is not a stage of the Ikore pipeline.", draft.stage),
        ));
    }

    let label = draft.label.trim();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        errors.push(PipelineValidationError::new(
            "label_invalid",
            None,
            format!("A verifier's label has 1 to {MAX_LABEL_CHARS} characters."),
        ));
    }

    if draft.position < 0 {
        errors.push(PipelineValidationError::new(
            "position_invalid",
            None,
            "Position must be zero or greater.",
        ));
    }

    if draft.reject_limit > MAX_REJECT_LIMIT {
        errors.push(PipelineValidationError::new(
            "reject_limit_invalid",
            None,
            format!("Rejection limit must be between 0 and {MAX_REJECT_LIMIT}."),
        ));
    }

    if draft.instructions.trim().is_empty() {
        errors.push(PipelineValidationError::new(
            "instructions_missing",
            None,
            "Tell the verifier what to check.",
        ));
    }
    if draft.instructions.len() > MAX_INSTRUCTIONS_BYTES {
        errors.push(PipelineValidationError::new(
            "instructions_too_long",
            None,
            "Instructions have at most 16 KiB.",
        ));
    }

    validate_step_agent(&draft.agent, None, "verifier", &mut errors);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_pipeline::ALLOWED_BACKENDS;

    fn sample_cli_draft() -> StageVerifierDraft {
        StageVerifierDraft {
            stage: "Code".into(),
            label: "Security audit".into(),
            position: 0,
            enabled: true,
            reject_limit: DEFAULT_REJECT_LIMIT,
            agent: StepAgent {
                transport: StepTransport::Cli,
                backend: Some("codex".into()),
                api_route_id: None,
                model: Some("gpt-5".into()),
                model_profile: None,
                thinking_level: "default".into(),
            },
            instructions: "Check for leaked credentials and unsafe SQL.".into(),
        }
    }

    fn codes(errors: &[PipelineValidationError]) -> Vec<&str> {
        errors.iter().map(|error| error.code.as_str()).collect()
    }

    #[test]
    fn valid_drafts_pass_across_all_stages_backends_and_transports() {
        for stage in STAGE_VALUES {
            for backend in ALLOWED_BACKENDS {
                let mut draft = sample_cli_draft();
                draft.stage = stage.into();
                draft.agent.backend = Some(backend.into());
                assert!(
                    validate_verifier_draft(&draft).is_empty(),
                    "expected valid draft for stage={stage}, backend={backend}"
                );
            }
        }

        for limit in 0..=MAX_REJECT_LIMIT {
            let mut draft = sample_cli_draft();
            draft.reject_limit = limit;
            draft.enabled = limit % 2 == 0;
            assert!(validate_verifier_draft(&draft).is_empty());
            assert_eq!(draft.is_advisory(), limit == 0);
        }

        let mut api_draft = sample_cli_draft();
        api_draft.agent = StepAgent {
            transport: StepTransport::Api,
            backend: None,
            api_route_id: Some("route-1".into()),
            model: None,
            model_profile: None,
            thinking_level: "medium".into(),
        };
        assert!(validate_verifier_draft(&api_draft).is_empty());

        let minimal_json = r#"{
            "stage": "SPECIFICATION",
            "label": "Spec checker",
            "agent": {
                "transport": "cli",
                "backend": "claude",
                "model": "claude-sonnet-4-5"
            },
            "instructions": "Verify acceptance criteria."
        }"#;
        let parsed = parse_verifier_draft(minimal_json).unwrap();
        assert_eq!(parsed.position, 0);
        assert!(parsed.enabled);
        assert_eq!(parsed.reject_limit, DEFAULT_REJECT_LIMIT);
        assert!(validate_verifier_draft(&parsed).is_empty());

        let round_tripped: StageVerifierDraft =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(round_tripped, parsed);
    }

    #[test]
    fn unknown_fields_are_rejected_for_draft_and_config() {
        let draft_json = r#"{
            "stage": "Code",
            "label": "Lint",
            "agent": { "transport": "cli", "backend": "codex", "model": "gpt-5" },
            "instructions": "Check lint.",
            "unexpected": true
        }"#;
        let err = parse_verifier_draft(draft_json).unwrap_err();
        assert_eq!(err.code, "verifier_invalid");
        assert!(!err.message.contains('`'));

        let config_json = r#"{
            "verifierId": "v1",
            "version": 1,
            "stage": "Code",
            "label": "Lint",
            "position": 0,
            "rejectLimit": 2,
            "agent": { "transport": "cli", "backend": "codex", "model": "gpt-5" },
            "instructions": "Check lint.",
            "extraField": 123
        }"#;
        let err = parse_verifier_config(config_json).unwrap_err();
        assert_eq!(err.code, "verifier_config_invalid");
        assert!(!err.message.contains('`'));
    }

    #[test]
    fn validation_enforces_all_stage_label_bounds_agent_rules_with_no_backticks() {
        let mut all_errors = Vec::new();

        // Stage rules (canonical DB casing only)
        for (stage, expected) in [
            ("", "stage_required"),
            ("   ", "stage_required"),
            ("Deploy", "stage_unknown"),
            ("Specification", "stage_unknown"),
            (" Code ", "stage_unknown"),
        ] {
            let mut draft = sample_cli_draft();
            draft.stage = stage.into();
            let errors = validate_verifier_draft(&draft);
            assert_eq!(codes(&errors), [expected], "stage={stage:?}");
            all_errors.extend(errors);
        }

        // Label rules (1..=60 trimmed Unicode chars)
        let mut sixty_unicode = sample_cli_draft();
        sixty_unicode.label = "é".repeat(MAX_LABEL_CHARS);
        assert!(validate_verifier_draft(&sixty_unicode).is_empty());

        for label in [
            "".to_owned(),
            "   ".to_owned(),
            "é".repeat(MAX_LABEL_CHARS + 1),
        ] {
            let mut draft = sample_cli_draft();
            draft.label = label;
            let errors = validate_verifier_draft(&draft);
            assert_eq!(codes(&errors), ["label_invalid"]);
            all_errors.extend(errors);
        }

        // Position & reject_limit bounds
        let mut neg_pos = sample_cli_draft();
        neg_pos.position = -1;
        let errors = validate_verifier_draft(&neg_pos);
        assert_eq!(codes(&errors), ["position_invalid"]);
        all_errors.extend(errors);

        let mut bad_limit = sample_cli_draft();
        bad_limit.reject_limit = MAX_REJECT_LIMIT + 1;
        let errors = validate_verifier_draft(&bad_limit);
        assert_eq!(codes(&errors), ["reject_limit_invalid"]);
        all_errors.extend(errors);

        // Instructions bounds
        let mut blank_inst = sample_cli_draft();
        blank_inst.instructions = "   \n\t ".into();
        let errors = validate_verifier_draft(&blank_inst);
        assert_eq!(codes(&errors), ["instructions_missing"]);
        all_errors.extend(errors);

        let mut long_inst = sample_cli_draft();
        long_inst.instructions = "x".repeat(MAX_INSTRUCTIONS_BYTES + 1);
        let errors = validate_verifier_draft(&long_inst);
        assert_eq!(codes(&errors), ["instructions_too_long"]);
        all_errors.extend(errors);

        // CLI agent rules
        for (backend, expected) in [
            (None, "backend_required"),
            (Some(""), "backend_required"),
            (Some("   "), "backend_required"),
            (Some("custom"), "backend_not_allowed"),
            (Some("Custom"), "backend_not_allowed"),
            (Some(" CUSTOM "), "backend_not_allowed"),
            (Some("aider"), "backend_unknown"),
            (Some(" codex "), "backend_unknown"),
        ] {
            let mut draft = sample_cli_draft();
            draft.agent.backend = backend.map(str::to_owned);
            let errors = validate_verifier_draft(&draft);
            assert_eq!(codes(&errors), [expected], "backend={backend:?}");
            all_errors.extend(errors);
        }

        for model in [None, Some(""), Some("   ")] {
            let mut draft = sample_cli_draft();
            draft.agent.model = model.map(str::to_owned);
            let errors = validate_verifier_draft(&draft);
            assert_eq!(codes(&errors), ["model_required"]);
            all_errors.extend(errors);
        }

        let mut bad_effort = sample_cli_draft();
        bad_effort.agent.thinking_level = "ultra".into();
        let errors = validate_verifier_draft(&bad_effort);
        assert_eq!(codes(&errors), ["thinking_level_invalid"]);
        all_errors.extend(errors);

        // API agent rules
        for route in [None, Some(""), Some("   ")] {
            let mut draft = sample_cli_draft();
            draft.agent.transport = StepTransport::Api;
            draft.agent.backend = None;
            draft.agent.model = None;
            draft.agent.api_route_id = route.map(str::to_owned);
            let errors = validate_verifier_draft(&draft);
            assert_eq!(codes(&errors), ["api_route_required"]);
            all_errors.extend(errors);
        }

        assert!(!all_errors.is_empty());
        for error in &all_errors {
            assert!(
                !error.message.contains('`'),
                "validation message must not contain literal backticks: {:?}",
                error
            );
        }
    }

    #[test]
    fn config_snapshot_from_draft_trims_label_and_round_trips() {
        let mut draft = sample_cli_draft();
        draft.label = "  Security audit  ".into();
        draft.reject_limit = 0;

        let config = StageVerifierConfig::from_draft("v-123", 4, &draft);
        assert_eq!(config.verifier_id, "v-123");
        assert_eq!(config.version, 4);
        assert_eq!(config.stage, "Code");
        assert_eq!(config.label, "Security audit");
        assert_eq!(config.position, 0);
        assert_eq!(config.reject_limit, 0);
        assert!(config.is_advisory());

        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(parse_verifier_config(&json).unwrap(), config);
    }

    #[test]
    fn exported_limits_match_approved_m4_constants() {
        assert_eq!(MAX_VERIFIERS_PER_STAGE, 5);
        assert_eq!(MAX_VERIFIERS_PER_PROJECT, 15);
        assert_eq!(MAX_REJECT_LIMIT, 5);
        assert_eq!(DEFAULT_REJECT_LIMIT, 2);
        assert_eq!(MAX_JUSTIFICATION_CHARS, 500);
        assert_eq!(MAX_STAGE_RESULT_INLINE_BYTES, 64 * 1024);
    }
}
