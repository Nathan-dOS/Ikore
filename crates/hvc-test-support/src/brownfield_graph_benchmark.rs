//! RS2: counterbalanced task-context benchmark over the persisted structural graph.

use crate::{
    CoreTestHarness, RecordedRequest, ScriptedHttpServer, ScriptedResponse, pulsation_benchmark,
};
use hvc_core::{
    orchestration::{
        ContentSha256, ReferenceResolution, StageHandoffV1, brownfield::SetBrownfieldConsentRequest,
    },
    persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
    },
    workspace_service::OpenWorkspaceRequest,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::{Duration, Instant},
};

pub const FIXTURE_VERSION: &str = "rs2-relational-tasks-v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TaskContextMode {
    OnDemand,
    ResolvedGraph,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RelationalCase {
    CalleeWithoutExplicitPath,
    RustCallChain,
    TypeScriptAliasReexport,
    ExtendsImplementsContract,
    AmbiguousDoesNotExpand,
    SimpleControl,
}

impl RelationalCase {
    pub const ALL: [Self; 6] = [
        Self::CalleeWithoutExplicitPath,
        Self::RustCallChain,
        Self::TypeScriptAliasReexport,
        Self::ExtendsImplementsContract,
        Self::AmbiguousDoesNotExpand,
        Self::SimpleControl,
    ];

    pub fn description(self) -> &'static str {
        match self {
            Self::CalleeWithoutExplicitPath => {
                "Especifique a mudança ZXQVA no comportamento de handle_request."
            }
            Self::RustCallChain => {
                "Especifique a mudança ZXQVB na política chamada por process_request."
            }
            Self::TypeScriptAliasReexport => {
                "Em alias-consumer, especifique a mudança ZXQVC no binding chamado."
            }
            Self::ExtendsImplementsContract => {
                "Em controller, especifique a mudança ZXQVD no contrato estrutural."
            }
            Self::AmbiguousDoesNotExpand => {
                "Em ambiguous, analise ZXQVE sem escolher um destino incerto."
            }
            Self::SimpleControl => "Em local_change, especifique a mudança local ZXQVF.",
        }
    }

    pub fn seed_path(self) -> &'static str {
        match self {
            Self::CalleeWithoutExplicitPath => "src/api.rs",
            Self::RustCallChain => "src/service.rs",
            Self::TypeScriptAliasReexport => "src/alias-consumer.ts",
            Self::ExtendsImplementsContract => "src/controller.ts",
            Self::AmbiguousDoesNotExpand => "src/ambiguous.ts",
            Self::SimpleControl => "src/control.rs",
        }
    }

    #[cfg(test)]
    fn seed_selector(self) -> &'static str {
        match self {
            Self::CalleeWithoutExplicitPath => "handle_request",
            Self::RustCallChain => "process_request",
            Self::TypeScriptAliasReexport => "alias-consumer",
            Self::ExtendsImplementsContract => "controller",
            Self::AmbiguousDoesNotExpand => "ambiguous",
            Self::SimpleControl => "local_change",
        }
    }

    pub fn target_path(self) -> Option<&'static str> {
        match self {
            Self::CalleeWithoutExplicitPath => Some("src/auth.rs"),
            Self::RustCallChain => Some("src/policy.rs"),
            Self::TypeScriptAliasReexport => Some("src/token.ts"),
            Self::ExtendsImplementsContract => Some("src/contracts.ts"),
            Self::AmbiguousDoesNotExpand => None,
            Self::SimpleControl => Some("src/control.rs"),
        }
    }

    pub fn target_marker(self) -> &'static str {
        match self {
            Self::CalleeWithoutExplicitPath => "RS2_CALLEE_CURRENT",
            Self::RustCallChain => "RS2_POLICY_CURRENT",
            Self::TypeScriptAliasReexport => "RS2_TOKEN_CURRENT",
            Self::ExtendsImplementsContract => "RS2_CONTRACT_CURRENT",
            Self::AmbiguousDoesNotExpand => "RS2_AMBIGUOUS_CURRENT",
            Self::SimpleControl => "RS2_CONTROL_CURRENT",
        }
    }

    pub fn target_symbol(self) -> Option<&'static str> {
        match self {
            Self::CalleeWithoutExplicitPath => Some("validate_token"),
            Self::RustCallChain => Some("apply_rule"),
            Self::TypeScriptAliasReexport => Some("verify"),
            Self::ExtendsImplementsContract => Some("Auditable"),
            Self::AmbiguousDoesNotExpand => None,
            Self::SimpleControl => Some("local_change"),
        }
    }

    fn allowed_paths(self) -> &'static [&'static str] {
        match self {
            Self::CalleeWithoutExplicitPath => &["src/api.rs", "src/auth.rs", "src/lib.rs"],
            Self::RustCallChain => &[
                "src/service.rs",
                "src/policy.rs",
                "src/entry.rs",
                "src/lib.rs",
            ],
            Self::TypeScriptAliasReexport => {
                &["src/alias-consumer.ts", "src/barrel.ts", "src/token.ts"]
            }
            Self::ExtendsImplementsContract => &["src/controller.ts", "src/contracts.ts"],
            Self::AmbiguousDoesNotExpand => &["src/ambiguous.ts"],
            Self::SimpleControl => &["src/control.rs"],
        }
    }

    pub fn is_relational(self) -> bool {
        matches!(
            self,
            Self::CalleeWithoutExplicitPath
                | Self::RustCallChain
                | Self::TypeScriptAliasReexport
                | Self::ExtendsImplementsContract
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RelationalTaskObservation {
    pub specification_checkpoint_ms: f64,
    pub provider_calls: u64,
    pub simulated_input_tokens: u64,
    pub simulated_output_tokens: u64,
    pub clarification_questions: u32,
    pub context_sources: Vec<String>,
    pub graph_resolution_context: Option<String>,
    pub resolved_references: u32,
    pub persisted_ambiguous_edges: u32,
    pub specification_grounded: bool,
    pub quality_score: u32,
    pub quality_max: u32,
    pub quality_evidence: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrownfieldGraphSample {
    pub mode: TaskContextMode,
    pub case: RelationalCase,
    pub iteration: usize,
    pub warmup: bool,
    pub fixture_sha256: String,
    pub resolution_state: Option<String>,
    pub task: RelationalTaskObservation,
}

fn write_fixture(root: &Path) -> String {
    fs::create_dir_all(root.join("src")).expect("RS2 source directory");
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"rs2-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"src/lib.rs\"\n",
        ),
        (
            "src/lib.rs",
            "mod api;\nmod auth;\nmod entry;\nmod policy;\nmod service;\n",
        ),
        (
            "src/api.rs",
            "use crate::auth::validate_token;\npub fn handle_request(expired: bool) -> u16 { validate_token(expired) }\n",
        ),
        (
            "src/auth.rs",
            "pub fn validate_token(expired: bool) -> u16 { /* RS2_CALLEE_CURRENT */ if expired { 403 } else { 200 } }\n",
        ),
        (
            "src/entry.rs",
            "use crate::service::process_request;\npub fn dispatch() -> bool { process_request() }\n",
        ),
        (
            "src/service.rs",
            "use crate::policy::apply_rule;\npub fn process_request() -> bool { apply_rule() }\n",
        ),
        (
            "src/policy.rs",
            "pub fn apply_rule() -> bool { /* RS2_POLICY_CURRENT */ true }\n",
        ),
        (
            "src/alias-consumer.ts",
            "import { verify as check } from './barrel';\nexport function execute(): boolean { return check(); }\n",
        ),
        ("src/barrel.ts", "export { verify } from './token';\n"),
        (
            "src/token.ts",
            "export function verify(): boolean { /* RS2_TOKEN_CURRENT */ return true; }\n",
        ),
        (
            "src/controller.ts",
            "import { BaseController, Auditable } from './contracts';\nexport class Controller extends BaseController implements Auditable { audit(): string { return 'ok'; } }\n",
        ),
        (
            "src/contracts.ts",
            "export class BaseController { static readonly RS2_CONTRACT_CURRENT = true; }\nexport interface Auditable { /* RS2_CONTRACT_CURRENT */ audit(): string; }\n",
        ),
        (
            "src/ambiguous.ts",
            "function choose(): number { return 1; }\nfunction choose(): number { return 2; }\nexport function ambiguous(): number { /* RS2_AMBIGUOUS_CURRENT */ return choose(); }\n",
        ),
        (
            "src/control.rs",
            "pub fn local_change() -> bool { /* RS2_CONTROL_CURRENT */ true }\n",
        ),
    ];
    let mut revisions = Vec::new();
    for (path, content) in files {
        fs::write(root.join(path), content).expect("RS2 fixture file");
        revisions.push((path, ContentSha256::of(content.as_bytes())));
    }
    ContentSha256::of(&serde_json::to_vec(&revisions).unwrap())
        .as_str()
        .into()
}

fn wait_until(label: &str, mut predicate: impl FnMut() -> bool) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if predicate() {
            return Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "RS2 timed out waiting for {label}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn specification_checkpoint(service: &PersistenceService, pulse: &str) -> Instant {
    let db = Connection::open(service.database_path()).unwrap();
    wait_until("Specification checkpoint", || {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.stage='SPECIFICATION' AND p.status='checkpoint' AND c.status='waiting' AND c.kind='artifact_review')",
            [pulse],
            |row| row.get(0),
        )
        .unwrap()
    })
}

fn wait_for_graph(service: &PersistenceService, project_id: &str) -> String {
    let db = Connection::open(service.database_path()).unwrap();
    wait_until("complete structural graph", || {
        db.query_row(
            "SELECT state='complete' FROM brownfield_resolution_state WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )
        .unwrap_or(false)
    });
    db.query_row(
        "SELECT state FROM brownfield_resolution_state WHERE project_id=?1",
        [project_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn relational_stage_response(case: RelationalCase, request: &RecordedRequest) -> ScriptedResponse {
    let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
    let prompt = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let stage = prompt
        .lines()
        .find_map(|line| line.strip_prefix("- stage: "))
        .expect("stage prompt");
    let grounded = prompt.contains(case.target_marker());
    let artifacts = if stage == "Intake" {
        json!([])
    } else {
        let kind = if stage == "SPECIFICATION" {
            "specification"
        } else {
            "stage_note"
        };
        let grounding = if grounded {
            "RS2_GROUNDED"
        } else {
            "RS2_CONTEXT_MISSING"
        };
        json!([{
            "name": stage,
            "kind": kind,
            "relativePath": null,
            "content": format!("# {stage}\n{grounding}: {:?}. No-op; no generated write.", case)
        }])
    };
    let mut output = json!({
        "outputLanguage": "en",
        "summary": format!("{stage} RS2 fixture"),
        "statusUpdate": if stage == "Intake" { "advance" } else { "checkpoint_ready" },
        "contextUsage": [],
        "notes": [],
        "artifacts": artifacts
    });
    if stage == "SPECIFICATION" {
        output["componentProposals"] = json!([]);
    }
    ScriptedResponse::json(
        200,
        json!({
            "choices": [{"message": {"content": output.to_string()}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": prompt.chars().count().div_ceil(4),
                "completion_tokens": 80
            }
        })
        .to_string(),
    )
}

fn observe_task(
    service: &PersistenceService,
    project_id: &str,
    case: RelationalCase,
) -> RelationalTaskObservation {
    let started = Instant::now();
    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project_id.into(),
            feature_name: case.description().into(),
        })
        .unwrap()
        .pulsation
        .id;
    let checkpoint = specification_checkpoint(service, &pulse);
    let db = Connection::open(service.database_path()).unwrap();
    let handoff_json: String = db
        .query_row(
            "SELECT acceptance.handoff_json FROM stage_acceptances acceptance JOIN jobs job ON job.id=acceptance.job_id WHERE job.pulsation_id=?1 AND job.kind='run_stage::SPECIFICATION'",
            [&pulse],
            |row| row.get(0),
        )
        .unwrap();
    let handoff: StageHandoffV1 = serde_json::from_str(&handoff_json).unwrap();
    let context = handoff.context.expect("Specification context");
    let artifact_id: String = db
        .query_row(
            "SELECT artifact.id FROM artifacts artifact JOIN jobs job ON job.id=artifact.producer_job_id WHERE job.pulsation_id=?1 AND job.kind='run_stage::SPECIFICATION' ORDER BY artifact.created_at,artifact.id LIMIT 1",
            [&pulse],
            |row| row.get(0),
        )
        .unwrap();
    let artifact = service.read_artifact_content(&pulse, &artifact_id).unwrap();
    let clarification_questions = db
        .query_row(
            "SELECT COALESCE(SUM(json_array_length(questions_json)),0) FROM specification_question_sets WHERE pulsation_id=?1",
            [&pulse],
            |row| row.get::<_, u32>(0),
        )
        .unwrap();
    let (provider_calls, simulated_input_tokens, simulated_output_tokens): (u64, u64, u64) = db
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(attempt.input_tokens),0),COALESCE(SUM(attempt.output_tokens),0) FROM llm_attempts attempt JOIN llm_route_executions execution ON execution.id=attempt.execution_id WHERE execution.pulsation_id=?1",
            [&pulse],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let context_sources: Vec<_> = context
        .facts
        .sources
        .iter()
        .map(|source| source.relative_path.as_str().to_owned())
        .collect();
    let source_set: BTreeSet<_> = context_sources.iter().map(String::as_str).collect();
    let allowed: BTreeSet<_> = case.allowed_paths().iter().copied().collect();
    let target_present = case
        .target_path()
        .is_none_or(|target| source_set.contains(target));
    let marker_present = context
        .facts
        .snippets
        .iter()
        .any(|snippet| snippet.content.contains(case.target_marker()));
    let symbol_present = case.target_symbol().is_none_or(|target| {
        context
            .facts
            .symbols
            .iter()
            .any(|symbol| symbol.name == target)
    });
    let resolved_references = context
        .facts
        .references
        .iter()
        .filter(|reference| matches!(reference.resolution, ReferenceResolution::Resolved { .. }))
        .count() as u32;
    let persisted_ambiguous_edges = db
        .query_row(
            "SELECT COUNT(*) FROM brownfield_edges WHERE project_id=?1 AND source_path=?2 AND resolution='ambiguous'",
            (project_id, case.seed_path()),
            |row| row.get::<_, u32>(0),
        )
        .unwrap_or(0);
    let no_unsafe_expansion = source_set.is_subset(&allowed)
        && if case == RelationalCase::AmbiguousDoesNotExpand {
            source_set == BTreeSet::from([case.seed_path()]) && resolved_references == 0
        } else {
            true
        };
    let specification_grounded = artifact.contains("RS2_GROUNDED");
    let mut quality_evidence = BTreeMap::new();
    quality_evidence.insert(
        "seed_source_present".into(),
        source_set.contains(case.seed_path()),
    );
    quality_evidence.insert("semantic_target_present".into(), target_present);
    quality_evidence.insert("current_target_bytes_present".into(), marker_present);
    quality_evidence.insert("target_symbol_present".into(), symbol_present);
    quality_evidence.insert("no_unsafe_expansion".into(), no_unsafe_expansion);
    quality_evidence.insert("specification_grounded".into(), specification_grounded);
    quality_evidence.insert(
        "no_clarification_needed".into(),
        clarification_questions == 0,
    );
    let quality_score = quality_evidence.values().filter(|value| **value).count() as u32;
    RelationalTaskObservation {
        specification_checkpoint_ms: checkpoint.duration_since(started).as_secs_f64() * 1000.0,
        provider_calls,
        simulated_input_tokens,
        simulated_output_tokens,
        clarification_questions,
        context_sources,
        graph_resolution_context: context.facts.extractor.resolution_context,
        resolved_references,
        persisted_ambiguous_edges,
        specification_grounded,
        quality_score,
        quality_max: quality_evidence.len() as u32,
        quality_evidence,
    }
}

pub fn run_sample(
    mode: TaskContextMode,
    case: RelationalCase,
    iteration: usize,
    warmup: bool,
) -> BrownfieldGraphSample {
    eprintln!("RS2 fixture {case:?} {mode:?} iteration={iteration} warmup={warmup}");
    let harness = CoreTestHarness::new();
    let fixture_sha256 = write_fixture(harness.workspace_dir());
    let server = ScriptedHttpServer::respond_with(move |request| {
        std::thread::sleep(Duration::from_millis(
            pulsation_benchmark::PROVIDER_DELAY_MS,
        ));
        relational_stage_response(case, request)
    });
    let core = harness.core();
    pulsation_benchmark::configure_loopback(core.persistence(), &server);
    core.persistence()
        .update_preparation_settings(hvc_core::orchestration::PreparationSettings {
            preparation_enabled: false,
            learning_enabled: false,
            ..Default::default()
        })
        .unwrap();
    core.open_workspace(OpenWorkspaceRequest {
        path: harness.workspace_dir().to_string_lossy().into_owned(),
    })
    .unwrap();
    let project_id = core
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: harness.workspace_dir().to_string_lossy().into_owned(),
            preferred_name: Some("RS2 isolated fixture".into()),
        })
        .unwrap()
        .project
        .id;
    // This benchmark drives a full AppCore, so the stage transport is resolved
    // against whatever CLI the host happens to have installed. Pin it.
    pulsation_benchmark::force_api_direct(core.persistence(), &project_id);
    let resolution_state = if mode == TaskContextMode::ResolvedGraph {
        core.set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
            project_id: project_id.clone(),
            consent: true,
        })
        .unwrap();
        Some(wait_for_graph(core.persistence(), &project_id))
    } else {
        None
    };
    let task = observe_task(core.persistence(), &project_id, case);
    assert_eq!(task.provider_calls, 2);
    assert!(task.context_sources.contains(&case.seed_path().to_owned()));
    if mode == TaskContextMode::OnDemand {
        assert!(task.graph_resolution_context.is_none());
    } else {
        assert!(task.graph_resolution_context.is_some());
        assert_eq!(task.quality_score, task.quality_max);
    }
    if mode == TaskContextMode::OnDemand && case.is_relational() {
        assert!(task.quality_score < task.quality_max);
    }
    BrownfieldGraphSample {
        mode,
        case,
        iteration,
        warmup,
        fixture_sha256,
        resolution_state,
        task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases_keep_explicit_seeds_out_of_semantic_targets() {
        for case in RelationalCase::ALL {
            assert!(case.description().contains(case.seed_selector()));
            if case.is_relational() {
                assert_ne!(case.target_path(), Some(case.seed_path()));
                assert!(!case.description().contains(case.target_path().unwrap()));
                assert!(!case.description().contains(case.target_symbol().unwrap()));
            }
        }
    }
}
