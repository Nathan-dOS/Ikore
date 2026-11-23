//! Feature 25: the developer's own pipelines around the Ikore pipeline
//! (`custom-pipeline-v1`) and the verdict of a step or of a verifier
//! (`stage-verdict-v1`). Their agents only read: they return text or a
//! verdict, and the host owns state, identifiers, publication and decisions.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CUSTOM_PIPELINE_SCHEMA_VERSION: u32 = 1;
pub const MAX_STEPS: usize = 8;
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;
pub const MAX_GATE_PROMPT_BYTES: usize = 2 * 1024;
pub const MAX_LABEL_CHARS: usize = 60;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_DESCRIPTION_CHARS: usize = 500;
/// The Ikore pipeline's box is called this; no custom pipeline may be.
pub const RESERVED_PIPELINE_NAME: &str = "Pipeline";
/// The official adapters a step or a verifier may run (E1). `custom` is
/// refused: the host cannot keep an arbitrary executable read-only.
pub const ALLOWED_BACKENDS: [&str; 5] = ["codex", "claude", "gemini", "opencode", "antigravity"];
pub const THINKING_LEVELS: [&str; 4] = ["default", "low", "medium", "high"];
/// The values of `PulsationStage::as_db_value()`.
pub const STAGE_VALUES: [&str; 6] = [
    "Intake",
    "SPECIFICATION",
    "Contract",
    "Code",
    "Test",
    "Integrator",
];

/// A pipeline of the developer's: what starts it and its steps, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomPipelineDefinitionV1 {
    pub schema_version: u32,
    pub trigger: PipelineTrigger,
    pub steps: Vec<PipelineStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PipelineTrigger {
    /// `Run` on the pipeline's box.
    Manual,
    /// An event of the Ikore pipeline (feature 25, M3).
    DefaultEvent {
        event: DefaultEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultEvent {
    /// The pulsation left `stage` forward.
    StagePassed,
    /// The Integrator was approved and the pulsation is done.
    PulsationDone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PipelineStep {
    Agent(AgentStep),
    Gate(GateStep),
    HostTool(HostToolStep),
}

impl PipelineStep {
    pub fn id(&self) -> &str {
        match self {
            Self::Agent(step) => &step.id,
            Self::Gate(step) => &step.id,
            Self::HostTool(step) => &step.id,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Agent(step) => &step.label,
            Self::Gate(step) => &step.label,
            Self::HostTool(step) => &step.label,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Agent(_) => "agent",
            Self::Gate(_) => "gate",
            Self::HostTool(_) => "host_tool",
        }
    }
}

/// An agent of the pipeline: a read-only run that returns text or a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentStep {
    pub id: String,
    pub label: String,
    pub agent: StepAgent,
    pub instructions: String,
    pub output: StepOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepAgent {
    pub transport: StepTransport,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_profile: Option<String>,
    #[serde(default = "default_thinking_level")]
    pub thinking_level: String,
    #[serde(default)]
    pub api_route_id: Option<String>,
}

fn default_thinking_level() -> String {
    "default".to_owned()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepTransport {
    Cli,
    Api,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepOutput {
    /// Markdown.
    Text,
    /// `stage-verdict-v1`: a `reject` stops the run.
    Verdict,
}

/// A human approval in the middle of the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateStep {
    pub id: String,
    pub label: String,
    pub prompt: String,
}

/// A git operation of the host (feature 25, M5; feature 08).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostToolStep {
    pub id: String,
    pub label: String,
    pub tool: HostTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostTool {
    #[serde(rename = "git.commit")]
    GitCommit,
    #[serde(rename = "git.push")]
    GitPush,
    #[serde(rename = "git.pull")]
    GitPull,
}

impl HostTool {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitCommit => "git.commit",
            Self::GitPush => "git.push",
            Self::GitPull => "git.pull",
        }
    }
}

/// A problem with a definition, a name or a verdict: a stable code, the step
/// it concerns, and a message in English for the form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineValidationError {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub message: String,
}

impl PipelineValidationError {
    pub fn new(code: &str, step_id: Option<&str>, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            step_id: step_id.map(str::to_owned),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PipelineValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.step_id {
            Some(step) => write!(formatter, "{} ({step}): {}", self.code, self.message),
            None => write!(formatter, "{}: {}", self.code, self.message),
        }
    }
}

/// Reads a stored or submitted definition. Unknown fields are refused.
pub fn parse_definition(json: &str) -> Result<CustomPipelineDefinitionV1, PipelineValidationError> {
    serde_json::from_str(json).map_err(|error| {
        PipelineValidationError::new(
            "definition_invalid",
            None,
            format!("The pipeline definition is not valid: {error}"),
        )
    })
}

fn valid_step_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Every problem of a definition at once. `host_tools` says whether the
/// git tool of the host is available to pipelines (feature 25, M5).
pub fn validate_definition(
    definition: &CustomPipelineDefinitionV1,
    host_tools: bool,
) -> Vec<PipelineValidationError> {
    let mut errors = Vec::new();
    if definition.schema_version != CUSTOM_PIPELINE_SCHEMA_VERSION {
        errors.push(PipelineValidationError::new(
            "schema_version_unsupported",
            None,
            format!("Only schema version {CUSTOM_PIPELINE_SCHEMA_VERSION} is supported."),
        ));
    }
    match &definition.trigger {
        PipelineTrigger::Manual => {}
        PipelineTrigger::DefaultEvent { event, stage } => match (event, stage.as_deref()) {
            (DefaultEvent::StagePassed, None) => errors.push(PipelineValidationError::new(
                "stage_required",
                None,
                "Choose the stage whose passing starts the pipeline.",
            )),
            (DefaultEvent::StagePassed, Some(stage)) if !STAGE_VALUES.contains(&stage) => {
                errors.push(PipelineValidationError::new(
                    "stage_unknown",
                    None,
                    format!("`{stage}` is not a stage of the Ikore pipeline."),
                ));
            }
            (DefaultEvent::StagePassed, Some("Integrator")) => {
                errors.push(PipelineValidationError::new(
                    "stage_unknown",
                    None,
                    "An approved Integrator is `Pulsation done`, not a passed stage.",
                ));
            }
            (DefaultEvent::PulsationDone, Some(_)) => errors.push(PipelineValidationError::new(
                "stage_not_allowed",
                None,
                "`Pulsation done` takes no stage.",
            )),
            _ => {}
        },
    }
    if definition.steps.is_empty() {
        errors.push(PipelineValidationError::new(
            "steps_missing",
            None,
            "Add at least one step.",
        ));
    }
    if definition.steps.len() > MAX_STEPS {
        errors.push(PipelineValidationError::new(
            "too_many_steps",
            None,
            format!("A pipeline has at most {MAX_STEPS} steps."),
        ));
    }
    let mut seen = HashSet::new();
    for step in &definition.steps {
        let id = step.id();
        if !valid_step_id(id) {
            errors.push(PipelineValidationError::new(
                "step_id_invalid",
                Some(id),
                "A step id has 1 to 40 lowercase letters, digits or hyphens.",
            ));
        } else if !seen.insert(id.to_owned()) {
            errors.push(PipelineValidationError::new(
                "step_id_duplicate",
                Some(id),
                "Two steps share this id.",
            ));
        }
        let label = step.label().trim();
        if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            errors.push(PipelineValidationError::new(
                "label_invalid",
                Some(id),
                format!("A step's label has 1 to {MAX_LABEL_CHARS} characters."),
            ));
        }
        match step {
            PipelineStep::Agent(agent) => validate_agent(agent, &mut errors),
            PipelineStep::Gate(gate) => {
                if gate.prompt.len() > MAX_GATE_PROMPT_BYTES {
                    errors.push(PipelineValidationError::new(
                        "gate_prompt_too_long",
                        Some(id),
                        "A gate's text has at most 2 KiB.",
                    ));
                }
            }
            PipelineStep::HostTool(_) if !host_tools => {
                errors.push(PipelineValidationError::new(
                    "host_tool_unavailable",
                    Some(id),
                    "Available after the commit tool ships.",
                ));
            }
            PipelineStep::HostTool(_) => {}
        }
    }
    errors
}

fn validate_agent(step: &AgentStep, errors: &mut Vec<PipelineValidationError>) {
    let id = Some(step.id.as_str());
    if step.instructions.trim().is_empty() {
        errors.push(PipelineValidationError::new(
            "instructions_missing",
            id,
            "Tell the agent what to do.",
        ));
    }
    if step.instructions.len() > MAX_INSTRUCTIONS_BYTES {
        errors.push(PipelineValidationError::new(
            "instructions_too_long",
            id,
            "Instructions have at most 16 KiB.",
        ));
    }
    if !THINKING_LEVELS.contains(&step.agent.thinking_level.as_str()) {
        errors.push(PipelineValidationError::new(
            "thinking_level_invalid",
            id,
            "Effort is default, low, medium or high.",
        ));
    }
    match step.agent.transport {
        StepTransport::Cli => match step.agent.backend.as_deref() {
            None => errors.push(PipelineValidationError::new(
                "backend_required",
                id,
                "Choose the CLI that runs this step.",
            )),
            Some("custom") => errors.push(PipelineValidationError::new(
                "backend_not_allowed",
                id,
                "Steps require an official read-only CLI adapter or API transport.",
            )),
            Some(backend) if !ALLOWED_BACKENDS.contains(&backend) => {
                errors.push(PipelineValidationError::new(
                    "backend_unknown",
                    id,
                    format!("`{backend}` is not a known CLI."),
                ));
            }
            Some(_) => {
                // Automatic runs bind a model the host verifies afterwards; a
                // CLI's own default names none (`require_verifiable_cli_selection`).
                if step
                    .agent
                    .model
                    .as_deref()
                    .is_none_or(|model| model.trim().is_empty())
                {
                    errors.push(PipelineValidationError::new(
                        "model_required",
                        id,
                        "Choose a model: automatic runs need a fixed model the app can verify.",
                    ));
                }
            }
        },
        StepTransport::Api => {
            if step
                .agent
                .api_route_id
                .as_deref()
                .is_none_or(|route| route.trim().is_empty())
            {
                errors.push(PipelineValidationError::new(
                    "api_route_required",
                    id,
                    "Choose the API route that runs this step.",
                ));
            }
        }
    }
}

/// A pipeline's name: 1 to 60 characters, and never `Pipeline`.
pub fn validate_name(name: &str) -> Option<PipelineValidationError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Some(PipelineValidationError::new(
            "name_invalid",
            None,
            format!("A pipeline's name has 1 to {MAX_NAME_CHARS} characters."),
        ));
    }
    if name.eq_ignore_ascii_case(RESERVED_PIPELINE_NAME) {
        return Some(PipelineValidationError::new(
            "name_reserved",
            None,
            "`Pipeline` is the Ikore pipeline's name.",
        ));
    }
    None
}

/// The verdict of a `verdict` step or of a verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StageVerdictV1 {
    pub verdict: Verdict,
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<VerdictFinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerdictFinding {
    pub id: String,
    pub severity: FindingSeverity,
    pub message: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub suggestion: Option<String>,
    /// Set by the host: the finding named a path the workspace lacks, and the
    /// path was dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub file_not_found: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Blocking,
    Warning,
    Note,
}

pub const MAX_SUMMARY_CHARS: usize = 1_200;
pub const MAX_FINDINGS: usize = 20;
pub const MAX_FINDING_CHARS: usize = 600;

/// The JSON object in a model's answer: the whole answer, or the first JSON
/// object inside a fenced block.
fn json_body(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return trimmed;
    }
    let after_fence = trimmed
        .find("```")
        .map(|start| &trimmed[start + 3..])
        .map(|rest| rest.trim_start_matches("json").trim_start());
    match after_fence {
        Some(body) => body.split("```").next().unwrap_or(body).trim(),
        None => trimmed,
    }
}

/// Reads and checks a verdict. The error names what is wrong, to show the
/// model once in a correction.
pub fn parse_verdict(raw: &str) -> Result<StageVerdictV1, String> {
    let verdict: StageVerdictV1 = serde_json::from_str(json_body(raw))
        .map_err(|error| format!("The answer is not a valid verdict: {error}"))?;
    let problems = validate_verdict(&verdict);
    if problems.is_empty() {
        Ok(verdict)
    } else {
        Err(problems.join(" "))
    }
}

pub fn validate_verdict(verdict: &StageVerdictV1) -> Vec<String> {
    let mut problems = Vec::new();
    let blocking = verdict
        .findings
        .iter()
        .any(|finding| finding.severity == FindingSeverity::Blocking);
    match verdict.verdict {
        Verdict::Reject if !blocking => {
            problems.push("A `reject` needs at least one blocking finding.".to_owned());
        }
        Verdict::Pass if blocking => {
            problems.push("A `pass` cannot have a blocking finding.".to_owned());
        }
        _ => {}
    }
    if verdict.summary.chars().count() > MAX_SUMMARY_CHARS {
        problems.push(format!(
            "The summary has at most {MAX_SUMMARY_CHARS} characters."
        ));
    }
    if verdict.findings.len() > MAX_FINDINGS {
        problems.push(format!("A verdict has at most {MAX_FINDINGS} findings."));
    }
    let mut ids = HashSet::new();
    for finding in &verdict.findings {
        if !ids.insert(finding.id.as_str()) {
            problems.push(format!("Two findings share the id `{}`.", finding.id));
        }
        if finding.message.chars().count() > MAX_FINDING_CHARS
            || finding
                .suggestion
                .as_deref()
                .is_some_and(|text| text.chars().count() > MAX_FINDING_CHARS)
        {
            problems.push(format!(
                "Finding `{}`: message and suggestion have at most {MAX_FINDING_CHARS} characters.",
                finding.id
            ));
        }
    }
    problems
}

/// The JSON Schema a CLI with structured output is held to.
pub fn stage_verdict_json_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "summary", "findings"],
        "properties": {
            "verdict": { "type": "string", "enum": ["pass", "reject"] },
            "summary": { "type": "string", "maxLength": MAX_SUMMARY_CHARS },
            "findings": {
                "type": "array",
                "maxItems": MAX_FINDINGS,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "severity", "message", "file", "line", "suggestion"],
                    "properties": {
                        "id": { "type": "string" },
                        "severity": { "type": "string", "enum": ["blocking", "warning", "note"] },
                        "message": { "type": "string", "maxLength": MAX_FINDING_CHARS },
                        "file": { "type": ["string", "null"] },
                        "line": { "type": ["integer", "null"], "minimum": 1 },
                        "suggestion": { "type": ["string", "null"], "maxLength": MAX_FINDING_CHARS }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The feature's example definition.
    const EXAMPLE: &str = r#"{
      "schemaVersion": 1,
      "trigger": { "kind": "default_event", "event": "stage_passed", "stage": "Code" },
      "steps": [
        {
          "id": "conventions",
          "kind": "agent",
          "label": "Company conventions",
          "agent": {
            "transport": "cli",
            "backend": "codex",
            "model": "gpt-5.6-luna",
            "modelProfile": null,
            "thinkingLevel": "default",
            "apiRouteId": null
          },
          "instructions": "Check the changed files against our naming and logging rules.",
          "output": "verdict"
        },
        {
          "id": "approve",
          "kind": "gate",
          "label": "Approve",
          "prompt": "Read the findings and approve or reject."
        }
      ]
    }"#;

    fn example() -> CustomPipelineDefinitionV1 {
        parse_definition(EXAMPLE).unwrap()
    }

    fn codes(errors: &[PipelineValidationError]) -> Vec<&str> {
        errors.iter().map(|error| error.code.as_str()).collect()
    }

    #[test]
    fn the_feature_example_is_valid_and_round_trips() {
        let definition = example();
        assert!(validate_definition(&definition, false).is_empty());
        let json = serde_json::to_string(&definition).unwrap();
        assert_eq!(parse_definition(&json).unwrap(), definition);
        assert_eq!(definition.steps[1].kind(), "gate");
    }

    #[test]
    fn unknown_fields_are_refused_at_every_level() {
        let top = EXAMPLE.replacen(
            "\"schemaVersion\": 1,",
            "\"schemaVersion\": 1, \"extra\": 1,",
            1,
        );
        assert_eq!(
            parse_definition(&top).unwrap_err().code,
            "definition_invalid"
        );
        let step = EXAMPLE.replacen(
            "\"output\": \"verdict\"",
            "\"output\": \"verdict\", \"x\": 1",
            1,
        );
        assert_eq!(
            parse_definition(&step).unwrap_err().code,
            "definition_invalid"
        );
        let agent = EXAMPLE.replacen("\"apiRouteId\": null", "\"apiRouteId\": null, \"y\": 2", 1);
        assert_eq!(
            parse_definition(&agent).unwrap_err().code,
            "definition_invalid"
        );
        let trigger = EXAMPLE.replacen("\"stage\": \"Code\"", "\"stage\": \"Code\", \"z\": 3", 1);
        assert_eq!(
            parse_definition(&trigger).unwrap_err().code,
            "definition_invalid"
        );
    }

    #[test]
    fn limits_ids_and_labels_are_checked() {
        let mut definition = example();
        let PipelineStep::Agent(agent) = definition.steps[0].clone() else {
            unreachable!();
        };
        definition.steps = (0..9)
            .map(|index| {
                let mut step = agent.clone();
                step.id = format!("step-{index}");
                PipelineStep::Agent(step)
            })
            .collect();
        assert!(codes(&validate_definition(&definition, false)).contains(&"too_many_steps"));
        definition.steps.truncate(2);
        if let PipelineStep::Agent(step) = &mut definition.steps[1] {
            step.id = "step-0".into();
            step.label = " ".into();
            step.instructions = "x".repeat(MAX_INSTRUCTIONS_BYTES + 1);
        }
        let found = validate_definition(&definition, false);
        assert!(codes(&found).contains(&"step_id_duplicate"));
        assert!(codes(&found).contains(&"label_invalid"));
        assert!(codes(&found).contains(&"instructions_too_long"));
        if let PipelineStep::Agent(step) = &mut definition.steps[1] {
            step.id = "Bad Id".into();
        }
        assert!(codes(&validate_definition(&definition, false)).contains(&"step_id_invalid"));
        definition.steps.clear();
        assert_eq!(
            codes(&validate_definition(&definition, false)),
            ["steps_missing"]
        );
    }

    #[test]
    fn a_cli_step_names_a_fixed_model() {
        let mut definition = example();
        if let PipelineStep::Agent(step) = &mut definition.steps[0] {
            step.agent.model = None;
        }
        assert_eq!(
            codes(&validate_definition(&definition, false)),
            ["model_required"]
        );
        if let PipelineStep::Agent(step) = &mut definition.steps[0] {
            step.agent.model = Some("  ".into());
            step.agent.transport = StepTransport::Api;
            step.agent.api_route_id = Some("route-1".into());
        }
        // An API step's model is its route's.
        assert!(validate_definition(&definition, false).is_empty());
    }

    #[test]
    fn custom_backend_and_host_tools_are_refused() {
        let mut definition = example();
        if let PipelineStep::Agent(step) = &mut definition.steps[0] {
            step.agent.backend = Some("custom".into());
        }
        definition.steps.push(PipelineStep::HostTool(HostToolStep {
            id: "commit".into(),
            label: "Commit".into(),
            tool: HostTool::GitCommit,
        }));
        let errors = validate_definition(&definition, false);
        let found = codes(&errors);
        assert!(found.contains(&"backend_not_allowed"));
        assert!(found.contains(&"host_tool_unavailable"));
        assert!(!codes(&validate_definition(&definition, true)).contains(&"host_tool_unavailable"));
    }

    #[test]
    fn triggers_name_a_real_stage() {
        let mut definition = example();
        definition.trigger = PipelineTrigger::DefaultEvent {
            event: DefaultEvent::StagePassed,
            stage: Some("Deploy".into()),
        };
        assert_eq!(
            codes(&validate_definition(&definition, false)),
            ["stage_unknown"]
        );
        definition.trigger = PipelineTrigger::DefaultEvent {
            event: DefaultEvent::PulsationDone,
            stage: Some("Code".into()),
        };
        assert_eq!(
            codes(&validate_definition(&definition, false)),
            ["stage_not_allowed"]
        );
        definition.trigger = PipelineTrigger::Manual;
        assert!(validate_definition(&definition, false).is_empty());
        let manual = serde_json::to_value(&definition.trigger).unwrap();
        assert_eq!(manual, json!({"kind": "manual"}));
    }

    #[test]
    fn names_are_bounded_and_pipeline_is_reserved() {
        assert_eq!(validate_name("pipeline").unwrap().code, "name_reserved");
        assert_eq!(validate_name("  ").unwrap().code, "name_invalid");
        assert_eq!(validate_name(&"n".repeat(61)).unwrap().code, "name_invalid");
        assert!(validate_name("Release notes").is_none());
    }

    #[test]
    fn verdicts_hold_their_rules() {
        let reject = r#"{"verdict":"reject","summary":"The handler logs the raw token.","findings":[{"id":"f1","severity":"blocking","message":"Token is written to the log on line 42.","file":"src/auth/handler.rs","line":42,"suggestion":"Log the token id instead."}]}"#;
        let verdict = parse_verdict(reject).unwrap();
        assert_eq!(verdict.verdict, Verdict::Reject);
        let fenced = format!("Here it is:\n```json\n{reject}\n```");
        assert_eq!(parse_verdict(&fenced).unwrap(), verdict);
        let no_blocking = reject.replace("blocking", "warning");
        assert!(
            parse_verdict(&no_blocking)
                .unwrap_err()
                .contains("blocking")
        );
        let pass_blocking = reject.replace("\"reject\"", "\"pass\"");
        assert!(parse_verdict(&pass_blocking).is_err());
        assert!(parse_verdict("not json").is_err());
        let schema = stage_verdict_json_schema();
        assert_eq!(
            schema["properties"]["verdict"]["enum"],
            json!(["pass", "reject"])
        );
    }
}
