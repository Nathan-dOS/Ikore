//! M7: cost/quality qualification harness for pipeline v4 (ADR-022).
//!
//! Four arms run the same task batch over the real supervisor, router and
//! SQLite: **A** legacy pipeline v3, **B** v4 with one unit per stage,
//! **C** v4 with host-planned units run one at a time, **D** v4 with up to
//! two independent units in flight. Fixture mode scripts every worker reply
//! from the prompt the host rendered (no cost, orchestration only). Pilot
//! mode sends the same prompts to one real provider under an explicit spend
//! limit and is never part of CI.
//!
//! Correctness is judged by each fixture's oracle - content predicates on
//! the published files - never by the tests the workers authored.
use crate::composition_replies::{
    PromptFacts, ReplyOptions, between, parse_prompt, unit_response_with,
};
use crate::pulsation_benchmark::{configure_loopback, force_api_direct};
use crate::{
    FixedEmbeddingGateway, IsolatedTestEnvironment, MemoryCredentialStore, RecordedRequest,
    ScriptedHttpServer, ScriptedResponse,
};
use ikore_core::{
    AppCoreDependencies,
    feature_forge::{
        AcceptanceCriterion, CreatePulsationFromTasksRequest, CreateTasksRequest, TaskDraft,
        TaskKind, TaskSelection, TaskService,
    },
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::{PreparationSettings, composition::settings::CompositionSettings},
    persistence_service::{
        ApplyHitlDecisionRequest, EnsureProjectForWorkspaceRequest,
        GetPulsationRuntimeSnapshotRequest, HitlDecision, PersistenceService,
        ResolveStageFailureRequest, StageFailureDecision, UpdateAgentRuntimeConfigRequest,
    },
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const FIXTURE_VERSION: &str = "stage-composition-fixtures-v1";
/// Wall-clock ceiling for one pulsation in fixture mode.
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(120);
/// Wall-clock ceiling for one pulsation in pilot mode (real provider).
const PILOT_TIMEOUT: Duration = Duration::from_secs(1_800);

struct SilentEvents;
impl LlmEventSink for SilentEvents {
    fn emit(&self, _: &LlmEventEnvelope) {}
}

// ----------------------------------------------------------------- arms

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    /// Pipeline v3 (legacy): one invocation per stage, full handoff prompt.
    A,
    /// Pipeline v4 with exactly one unit per stage (no split).
    B,
    /// Pipeline v4 with host-planned units, one in flight.
    C,
    /// Pipeline v4 with host-planned units, up to two in flight.
    D,
}

impl Arm {
    pub const ALL: [Arm; 4] = [Arm::A, Arm::B, Arm::C, Arm::D];

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_uppercase().as_str() {
            "A" => Some(Arm::A),
            "B" => Some(Arm::B),
            "C" => Some(Arm::C),
            "D" => Some(Arm::D),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Arm::A => "A: pipeline v3 (legacy, one call per stage)",
            Arm::B => "B: pipeline v4, one unit per stage",
            Arm::C => "C: pipeline v4, planned units, sequential",
            Arm::D => "D: pipeline v4, planned units, up to two in flight",
        }
    }

    /// `None` keeps the legacy pipeline; otherwise the project settings.
    /// Arm B is composition without promoted recipes (one unit per stage);
    /// C and D promote Code and Test.
    pub fn composition(self) -> Option<CompositionSettings> {
        match self {
            Arm::A => None,
            Arm::B => Some(CompositionSettings {
                enabled: true,
                ..CompositionSettings::default()
            }),
            Arm::C => Some(
                CompositionSettings {
                    enabled: true,
                    ..CompositionSettings::default()
                }
                .with_all_recipes_qualified(),
            ),
            Arm::D => Some(
                CompositionSettings {
                    enabled: true,
                    max_in_flight_units: 2,
                    ..CompositionSettings::default()
                }
                .with_all_recipes_qualified(),
            ),
        }
    }

    fn plans_units(self) -> bool {
        matches!(self, Arm::C | Arm::D)
    }
}

// ----------------------------------------------------------------- fixtures

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    SimpleMaintenance,
    CohesiveMultiFile,
    SeparableMultiTask,
    FailureRecovery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureCriterion {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureTask {
    pub title: String,
    pub kind: String,
    pub description: String,
    pub criteria: Vec<FixtureCriterion>,
}

/// A unit the Specification worker proposes in arms C/D.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureUnit {
    pub unit_key: String,
    pub kind: String,
    pub task: usize,
    pub criterion_ids: Vec<String>,
    pub input_paths: Vec<String>,
    pub destination_path: String,
    pub depends_on: Vec<String>,
}

/// Independent judge of a published file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OracleCheck {
    pub path: String,
    pub must_contain: Vec<String>,
    pub must_not_contain: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    /// The first call of the stage fails at the transport (HTTP 500).
    TransportOnce,
    /// The first call of the stage returns a reply the host rejects.
    MalformedOnce,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fault {
    pub kind: FaultKind,
    pub stage: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fixture {
    pub id: String,
    pub family: Family,
    pub held_out: bool,
    pub sources: BTreeMap<String, String>,
    pub tasks: Vec<FixtureTask>,
    pub units: Vec<FixtureUnit>,
    pub outputs: BTreeMap<String, String>,
    pub oracle: Vec<OracleCheck>,
    pub fault: Option<Fault>,
}

impl Fixture {
    pub fn task_drafts(&self) -> Vec<TaskDraft> {
        self.tasks
            .iter()
            .map(|task| TaskDraft {
                title: task.title.clone(),
                kind: match task.kind.as_str() {
                    "feature" => TaskKind::Feature,
                    "refactor" => TaskKind::Refactor,
                    _ => TaskKind::Bug,
                },
                description: task.description.clone(),
                criteria: task
                    .criteria
                    .iter()
                    .map(|criterion| AcceptanceCriterion {
                        id: criterion.id.clone(),
                        description: criterion.description.clone(),
                    })
                    .collect(),
                dependencies: vec![],
                feature_id: None,
            })
            .collect()
    }

    /// Checks the published workspace; returns the failed predicates.
    pub fn judge(&self, workspace: &Path) -> Vec<String> {
        let mut failures = Vec::new();
        for check in &self.oracle {
            let content = fs::read_to_string(workspace.join(&check.path)).unwrap_or_default();
            if content.is_empty() {
                failures.push(format!("{}: missing", check.path));
                continue;
            }
            for needle in &check.must_contain {
                if !content.contains(needle) {
                    failures.push(format!("{}: missing `{needle}`", check.path));
                }
            }
            for needle in &check.must_not_contain {
                if content.contains(needle) {
                    failures.push(format!("{}: still contains `{needle}`", check.path));
                }
            }
        }
        failures
    }
}

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/composition")
}

pub fn load_fixtures() -> Vec<Fixture> {
    let mut fixtures = fs::read_dir(fixtures_dir())
        .expect("composition fixtures directory")
        .map(|entry| entry.expect("fixture entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            let text = fs::read_to_string(&path).expect("fixture file");
            serde_json::from_str::<Fixture>(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect::<Vec<_>>();
    fixtures.sort_by(|a, b| a.id.cmp(&b.id));
    fixtures
}

pub fn fixtures_sha256(fixtures: &[Fixture]) -> String {
    ikore_core::orchestration::ContentSha256::of(&serde_json::to_vec(fixtures).unwrap())
        .as_str()
        .to_owned()
}

// ----------------------------------------------------------------- modes

#[derive(Clone, Debug)]
pub struct PilotConfig {
    pub provider_id: String,
    pub endpoint_url: String,
    pub model_id: String,
    pub api_key: String,
    pub spend_limit_micro_usd: u64,
}

#[derive(Clone, Debug)]
pub enum Mode {
    Fixture,
    Pilot(PilotConfig),
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Fixture => "fixture",
            Mode::Pilot(_) => "pilot",
        }
    }
}

// ----------------------------------------------------------------- observation

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub arm: Arm,
    pub fixture: String,
    pub family: Family,
    pub held_out: bool,
    pub mode: String,
    /// The pulsation reached `done`.
    pub completed: bool,
    /// Every oracle predicate held on the published workspace.
    pub correct: bool,
    pub oracle_failures: Vec<String>,
    pub provider_calls: u64,
    /// Attempts the host rejected or that failed at the transport.
    pub rejected_attempts: u64,
    pub human_checkpoints: u64,
    pub failure_checkpoints: u64,
    /// Interventions the driver had to make beyond the standard approvals.
    pub interventions: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub reasoning_tokens: u64,
    pub usage_records: u64,
    pub usage_partial_records: u64,
    pub usage_unavailable_records: u64,
    /// Sum of the provider-reported cost, when any attempt reported one.
    pub estimated_cost_micro_usd: Option<u64>,
    pub prompt_chars: u64,
    /// One entry per provider call, in call order (fixture mode only).
    #[serde(default)]
    pub prompt_calls: Vec<PromptCall>,
    /// What the prompt input inventory (ADR-037) rejected in the prompts
    /// this observation served (fixture mode only).
    #[serde(default)]
    pub inventory_violations: Vec<String>,
    pub wall_ms: u64,
    pub diagnostics: Vec<String>,
}

/// The size of one rendered prompt, kept per call so ADR-037 M1 can compare
/// its prompt growth call by call against the baseline before M1.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromptCall {
    pub executor: String,
    pub chars: u64,
    /// Content items M1 labels with a provenance line (see `provenance_items`).
    pub items: u32,
}

/// What the scripted provider sees across one pulsation.
#[derive(Default)]
struct ProviderLog {
    calls: u64,
    prompt_chars: u64,
    fault_injected: bool,
    executors: Vec<String>,
    prompt_calls: Vec<PromptCall>,
    inventory_violations: Vec<String>,
}

/// Counts the content items ADR-037 M1 gives a provenance line: every `### `
/// title outside `## Interfaces`, the sections authored by agents and the
/// blocks the stage guidance embeds. An estimate for the growth ceiling, not a
/// parser of the prompt grammar.
fn provenance_items(user: &str) -> u32 {
    const SECTIONS: [&str; 9] = [
        "## Tasks and criteria",
        "## Decisions",
        "## Interfaces",
        "## Changes (base → candidate)",
        "## Author claims (narrative, not evidence)",
        "## Review policy (read-only)",
        "## Task batch - authoritative scope",
        "## Approved handoff",
        "## Exact code context (derived - not citable)",
    ];
    const EMBEDDED: [&str; 6] = [
        "Feature `",
        "Revision feedback from the developer:",
        "Clarification - Q:",
        "Issues reported by Intake:",
        "Approved interface intent:",
        "Approved unit proposals",
    ];
    let mut items = 0;
    let mut in_interfaces = false;
    for line in user.lines() {
        if line.starts_with("## ") {
            in_interfaces = line == "## Interfaces";
        }
        if SECTIONS.contains(&line)
            || EMBEDDED.iter().any(|marker| line.starts_with(marker))
            || (line.starts_with("### ") && !in_interfaces)
        {
            items += 1;
        }
    }
    items
}

fn legacy_stage(prompt: &str) -> Option<&str> {
    prompt
        .lines()
        .find_map(|line| line.strip_prefix("- stage: "))
}

/// A pipeline v3 reply built from the batch scope the host rendered:
/// artifacts with real destinations for Code/Test, one evidence row per
/// listed task citing only its listed criteria.
fn legacy_reply(prompt: &str, fixture: &Fixture, malformed: bool) -> Value {
    let stage = legacy_stage(prompt).unwrap_or("Intake");
    let mut tasks: Vec<(String, Vec<String>)> = Vec::new();
    for line in prompt.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.split("taskId: ").nth(1) {
            tasks.push((rest.trim().to_owned(), Vec::new()));
        } else if let Some(rest) = trimmed.strip_prefix("criterionIds: ")
            && let Some(last) = tasks.last_mut()
        {
            last.1 = rest
                .split(',')
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty() && c != "(none)")
                .collect();
        }
    }
    let artifacts: Vec<Value> = match stage {
        "Code" => fixture
            .outputs
            .iter()
            .filter(|(path, _)| !is_test_path(path))
            .map(|(path, content)| {
                json!({"name": path, "kind": "code", "relativePath": path, "content": content})
            })
            .collect(),
        "Test" => fixture
            .outputs
            .iter()
            .filter(|(path, _)| is_test_path(path))
            .map(|(path, content)| {
                json!({"name": path, "kind": "test", "relativePath": path, "content": content})
            })
            .collect(),
        "SPECIFICATION" => vec![json!({"name": "specification", "kind": "specification", "relativePath": null,
            "content": format!("# Specification\n\n{}\n", fixture.tasks.iter().map(|t| format!("- {}: {}", t.title, t.description)).collect::<Vec<_>>().join("\n"))})],
        "Contract" => vec![json!({"name": "contract", "kind": "contract", "relativePath": null,
            "content": "# Contract\n\nPublic signatures are preserved; behaviour follows the specification.\n"})],
        "Integrator" => vec![json!({"name": "integration report", "kind": "integration_report", "relativePath": null,
            "content": "# Integration report\n\nEvery criterion is supported by the delivered files and authored tests.\n"})],
        _ => vec![json!({"name": "intake", "kind": "stage_output", "relativePath": null,
            "content": "# Intake\n\nEach task is a low-risk bugfix in the named file.\n"})],
    };
    let positions: Vec<usize> = (0..artifacts.len().max(1)).collect();
    let evidence: Vec<Value> = tasks
        .iter()
        .map(|(task, criteria)| {
            json!({
                "taskId": if malformed { "task-that-does-not-exist".to_owned() } else { task.clone() },
                "criterionIds": criteria,
                "artifactPositions": if artifacts.len() > 1 { positions.clone() } else { vec![0] },
                "summary": "The named file was changed as specified; no command execution is claimed."
            })
        })
        .collect();
    let mut output = json!({
        "outputLanguage": "en",
        "summary": format!("{stage} fixture"),
        "statusUpdate": if matches!(stage, "Intake" | "Code") { "advance" } else { "checkpoint_ready" },
        "contextUsage": [],
        "notes": [],
        "artifacts": artifacts,
        "taskEvidence": evidence
    });
    if stage == "SPECIFICATION" {
        output["componentProposals"] = json!([]);
    }
    output
}

fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/") || path.ends_with("_test.rs")
}

/// The `unitProposals` of arms C/D, expressed with the handles the host
/// rendered for this Specification prompt.
fn proposals_for(fixture: &Fixture, facts: &PromptFacts) -> Value {
    Value::Array(
        fixture
            .units
            .iter()
            .map(|unit| {
                let task = format!("task_{}", unit.task + 1);
                let criteria: Vec<String> = unit
                    .criterion_ids
                    .iter()
                    .map(|id| {
                        let position = fixture.tasks[unit.task]
                            .criteria
                            .iter()
                            .position(|c| &c.id == id)
                            .expect("criterion of the task");
                        format!("crit_{}_{}", unit.task + 1, position + 1)
                    })
                    .collect();
                let inputs: Vec<&str> = unit
                    .input_paths
                    .iter()
                    .map(|path| facts.handle_for_path(path))
                    .collect();
                json!({
                    "unitKey": unit.unit_key, "kind": unit.kind, "taskRefs": [task], "criterionRefs": criteria,
                    "inputRefs": inputs, "destinationRefs": [facts.handle_for_path(&unit.destination_path)],
                    "dependsOn": unit.depends_on, "objective": format!("Change {} as the specification requires.", unit.destination_path)
                })
            })
            .collect(),
    )
}

/// A v4 reply the host rejects: an intake row citing an unknown task.
fn malformed_v4_reply(executor: &str) -> Value {
    let payload = match executor {
        "intake_classifier" => {
            json!({"tasks": [{"taskRef": "task_9", "classification": "bugfix", "risk": "low", "summary": "Wrong handle."}], "issues": [], "usedRefs": []})
        }
        _ => {
            json!({"edits": [], "evidence": [{"taskRef": "task_9", "criterionRefs": [], "artifactKeys": [], "inputRefs": [], "explanation": "Wrong handle."}], "usedRefs": []})
        }
    };
    json!({"schemaVersion": 1, "outcome": "completed", "executorKind": executor, "payload": payload})
}

fn stage_of_executor(executor: &str) -> &'static str {
    match executor {
        "intake_classifier" => "Intake",
        "specification_author" => "SPECIFICATION",
        "contract_author" => "Contract",
        "code_implementer" | "code_repairer" => "Code",
        "test_author" | "test_repairer" => "Test",
        _ => "Integrator",
    }
}

/// Simulated accounting: characters over four, labelled as such in the
/// report. Never a tokenizer or a price.
fn simulated_usage(prompt_chars: usize, content: &str) -> Value {
    json!({
        "prompt_tokens": prompt_chars.div_ceil(4),
        "completion_tokens": content.chars().count().div_ceil(4)
    })
}

fn scripted_reply(
    request: &RecordedRequest,
    arm: Arm,
    fixture: &Fixture,
    log: &Mutex<ProviderLog>,
) -> ScriptedResponse {
    let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
    let model = body["model"]
        .as_str()
        .expect("provider request model identity");
    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
    let user = body["messages"][1]["content"].as_str().unwrap_or_default();
    let prompt_chars = system.chars().count() + user.chars().count();
    let composed = between(system, "You are the `", "` worker").is_some();
    let mut log = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    log.calls += 1;
    log.prompt_chars += prompt_chars as u64;
    let (stage, executor) = if composed {
        let facts = parse_prompt(request);
        (
            stage_of_executor(&facts.executor).to_owned(),
            facts.executor,
        )
    } else {
        let stage = legacy_stage(user).unwrap_or("Intake").to_owned();
        (stage.clone(), format!("legacy:{stage}"))
    };
    log.executors.push(executor.clone());
    log.prompt_calls.push(PromptCall {
        executor: executor.clone(),
        chars: prompt_chars as u64,
        items: provenance_items(user),
    });
    if let Err(errors) = crate::prompt_inventory::check_prompt(system, user) {
        let call = log.calls;
        log.inventory_violations.extend(
            errors
                .into_iter()
                .map(|error| format!("call {call}: {error}")),
        );
    }
    let fault = fixture
        .fault
        .as_ref()
        .filter(|fault| !log.fault_injected && fault.stage == stage)
        .map(|fault| fault.kind);
    if let Some(kind) = fault {
        log.fault_injected = true;
        match kind {
            FaultKind::TransportOnce => {
                return ScriptedResponse::json(500, r#"{"error":{"message":"fixture outage"}}"#);
            }
            FaultKind::MalformedOnce => {
                let content = if composed {
                    malformed_v4_reply(&executor)
                } else {
                    legacy_reply(user, fixture, true)
                };
                return ScriptedResponse::json(
                    200,
                    json!({"model":model,"choices":[{"message":{"content":content.to_string()},"finish_reason":"stop"}],
                           "usage": simulated_usage(prompt_chars, &content.to_string())})
                    .to_string(),
                );
            }
        }
    }
    let content = if composed {
        let facts = parse_prompt(request);
        let mut options = ReplyOptions {
            contents: fixture.outputs.clone(),
            ..ReplyOptions::default()
        };
        if facts.executor == "specification_author" {
            options.proposals = Some(if arm.plans_units() {
                proposals_for(fixture, &facts)
            } else {
                json!([])
            });
        }
        if facts.executor == "contract_author" && !arm.plans_units() {
            // No proposals were made, so nothing is bound (arm B).
            options.bindings = Some(json!([]));
        }
        unit_response_with(&facts, &options)
    } else {
        legacy_reply(user, fixture, false)
    };
    ScriptedResponse::json(
        200,
        json!({"model":model,"choices":[{"message":{"content":content.to_string()},"finish_reason":"stop"}],
               "usage": simulated_usage(prompt_chars, &content.to_string())})
        .to_string(),
    )
}

/// Points the isolated data directory's legacy connection at a real
/// provider for pilot mode. Configuration fixture only; the key lives in
/// the in-memory credential store of this process.
fn configure_pilot(
    service: &PersistenceService,
    credentials: &MemoryCredentialStore,
    pilot: &PilotConfig,
) {
    use ikore_core::llm::credentials::CredentialStore;
    credentials
        .set("openai", &pilot.api_key)
        .expect("pilot credential");
    let db = Connection::open(service.database_path()).unwrap();
    db.execute(
        "UPDATE provider_connections SET provider_id=?1,endpoint_url=?2,default_model=?3 WHERE id='legacy-openai'",
        rusqlite::params![pilot.provider_id, pilot.endpoint_url, pilot.model_id],
    )
    .unwrap();
    db.execute(
        "UPDATE provider_route_targets SET model_id=?1 WHERE connection_id='legacy-openai'",
        [&pilot.model_id],
    )
    .unwrap();
    db.execute(
        "UPDATE runtime_settings SET value='off' WHERE key='runtime.routing_v2_mode'",
        [],
    )
    .unwrap();
    service
        .update_agent_runtime_config(UpdateAgentRuntimeConfigRequest {
            provider: Some(ikore_core::agent_runtime::LlmProvider::Openai),
            model: Some(pilot.model_id.clone()),
            temperature: None,
            max_output_tokens: None,
            timeout_ms: None,
            max_retries: None,
            context_chars_limit: None,
            max_context_files: None,
            context_strategy: None,
            embedding_enabled: None,
            embedding_provider: None,
            embedding_model: None,
            embedding_top_k: None,
            adaptive_context_mode: None,
            dynamic_context_policy: None,
            routing_v2_mode: None,
        })
        .unwrap();
}

fn seed_legacy_baselines(
    service: &PersistenceService,
    project: &str,
    sources: &BTreeMap<String, String>,
) {
    let db = Connection::open(service.database_path()).unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    for (path, content) in sources {
        db.execute(
            "INSERT INTO project_file_baselines(project_id,relative_path,content_sha256,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(project_id,relative_path) DO UPDATE SET content_sha256=excluded.content_sha256,updated_at=excluded.updated_at",
            rusqlite::params![
                project,
                path,
                ikore_core::orchestration::ContentSha256::of(content.as_bytes()).as_str(),
                now
            ],
        )
        .unwrap();
    }
}

struct Driver<'a> {
    service: &'a PersistenceService,
    project: String,
    pulse: String,
    timeout: Duration,
}

enum Outcome {
    Done,
    Failed(String),
    TimedOut(String),
}

impl Driver<'_> {
    /// Approves every artifact review as it appears, retries a parked
    /// stage while the host offers it (counting the intervention, at most
    /// twice per pulsation) and aborts otherwise; answers no clarification
    /// (a question ends the run).
    fn drive(&self, interventions: &mut u64, failure_checkpoints: &mut u64) -> Outcome {
        let db = Connection::open(self.service.database_path()).unwrap();
        let started = Instant::now();
        loop {
            let (status, stage): (String, String) = db
                .query_row(
                    "SELECT status,stage FROM pulsations WHERE id=?1",
                    [&self.pulse],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            match status.as_str() {
                "done" => return Outcome::Done,
                "failed" => {
                    let reason: Option<String> = db
                        .query_row(
                            "SELECT terminal_reason FROM pulsations WHERE id=?1",
                            [&self.pulse],
                            |r| r.get(0),
                        )
                        .unwrap();
                    return Outcome::Failed(reason.unwrap_or_else(|| "failed".into()));
                }
                "checkpoint" => {
                    let kind: Option<String> = db
                        .query_row(
                            "SELECT kind FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' ORDER BY created_at DESC LIMIT 1",
                            [&self.pulse],
                            |r| r.get(0),
                        )
                        .ok();
                    match kind.as_deref() {
                        Some("artifact_review") => {
                            let checkpoint = self.service.get_hitl_checkpoint(&self.pulse).unwrap();
                            // A high-risk review approves with an
                            // acknowledgement (ADR-034 M3); ignored otherwise.
                            self.service
                                .apply_hitl_decision_with_acknowledgements(
                                    ApplyHitlDecisionRequest {
                                        pulsation_id: self.pulse.clone(),
                                        checkpoint_id: checkpoint.checkpoint_id,
                                        expected_state_version: checkpoint.state_version,
                                        feedback: None,
                                        decision: HitlDecision::Approve,
                                    },
                                    None,
                                    Some(ikore_core::persistence_service::RiskAcknowledgement {
                                        reason: "Benchmark approval.".into(),
                                    }),
                                )
                                .unwrap();
                        }
                        Some("stage_failure") => {
                            // The same read the Review tab performs.
                            let snapshot = self
                                .service
                                .get_pulsation_runtime_snapshot(
                                    GetPulsationRuntimeSnapshotRequest {
                                        project_id: self.project.clone(),
                                    },
                                )
                                .unwrap()
                                .snapshot;
                            let Some(checkpoint) = snapshot
                                .failure_checkpoints
                                .into_iter()
                                .find(|c| c.pulsation_id == self.pulse)
                            else {
                                std::thread::sleep(Duration::from_millis(15));
                                continue;
                            };
                            *failure_checkpoints += 1;
                            *interventions += 1;
                            let decision = if checkpoint.allow_retry && *interventions <= 2 {
                                StageFailureDecision::RetryStage
                            } else {
                                StageFailureDecision::Abort
                            };
                            let _ =
                                self.service
                                    .resolve_stage_failure(ResolveStageFailureRequest {
                                        checkpoint_id: checkpoint.checkpoint_id,
                                        expected_state_version: checkpoint.state_version,
                                        decision,
                                        feedback: None,
                                    });
                        }
                        Some(other) => {
                            return Outcome::Failed(format!(
                                "unanswered checkpoint `{other}` at {stage}"
                            ));
                        }
                        None => {}
                    }
                }
                _ => {}
            }
            if started.elapsed() > self.timeout {
                let jobs: Vec<(String, String, Option<String>)> = db
                    .prepare("SELECT kind,status,last_error FROM jobs WHERE pulsation_id=?1")
                    .unwrap()
                    .query_map([&self.pulse], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .map(Result::unwrap)
                    .collect();
                return Outcome::TimedOut(format!("{status} at {stage}; jobs={jobs:?}"));
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

/// Runs one arm over one fixture and measures it.
pub fn run_observation(arm: Arm, fixture: &Fixture, mode: &Mode) -> Observation {
    let env = IsolatedTestEnvironment::new();
    let workspace = env.workspace_dir().to_path_buf();
    for (path, content) in &fixture.sources {
        let target = workspace.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, content).unwrap();
    }
    let log: Arc<Mutex<ProviderLog>> = Arc::default();
    let server = match mode {
        Mode::Fixture => {
            let fixture = fixture.clone();
            let log = log.clone();
            Some(ScriptedHttpServer::respond_with(move |request| {
                scripted_reply(request, arm, &fixture, &log)
            }))
        }
        Mode::Pilot(_) => None,
    };
    let credentials = Arc::new(MemoryCredentialStore::default());
    let deps = AppCoreDependencies::default();
    let service = PersistenceService::initialize_with_dependencies(
        env.data_dir(),
        Arc::new(SilentEvents),
        credentials.clone(),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    match (&server, mode) {
        (Some(server), _) => configure_loopback(&service, server),
        (None, Mode::Pilot(pilot)) => configure_pilot(&service, &credentials, pilot),
        (None, Mode::Fixture) => unreachable!(),
    }
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: false,
            learning_enabled: false,
            ..PreparationSettings::default()
        })
        .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.to_str().unwrap().into(),
            preferred_name: Some(format!("Benchmark {}", fixture.id)),
        })
        .unwrap()
        .project
        .id;
    force_api_direct(&service, &project);
    match arm.composition() {
        Some(settings) => {
            service
                .update_composition_settings(&project, settings)
                .unwrap();
        }
        // The legacy pipeline publishes over an existing file only against a
        // recorded baseline (a file it published or the developer reconciled);
        // a composed stage observes the file at admission instead. Arm A gets
        // the baselines the app records after a base reconciliation, so the
        // arms compare cost on the same task. Configuration fixture only.
        None => seed_legacy_baselines(&service, &project, &fixture.sources),
    }
    let created = TaskService::new(service.clone())
        .create(CreateTasksRequest {
            project_id: project.clone(),
            request_id: format!("bench-{}-{:?}", fixture.id, arm),
            tasks: fixture.task_drafts(),
        })
        .unwrap();
    let pulse = service
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project.clone(),
            request_id: format!("bench-dispatch-{}-{:?}", fixture.id, arm),
            title: fixture.id.clone(),
            tasks: created
                .iter()
                .map(|task| TaskSelection {
                    task_id: task.id.clone(),
                    expected_revision: task.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let stop = CancellationToken::new();
    let guard = stop.clone().drop_guard();
    let worker_service = service.clone();
    runtime.spawn(async move { worker_service.run_job_worker_until(stop).await });

    let started = Instant::now();
    let mut interventions = 0;
    let mut failure_checkpoints = 0;
    let driver = Driver {
        service: &service,
        project: project.clone(),
        pulse: pulse.clone(),
        timeout: match mode {
            Mode::Fixture => FIXTURE_TIMEOUT,
            Mode::Pilot(_) => PILOT_TIMEOUT,
        },
    };
    let outcome = driver.drive(&mut interventions, &mut failure_checkpoints);
    let wall_ms = started.elapsed().as_millis() as u64;
    drop(guard);
    runtime.shutdown_timeout(Duration::from_secs(5));

    let mut diagnostics = Vec::new();
    let completed = match outcome {
        Outcome::Done => true,
        Outcome::Failed(reason) => {
            diagnostics.push(format!("pulsation failed: {reason}"));
            false
        }
        Outcome::TimedOut(state) => {
            diagnostics.push(format!("timed out: {state}"));
            false
        }
    };
    let oracle_failures = fixture.judge(&workspace);
    let db = Connection::open(service.database_path()).unwrap();
    if !completed {
        // Why: the host's own rejection reasons and the parked executions.
        let rejections: Vec<String> = db
            .prepare("SELECT substr(o.validation_error,1,200) FROM agent_run_outputs o JOIN agent_runs r ON r.id=o.agent_run_id JOIN jobs j ON j.id=r.job_id WHERE j.pulsation_id=?1 AND o.validation_error IS NOT NULL ORDER BY o.created_at")
            .unwrap()
            .query_map([&pulse], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        diagnostics.extend(rejections.into_iter().map(|r| format!("rejected: {r}")));
        let parked: Vec<String> = db
            .prepare("SELECT stage||' '||status||': '||COALESCE(blocked_reason,'') FROM stage_executions WHERE pulsation_id=?1 AND status IN ('parked','failed') ORDER BY created_at")
            .unwrap()
            .query_map([&pulse], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        diagnostics.extend(parked);
        let jobs: Vec<String> = db
            .prepare("SELECT kind||' '||status||': '||COALESCE(substr(last_error,1,200),'') FROM jobs WHERE pulsation_id=?1 AND last_error IS NOT NULL ORDER BY created_at")
            .unwrap()
            .query_map([&pulse], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        diagnostics.extend(jobs);
        let publications: Vec<String> = db
            .prepare("SELECT relative_path||' '||state||' expected='||COALESCE(substr(expected_sha256,1,8),'-')||' desired='||substr(desired_sha256,1,8)||' '||COALESCE(substr(last_error,1,120),'') FROM artifact_publications WHERE pulsation_id=?1 ORDER BY updated_at")
            .unwrap()
            .query_map([&pulse], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        diagnostics.extend(
            publications
                .into_iter()
                .map(|p| format!("publication: {p}")),
        );
    }
    let human_checkpoints: u64 = db
        .query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND kind='artifact_review'",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap();
    let rejected_attempts: u64 = if arm == Arm::A {
        db.query_row(
            "SELECT COUNT(*) FROM agent_runs r JOIN jobs j ON j.id=r.job_id WHERE j.pulsation_id=?1 AND r.status IN ('failed','interrupted')",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap()
    } else {
        db.query_row(
            "SELECT COUNT(*) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND a.state<>'succeeded'",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap()
    };
    let usage: (u64, u64, u64, u64, u64, u64, u64) = db
        .query_row(
            "SELECT COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0),COALESCE(SUM(cache_read_tokens),0),COALESCE(SUM(reasoning_tokens),0),COUNT(*),SUM(quality='partial'),SUM(quality='unavailable') FROM token_usage_records WHERE pulsation_id=?1",
            [&pulse],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<_, Option<u64>>(5)?.unwrap_or(0),
                    r.get::<_, Option<u64>>(6)?.unwrap_or(0),
                ))
            },
        )
        .unwrap();
    let cost: Option<f64> = db
        .query_row(
            "SELECT SUM(estimated_cost_usd) FROM agent_runs r JOIN jobs j ON j.id=r.job_id WHERE j.pulsation_id=?1 AND r.estimated_cost_usd IS NOT NULL",
            [&pulse],
            |r| r.get(0),
        )
        .unwrap();
    let (provider_calls, prompt_chars, prompt_calls, inventory_violations) = match mode {
        Mode::Fixture => {
            let log = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                log.calls,
                log.prompt_chars,
                log.prompt_calls.clone(),
                log.inventory_violations.clone(),
            )
        }
        Mode::Pilot(_) => (
            db.query_row(
                "SELECT COUNT(*) FROM llm_attempts a JOIN llm_route_executions x ON x.id=a.execution_id WHERE x.pulsation_id=?1",
                [&pulse],
                |r| r.get(0),
            )
            .unwrap_or(0),
            0,
            Vec::new(),
            Vec::new(),
        ),
    };
    Observation {
        arm,
        fixture: fixture.id.clone(),
        family: fixture.family,
        held_out: fixture.held_out,
        mode: mode.label().into(),
        completed,
        correct: completed && oracle_failures.is_empty(),
        oracle_failures,
        provider_calls,
        rejected_attempts,
        human_checkpoints,
        failure_checkpoints,
        interventions,
        input_tokens: usage.0,
        output_tokens: usage.1,
        cache_read_tokens: usage.2,
        reasoning_tokens: usage.3,
        usage_records: usage.4,
        usage_partial_records: usage.5,
        usage_unavailable_records: usage.6,
        estimated_cost_micro_usd: cost.map(|usd| (usd * 1_000_000.0).round() as u64),
        prompt_chars,
        prompt_calls,
        inventory_violations,
        wall_ms,
        diagnostics,
    }
}

// ----------------------------------------------------------------- summary

fn percentile(values: &mut [u64], p: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let rank = (p * (values.len() - 1)).div_ceil(100);
    values[rank.min(values.len() - 1)]
}

/// Per-arm aggregation over the development fixtures and, separately, the
/// held-out ones. The primary metric (spend per correct task) is reported
/// only when every observation of the arm carried a provider cost; fixture
/// runs report simulated tokens per correct task instead.
pub fn summarize(observations: &[Observation]) -> Value {
    let mut arms = serde_json::Map::new();
    for arm in Arm::ALL {
        for held_out in [false, true] {
            let group: Vec<&Observation> = observations
                .iter()
                .filter(|o| o.arm == arm && o.held_out == held_out)
                .collect();
            if group.is_empty() {
                continue;
            }
            let tasks: u64 = group.iter().map(|o| task_count(o)).sum();
            let correct_tasks: u64 = group
                .iter()
                .filter(|o| o.correct)
                .map(|o| task_count(o))
                .sum();
            let input: u64 = group.iter().map(|o| o.input_tokens).sum();
            let output: u64 = group.iter().map(|o| o.output_tokens).sum();
            let calls: u64 = group.iter().map(|o| o.provider_calls).sum();
            let rejected: u64 = group.iter().map(|o| o.rejected_attempts).sum();
            let interventions: u64 = group.iter().map(|o| o.interventions).sum();
            let cost: Option<u64> = group
                .iter()
                .map(|o| o.estimated_cost_micro_usd)
                .collect::<Option<Vec<_>>>()
                .map(|costs| costs.iter().sum());
            let mut wall: Vec<u64> = group.iter().map(|o| o.wall_ms).collect();
            let mut prompt: Vec<u64> = group.iter().map(|o| o.prompt_chars).collect();
            let key = format!("{arm:?}{}", if held_out { "_held_out" } else { "" });
            arms.insert(
                key,
                json!({
                    "arm": arm,
                    "label": arm.label(),
                    "held_out": held_out,
                    "fixtures": group.len(),
                    "completed": group.iter().filter(|o| o.completed).count(),
                    "correct": group.iter().filter(|o| o.correct).count(),
                    "tasks": tasks,
                    "correct_tasks": correct_tasks,
                    "completion_rate": if group.is_empty() { 0.0 } else { group.iter().filter(|o| o.completed).count() as f64 / group.len() as f64 },
                    "provider_calls": calls,
                    "rejected_attempts": rejected,
                    "interventions": interventions,
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_tokens": group.iter().map(|o| o.cache_read_tokens).sum::<u64>(),
                    "reasoning_tokens": group.iter().map(|o| o.reasoning_tokens).sum::<u64>(),
                    "tokens_per_correct_task": if correct_tasks == 0 { Value::Null } else { json!((input + output) as f64 / correct_tasks as f64) },
                    "estimated_cost_micro_usd": cost,
                    "spend_per_correct_task_micro_usd": match (cost, correct_tasks) { (Some(cost), n) if n > 0 => json!(cost as f64 / n as f64), _ => Value::Null },
                    "prompt_chars": {"p50": percentile(&mut prompt, 50), "p95": percentile(&mut prompt, 95)},
                    "wall_ms": {"p50": percentile(&mut wall, 50), "p95": percentile(&mut wall, 95)},
                    "usage_unavailable_records": group.iter().map(|o| o.usage_unavailable_records).sum::<u64>(),
                }),
            );
        }
    }
    let mut families = serde_json::Map::new();
    for family in [
        Family::SimpleMaintenance,
        Family::CohesiveMultiFile,
        Family::SeparableMultiTask,
        Family::FailureRecovery,
    ] {
        let mut per_arm = serde_json::Map::new();
        for arm in Arm::ALL {
            let group: Vec<&Observation> = observations
                .iter()
                .filter(|o| o.arm == arm && o.family == family && !o.held_out)
                .collect();
            if group.is_empty() {
                continue;
            }
            per_arm.insert(
                format!("{arm:?}"),
                json!({
                    "correct": group.iter().filter(|o| o.correct).count(),
                    "fixtures": group.len(),
                    "provider_calls": group.iter().map(|o| o.provider_calls).sum::<u64>(),
                    "input_tokens": group.iter().map(|o| o.input_tokens).sum::<u64>(),
                    "output_tokens": group.iter().map(|o| o.output_tokens).sum::<u64>(),
                    "interventions": group.iter().map(|o| o.interventions).sum::<u64>(),
                }),
            );
        }
        families.insert(format!("{family:?}"), Value::Object(per_arm));
    }
    json!({"arms": arms, "families": families})
}

fn task_count(observation: &Observation) -> u64 {
    // Two-task fixtures carry `-and-` or `-pair` in their id; every other
    // fixture is a single task. Kept in the id so the summary needs no
    // fixture lookup.
    if observation.fixture.contains("-and-") || observation.fixture.ends_with("-pair") {
        2
    } else {
        1
    }
}

/// Recipe qualification from the development set: a recipe (arm) is
/// qualified when every development fixture completed correctly with no
/// intervention beyond the injected faults, and the held-out fixtures agree.
pub fn qualification(observations: &[Observation]) -> Value {
    let mut result = serde_json::Map::new();
    for arm in [Arm::B, Arm::C, Arm::D] {
        let dev: Vec<&Observation> = observations
            .iter()
            .filter(|o| o.arm == arm && !o.held_out)
            .collect();
        let held: Vec<&Observation> = observations
            .iter()
            .filter(|o| o.arm == arm && o.held_out)
            .collect();
        let all_correct = !dev.is_empty() && dev.iter().all(|o| o.correct);
        let held_correct = held.iter().all(|o| o.correct);
        let unexpected_interventions = dev
            .iter()
            .filter(|o| o.interventions > u64::from(o.family == Family::FailureRecovery))
            .count();
        let fixture_only = dev.iter().all(|o| o.mode == "fixture");
        result.insert(
            format!("{arm:?}"),
            json!({
                "development_correct": all_correct,
                "held_out_correct": held_correct,
                "unexpected_interventions": unexpected_interventions,
                "orchestration_qualified": all_correct && held_correct && unexpected_interventions == 0,
                "cost_quality_qualified": if fixture_only { Value::Null } else { json!(all_correct && held_correct && unexpected_interventions == 0) },
                "note": if fixture_only { "fixture mode proves orchestration only; no monetary or semantic conclusion" } else { "pilot observations with a real provider" }
            }),
        );
    }
    Value::Object(result)
}

/// The Markdown report next to the JSON, in the style of the brownfield
/// benchmark reports under `quality/`.
pub fn render_markdown(
    meta: &Value,
    summary: &Value,
    qualification: &Value,
    observations: &[Observation],
) -> String {
    let mut out = String::new();
    out.push_str("# Stage composition benchmark\n\n");
    out.push_str(&format!(
        "- generated_at: {}\n- mode: {}\n- code_base: {}\n- fixture_version: {}\n- fixtures_sha256: {}\n- host: {} {}\n\n",
        meta["generated_at"].as_str().unwrap_or(""),
        meta["mode"].as_str().unwrap_or(""),
        meta["code_base"].as_str().unwrap_or(""),
        meta["fixture_version"].as_str().unwrap_or(""),
        meta["fixtures_sha256"].as_str().unwrap_or(""),
        meta["os"].as_str().unwrap_or(""),
        meta["arch"].as_str().unwrap_or("")
    ));
    if meta["mode"] == "fixture" {
        out.push_str("Fixture mode: every worker reply is scripted from the prompt the host rendered; tokens are **simulated** (characters over four) and there is **no monetary conclusion**. What this mode proves is orchestration: calls, retries, checkpoints, publication and the oracle on the published files.\n\n");
    }
    out.push_str("## Arms\n\n| Arm | Set | Fixtures | Completed | Correct | Tasks correct | Calls | Rejected | Interventions | Input tokens | Output tokens | Tokens / correct task | Spend / correct task (µUSD) | Wall p50 ms | Wall p95 ms |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    if let Some(arms) = summary["arms"].as_object() {
        for (_, row) in arms {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                row["label"].as_str().unwrap_or(""),
                if row["held_out"].as_bool().unwrap_or(false) {
                    "held-out"
                } else {
                    "development"
                },
                row["fixtures"],
                row["completed"],
                row["correct"],
                row["correct_tasks"],
                row["tasks"],
                row["provider_calls"],
                row["rejected_attempts"],
                row["interventions"],
                row["input_tokens"],
                row["output_tokens"],
                row["tokens_per_correct_task"]
                    .as_f64()
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "n/a".into()),
                row["spend_per_correct_task_micro_usd"]
                    .as_f64()
                    .map(|v| format!("{v:.0}"))
                    .unwrap_or_else(|| "unknown".into()),
                row["wall_ms"]["p50"],
                row["wall_ms"]["p95"],
            ));
        }
    }
    out.push_str("\n## Families (development set)\n\n| Family | Arm | Correct | Calls | Input tokens | Output tokens | Interventions |\n|---|---|---|---|---|---|---|\n");
    if let Some(families) = summary["families"].as_object() {
        for (family, arms) in families {
            if let Some(arms) = arms.as_object() {
                for (arm, row) in arms {
                    out.push_str(&format!(
                        "| {family} | {arm} | {}/{} | {} | {} | {} | {} |\n",
                        row["correct"],
                        row["fixtures"],
                        row["provider_calls"],
                        row["input_tokens"],
                        row["output_tokens"],
                        row["interventions"]
                    ));
                }
            }
        }
    }
    out.push_str("\n## Qualification per recipe\n\n| Arm | Development correct | Held-out correct | Unexpected interventions | Orchestration qualified | Cost/quality qualified |\n|---|---|---|---|---|---|\n");
    if let Some(rows) = qualification.as_object() {
        for (arm, row) in rows {
            out.push_str(&format!(
                "| {arm} | {} | {} | {} | {} | {} |\n",
                row["development_correct"],
                row["held_out_correct"],
                row["unexpected_interventions"],
                row["orchestration_qualified"],
                row["cost_quality_qualified"]
                    .as_bool()
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "pending (pilot not run)".into())
            ));
        }
    }
    out.push_str("\n## Observations\n\n| Fixture | Family | Arm | Completed | Correct | Calls | Rejected | Interventions | Input | Output | Prompt chars | Wall ms | Diagnostics |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for o in observations {
        out.push_str(&format!(
            "| {}{} | {:?} | {:?} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            o.fixture,
            if o.held_out { " (held-out)" } else { "" },
            o.family,
            o.arm,
            o.completed,
            o.correct,
            o.provider_calls,
            o.rejected_attempts,
            o.interventions,
            o.input_tokens,
            o.output_tokens,
            o.prompt_chars,
            o.wall_ms,
            if o.diagnostics.is_empty() && o.oracle_failures.is_empty() {
                "—".to_owned()
            } else {
                o.diagnostics
                    .iter()
                    .chain(o.oracle_failures.iter())
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
                    .replace('|', "\\|")
            }
        ));
    }
    out.push_str("\n## Limitations\n\n- Fixture mode never calls a model: semantic quality is not measured and tokens are simulated from prompt/reply length; only relative prompt size between arms is meaningful.\n- Pilot mode reports the provider's own usage; a run whose usage is partial or unavailable yields no monetary conclusion.\n- Wall time on the loopback provider is serialized by the single-slot connection: arm D is not expected to be faster here.\n- Oracles are content predicates on published files, independent of the tests the workers authored.\n");
    out
}
