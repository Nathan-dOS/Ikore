//! Feature 25: the developer's own pipelines around the Ikore pipeline
//! (`custom-pipeline-v1`) and the verdict of a step or of a verifier
//! (`stage-verdict-v1`). Their agents only read: they return text or a
//! verdict, and the host owns state, identifiers, publication and decisions.

use std::collections::{BTreeMap, HashSet};

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

/// The problems of a trigger: a passed stage names a stage of the Ikore
/// pipeline other than the Integrator, and `Pulsation done` takes none.
pub fn validate_trigger(trigger: &PipelineTrigger) -> Vec<PipelineValidationError> {
    let mut errors = Vec::new();
    match trigger {
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
                    format!("\"{stage}\" is not a stage of the Ikore pipeline."),
                ));
            }
            (DefaultEvent::StagePassed, Some("Integrator")) => {
                errors.push(PipelineValidationError::new(
                    "stage_unknown",
                    None,
                    "An approved Integrator is \"Pulsation done\", not a passed stage.",
                ));
            }
            (DefaultEvent::PulsationDone, Some(_)) => errors.push(PipelineValidationError::new(
                "stage_not_allowed",
                None,
                "\"Pulsation done\" takes no stage.",
            )),
            _ => {}
        },
    }
    errors
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
    errors.extend(validate_trigger(&definition.trigger));
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

/// Validates a `StepAgent`'s transport, backend, verifiable model or API
/// route, and thinking level (E1, E2, P9). Shared by custom pipeline agent
/// steps and stage verifiers.
pub fn validate_step_agent(
    agent: &StepAgent,
    step_id: Option<&str>,
    subject: &str,
    errors: &mut Vec<PipelineValidationError>,
) {
    if !THINKING_LEVELS.contains(&agent.thinking_level.as_str()) {
        errors.push(PipelineValidationError::new(
            "thinking_level_invalid",
            step_id,
            "Effort is default, low, medium or high.",
        ));
    }
    match agent.transport {
        StepTransport::Cli => match agent.backend.as_deref() {
            None => errors.push(PipelineValidationError::new(
                "backend_required",
                step_id,
                format!("Choose the CLI that runs this {subject}."),
            )),
            Some(backend) if backend.trim().is_empty() => {
                errors.push(PipelineValidationError::new(
                    "backend_required",
                    step_id,
                    format!("Choose the CLI that runs this {subject}."),
                ));
            }
            Some(backend) if backend.trim().eq_ignore_ascii_case("custom") => {
                errors.push(PipelineValidationError::new(
                    "backend_not_allowed",
                    step_id,
                    format!(
                        "A {subject} requires an official read-only CLI adapter or API transport."
                    ),
                ));
            }
            Some(backend) if !ALLOWED_BACKENDS.contains(&backend) => {
                errors.push(PipelineValidationError::new(
                    "backend_unknown",
                    step_id,
                    format!("\"{backend}\" is not a known CLI."),
                ));
            }
            Some(_) => {
                // Automatic runs bind a model the host verifies afterwards; a
                // CLI's own default names none (`require_verifiable_cli_selection`).
                if agent
                    .model
                    .as_deref()
                    .is_none_or(|model| model.trim().is_empty())
                {
                    errors.push(PipelineValidationError::new(
                        "model_required",
                        step_id,
                        "Choose a model: automatic runs need a fixed model the app can verify.",
                    ));
                }
            }
        },
        StepTransport::Api => {
            if agent
                .api_route_id
                .as_deref()
                .is_none_or(|route| route.trim().is_empty())
            {
                errors.push(PipelineValidationError::new(
                    "api_route_required",
                    step_id,
                    format!("Choose the API route that runs this {subject}."),
                ));
            }
        }
    }
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
    validate_step_agent(&step.agent, id, "step", errors);
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
    let mut verdict: StageVerdictV1 = serde_json::from_str(json_body(raw)).map_err(|error| {
        let detail = error.to_string().replace('`', "\"");
        format!("The answer is not a valid verdict: {detail}")
    })?;
    for finding in &mut verdict.findings {
        // Host-owned flag: untrusted model output cannot forge a missing-file marker.
        finding.file_not_found = false;
    }
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
            problems.push("A \"reject\" needs at least one blocking finding.".to_owned());
        }
        Verdict::Pass if blocking => {
            problems.push("A \"pass\" cannot have a blocking finding.".to_owned());
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
        let trimmed_id = finding.id.trim();
        if trimmed_id.is_empty() {
            problems.push("Every finding needs a non-empty id.".to_owned());
        } else if !ids.insert(trimmed_id) {
            problems.push(format!("Two findings share the id \"{trimmed_id}\"."));
        }
        if finding.line == Some(0) {
            if trimmed_id.is_empty() {
                problems.push("A finding's line number starts at 1.".to_owned());
            } else {
                problems.push(format!(
                    "Finding \"{trimmed_id}\": line numbers start at 1."
                ));
            }
        }
        if finding.message.chars().count() > MAX_FINDING_CHARS
            || finding
                .suggestion
                .as_deref()
                .is_some_and(|text| text.chars().count() > MAX_FINDING_CHARS)
        {
            if trimmed_id.is_empty() {
                problems.push(format!(
                    "A finding's message and suggestion have at most {MAX_FINDING_CHARS} characters."
                ));
            } else {
                problems.push(format!(
                    "Finding \"{trimmed_id}\": message and suggestion have at most {MAX_FINDING_CHARS} characters."
                ));
            }
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
                        "id": { "type": "string", "minLength": 1 },
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

/// A child pipeline (feature 25, M2b, plan P1): what it changes in its
/// parent, step by step. Its parent's edits reach it by the same adoption
/// as any version; one level only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomPipelineChildV1 {
    pub schema_version: u32,
    /// The child's own trigger; the parent's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<PipelineTrigger>,
    /// Parent steps the child leaves out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// Fields of parent steps the child changes, by step id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub change: BTreeMap<String, StepChange>,
    /// The child's own steps, each after a step (or first), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub insert: Vec<StepInsert>,
}

/// The fields of a parent step a child changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// An agent step's transport, CLI, model and effort, together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<StepAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<StepOutput>,
    /// A gate's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl StepChange {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepInsert {
    /// The step it follows; the first place when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub step: PipelineStep,
}

/// What a child resolves to over its parent: the definition, with every
/// change that still fits applied, and what no longer fits. A child with a
/// conflict is `Needs edit` (P1); editing it from this definition and saving
/// derives its changes again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChild {
    pub definition: CustomPipelineDefinitionV1,
    pub conflicts: Vec<String>,
}

pub fn parse_child(json: &str) -> Result<CustomPipelineChildV1, PipelineValidationError> {
    serde_json::from_str(json).map_err(|error| {
        PipelineValidationError::new(
            "definition_invalid",
            None,
            format!("The child pipeline is not valid: {error}"),
        )
    })
}

/// Applies a change to a step; `false` when the change does not fit its kind.
fn apply_change(step: &mut PipelineStep, change: &StepChange) -> bool {
    match step {
        PipelineStep::Agent(agent) => {
            if change.prompt.is_some() {
                return false;
            }
            if let Some(label) = &change.label {
                agent.label.clone_from(label);
            }
            if let Some(config) = &change.agent {
                agent.agent = config.clone();
            }
            if let Some(instructions) = &change.instructions {
                agent.instructions.clone_from(instructions);
            }
            if let Some(output) = change.output {
                agent.output = output;
            }
            true
        }
        PipelineStep::Gate(gate) => {
            if change.agent.is_some() || change.instructions.is_some() || change.output.is_some() {
                return false;
            }
            if let Some(label) = &change.label {
                gate.label.clone_from(label);
            }
            if let Some(prompt) = &change.prompt {
                gate.prompt.clone_from(prompt);
            }
            true
        }
        PipelineStep::HostTool(tool) => {
            if change.agent.is_some()
                || change.instructions.is_some()
                || change.output.is_some()
                || change.prompt.is_some()
            {
                return false;
            }
            if let Some(label) = &change.label {
                tool.label.clone_from(label);
            }
            true
        }
    }
}

/// The child over its parent: removals, then changes, then its own steps.
pub fn resolve_child(
    parent: &CustomPipelineDefinitionV1,
    child: &CustomPipelineChildV1,
) -> ResolvedChild {
    let mut conflicts = Vec::new();
    let mut steps = parent.steps.clone();
    for id in &child.remove {
        match steps.iter().position(|step| step.id() == id) {
            Some(index) => {
                steps.remove(index);
            }
            None => conflicts.push(format!(
                "The parent no longer has the step \"{id}\" this pipeline leaves out."
            )),
        }
    }
    for (id, change) in &child.change {
        match steps.iter_mut().find(|step| step.id() == id) {
            Some(step) => {
                if !apply_change(step, change) {
                    conflicts.push(format!("The step \"{id}\" changed kind in the parent."));
                }
            }
            None => conflicts.push(format!(
                "The parent no longer has the step \"{id}\" this pipeline changes."
            )),
        }
    }
    for insert in &child.insert {
        let id = insert.step.id();
        if steps.iter().any(|step| step.id() == id) {
            conflicts.push(format!("The parent now has a step \"{id}\" too."));
            continue;
        }
        let at = match &insert.after {
            None => 0,
            Some(after) => match steps.iter().position(|step| step.id() == after) {
                Some(index) => index + 1,
                None => {
                    conflicts.push(format!(
                        "The parent no longer has the step \"{after}\" that \"{id}\" follows."
                    ));
                    steps.len()
                }
            },
        };
        steps.insert(at, insert.step.clone());
    }
    ResolvedChild {
        definition: CustomPipelineDefinitionV1 {
            schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
            trigger: child
                .trigger
                .clone()
                .unwrap_or_else(|| parent.trigger.clone()),
            steps,
        },
        conflicts,
    }
}

fn step_change(parent: &PipelineStep, edited: &PipelineStep) -> Option<StepChange> {
    let mut change = StepChange::default();
    if parent.label() != edited.label() {
        change.label = Some(edited.label().to_owned());
    }
    match (parent, edited) {
        (PipelineStep::Agent(before), PipelineStep::Agent(after)) => {
            if before.agent != after.agent {
                change.agent = Some(after.agent.clone());
            }
            if before.instructions != after.instructions {
                change.instructions = Some(after.instructions.clone());
            }
            if before.output != after.output {
                change.output = Some(after.output);
            }
        }
        (PipelineStep::Gate(before), PipelineStep::Gate(after)) => {
            if before.prompt != after.prompt {
                change.prompt = Some(after.prompt.clone());
            }
        }
        (PipelineStep::HostTool(before), PipelineStep::HostTool(after))
            if before.tool == after.tool => {}
        // Another kind under the same id is not a change: the caller
        // removes the parent's step and inserts the child's.
        _ => return None,
    }
    Some(change)
}

/// The changes that make `edited` out of `parent`. The parent's steps keep
/// their order: a child cannot move them, only leave them out or put its
/// own steps between them.
pub fn child_changes(
    parent: &CustomPipelineDefinitionV1,
    edited: &CustomPipelineDefinitionV1,
) -> Result<CustomPipelineChildV1, PipelineValidationError> {
    let mut child = CustomPipelineChildV1 {
        schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
        trigger: (edited.trigger != parent.trigger).then(|| edited.trigger.clone()),
        ..CustomPipelineChildV1::default()
    };
    // Parent steps the child keeps, as the same kind.
    let mut kept = Vec::new();
    for step in &parent.steps {
        let same = edited
            .steps
            .iter()
            .find(|candidate| candidate.id() == step.id())
            .and_then(|candidate| step_change(step, candidate).map(|change| (candidate, change)));
        match same {
            Some((_, change)) => {
                if !change.is_empty() {
                    child.change.insert(step.id().to_owned(), change);
                }
                kept.push(step.id().to_owned());
            }
            None => child.remove.push(step.id().to_owned()),
        }
    }
    let order = edited
        .steps
        .iter()
        .map(PipelineStep::id)
        .filter(|id| kept.iter().any(|kept| kept == id))
        .collect::<Vec<_>>();
    if order != kept.iter().map(String::as_str).collect::<Vec<_>>() {
        return Err(PipelineValidationError::new(
            "child_order",
            None,
            "A child keeps its parent's steps in their order. Remove a step and add your own where you want it.",
        ));
    }
    let mut previous: Option<String> = None;
    for step in &edited.steps {
        if !kept.iter().any(|id| id == step.id()) {
            child.insert.push(StepInsert {
                after: previous.clone(),
                step: step.clone(),
            });
        }
        previous = Some(step.id().to_owned());
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_step(id: &str, label: &str) -> PipelineStep {
        PipelineStep::Agent(AgentStep {
            id: id.into(),
            label: label.into(),
            agent: StepAgent {
                transport: StepTransport::Cli,
                backend: Some("claude".into()),
                model: Some("haiku".into()),
                model_profile: None,
                thinking_level: "default".into(),
                api_route_id: None,
            },
            instructions: format!("Do {id}."),
            output: StepOutput::Text,
        })
    }

    fn gate_step(id: &str) -> PipelineStep {
        PipelineStep::Gate(GateStep {
            id: id.into(),
            label: format!("Gate {id}"),
            prompt: "Check.".into(),
        })
    }

    fn manual(steps: Vec<PipelineStep>) -> CustomPipelineDefinitionV1 {
        CustomPipelineDefinitionV1 {
            schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
            trigger: PipelineTrigger::Manual,
            steps,
        }
    }

    /// A small deterministic generator: no new dependency for the property.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self, below: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % below.max(1)
        }
    }

    /// Plan P1: whatever a child makes of its parent (leaving steps out,
    /// changing their fields, putting its own steps between them, another
    /// trigger), its changes resolve back to exactly what it made.
    #[test]
    fn a_childs_changes_resolve_back_to_what_it_made() {
        let mut random = Lcg(0x5eed);
        for case in 0..600 {
            let parent_len = 1 + random.next(6);
            let parent = manual(
                (0..parent_len)
                    .map(|index| {
                        if random.next(3) == 0 {
                            gate_step(&format!("p{index}"))
                        } else {
                            agent_step(&format!("p{index}"), &format!("Parent {index}"))
                        }
                    })
                    .collect(),
            );
            let mut edited = parent.clone();
            edited.steps.retain(|_| random.next(4) != 0);
            for step in &mut edited.steps {
                match step {
                    PipelineStep::Agent(agent) if random.next(2) == 0 => {
                        agent.label = format!("{} (child {case})", agent.label);
                        if random.next(2) == 0 {
                            agent.agent.backend = Some("codex".into());
                            agent.agent.model = Some("gpt-5.6-luna".into());
                        }
                        if random.next(2) == 0 {
                            agent.output = StepOutput::Verdict;
                        }
                    }
                    PipelineStep::Gate(gate) if random.next(2) == 0 => {
                        gate.prompt = format!("Child {case} checks.");
                    }
                    _ => {}
                }
            }
            for extra in 0..random.next(3) {
                let at = random.next(edited.steps.len() + 1);
                edited.steps.insert(
                    at,
                    agent_step(&format!("c{extra}"), &format!("Child {extra}")),
                );
            }
            if random.next(4) == 0 {
                edited.trigger = PipelineTrigger::DefaultEvent {
                    event: DefaultEvent::PulsationDone,
                    stage: None,
                };
            }
            let changes = child_changes(&parent, &edited).unwrap();
            let json = serde_json::to_string(&changes).unwrap();
            assert_eq!(parse_child(&json).unwrap(), changes, "case {case}");
            let resolved = resolve_child(&parent, &changes);
            assert!(
                resolved.conflicts.is_empty(),
                "case {case}: {:?}",
                resolved.conflicts
            );
            assert_eq!(resolved.definition, edited, "case {case}");
        }
    }

    #[test]
    fn an_untouched_child_holds_no_change_and_follows_its_parent() {
        let parent = manual(vec![agent_step("a", "A"), gate_step("b")]);
        let changes = child_changes(&parent, &parent).unwrap();
        assert_eq!(
            serde_json::to_value(&changes).unwrap(),
            json!({"schemaVersion": 1})
        );
        // The parent's next version reaches the child as it is.
        let next = manual(vec![
            agent_step("a", "A2"),
            gate_step("b"),
            agent_step("z", "Z"),
        ]);
        assert_eq!(resolve_child(&next, &changes).definition, next);
    }

    #[test]
    fn a_child_keeps_its_parents_order() {
        let parent = manual(vec![agent_step("a", "A"), agent_step("b", "B")]);
        let swapped = manual(vec![agent_step("b", "B"), agent_step("a", "A")]);
        assert_eq!(
            child_changes(&parent, &swapped).unwrap_err().code,
            "child_order"
        );
    }

    /// A parent edit that breaks a change puts the child in `Needs edit`, with
    /// the reason; what still fits is applied.
    #[test]
    fn parent_edits_that_break_a_change_are_conflicts() {
        let parent = manual(vec![
            agent_step("a", "A"),
            gate_step("b"),
            agent_step("c", "C"),
        ]);
        let mut edited = parent.clone();
        edited.steps.remove(1);
        if let PipelineStep::Agent(agent) = &mut edited.steps[1] {
            agent.label = "Changed C".into();
            agent.instructions = "The child's way.".into();
        }
        edited.steps.insert(1, agent_step("mine", "Mine"));
        let changes = child_changes(&parent, &edited).unwrap();
        assert_eq!(changes.remove, ["b"]);
        assert_eq!(changes.insert[0].after.as_deref(), Some("a"));
        // The parent drops `a` and `c`, and has a step `mine` of its own.
        let next = manual(vec![gate_step("b"), agent_step("mine", "Theirs")]);
        let resolved = resolve_child(&next, &changes);
        let conflicts = resolved.conflicts.join(" ");
        assert!(
            conflicts.contains("\"c\" this pipeline changes"),
            "{conflicts}"
        );
        assert!(conflicts.contains("now has a step \"mine\""), "{conflicts}");
        assert!(
            resolved
                .definition
                .steps
                .iter()
                .all(|step| step.id() != "b")
        );
        // A step whose kind changed under the child's change.
        let kind = manual(vec![agent_step("a", "A"), gate_step("b"), gate_step("c")]);
        let resolved = resolve_child(&kind, &changes);
        assert!(
            resolved
                .conflicts
                .iter()
                .any(|conflict| conflict.contains("changed kind"))
        );
        // The step an own step follows is gone: it goes last.
        let gone = manual(vec![gate_step("b"), agent_step("c", "C")]);
        let resolved = resolve_child(&gone, &changes);
        assert!(
            resolved
                .conflicts
                .iter()
                .any(|conflict| conflict.contains("that \"mine\" follows"))
        );
        assert_eq!(resolved.definition.steps.last().unwrap().id(), "mine");
    }

    #[test]
    fn unknown_fields_are_refused_in_a_child() {
        let refused = r#"{"schemaVersion":1,"remove":["a"],"extra":true}"#;
        assert_eq!(parse_child(refused).unwrap_err().code, "definition_invalid");
        let refused = r#"{"schemaVersion":1,"change":{"a":{"label":"x","model":"y"}}}"#;
        assert!(parse_child(refused).is_err());
    }

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
        assert_eq!(
            schema["properties"]["findings"]["items"]["properties"]["id"]["minLength"],
            json!(1)
        );
    }

    #[test]
    fn verdicts_hardening_rejects_empty_ids_line_zero_and_forged_file_not_found() {
        let blank_id = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"   ","severity":"note","message":"msg","file":null,"line":null,"suggestion":null}]}"#;
        let err = parse_verdict(blank_id).unwrap_err();
        assert!(err.contains("non-empty id"));
        assert!(!err.contains('`'));

        let empty_id = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"","severity":"note","message":"msg","file":null,"line":null,"suggestion":null}]}"#;
        assert!(parse_verdict(empty_id).is_err());

        let dup_trimmed = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"f1","severity":"note","message":"a","file":null,"line":null,"suggestion":null},{"id":" f1 ","severity":"warning","message":"b","file":null,"line":null,"suggestion":null}]}"#;
        let err = parse_verdict(dup_trimmed).unwrap_err();
        assert!(err.contains("share the id"));
        assert!(!err.contains('`'));

        let line_zero = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"f1","severity":"note","message":"a","file":"src/lib.rs","line":0,"suggestion":null}]}"#;
        let err = parse_verdict(line_zero).unwrap_err();
        assert!(err.contains("start at 1"));
        assert!(!err.contains('`'));

        let line_one = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"f1","severity":"note","message":"a","file":"src/lib.rs","line":1,"suggestion":null}]}"#;
        assert!(parse_verdict(line_one).is_ok());

        let line_none = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"f1","severity":"note","message":"a","file":"src/lib.rs","line":null,"suggestion":null}]}"#;
        assert!(parse_verdict(line_none).is_ok());

        // Untrusted model JSON setting fileNotFound is cleared by parse_verdict,
        // while persisted DB JSON deserialized with serde_json preserves it.
        let forged = r#"{"verdict":"pass","summary":"ok","findings":[{"id":"f1","severity":"note","message":"a","file":null,"line":null,"suggestion":null,"fileNotFound":true}]}"#;
        let parsed = parse_verdict(forged).unwrap();
        assert!(!parsed.findings[0].file_not_found);
        let persisted: StageVerdictV1 = serde_json::from_str(forged).unwrap();
        assert!(persisted.findings[0].file_not_found);

        // Zero literal backticks across all validate_verdict error branches.
        let all_problems = StageVerdictV1 {
            verdict: Verdict::Pass,
            summary: "s".repeat(MAX_SUMMARY_CHARS + 1),
            findings: vec![
                VerdictFinding {
                    id: "".into(),
                    severity: FindingSeverity::Blocking,
                    message: "m".repeat(MAX_FINDING_CHARS + 1),
                    file: None,
                    line: Some(0),
                    suggestion: None,
                    file_not_found: false,
                },
                VerdictFinding {
                    id: "dup".into(),
                    severity: FindingSeverity::Note,
                    message: "ok".into(),
                    file: None,
                    line: Some(0),
                    suggestion: Some("x".repeat(MAX_FINDING_CHARS + 1)),
                    file_not_found: false,
                },
                VerdictFinding {
                    id: " dup ".into(),
                    severity: FindingSeverity::Note,
                    message: "ok".into(),
                    file: None,
                    line: Some(1),
                    suggestion: None,
                    file_not_found: false,
                },
            ],
        };
        let problems = validate_verdict(&all_problems);
        assert!(problems.len() >= 6);
        assert!(problems.iter().all(|p| !p.contains('`')));

        let reject_without_blocking = StageVerdictV1 {
            verdict: Verdict::Reject,
            summary: "nope".into(),
            findings: Vec::new(),
        };
        let reject_problems = validate_verdict(&reject_without_blocking);
        assert_eq!(reject_problems.len(), 1);
        assert!(!reject_problems[0].contains('`'));
    }
}
