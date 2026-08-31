//! BF5: auditable structural-mapping and same-task comparison on isolated repositories.
//! The benchmark uses the real AppCore worker, watcher and SQLite persistence with a loopback LLM.

use crate::{CoreTestHarness, ScriptedHttpServer, pulsation_benchmark};
use hvc_core::{
    orchestration::{ContentSha256, brownfield::SetBrownfieldConsentRequest},
    persistence_service::{
        CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
    },
    workspace_service::OpenWorkspaceRequest,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};

pub const FIXTURE_VERSION: &str = "bf5-multilang-v1";
pub const TASK_DESCRIPTION: &str = "Em src/auth.rs, especifique que validate_token deve retornar 401 quando o token estiver expirado.";
const TARGET_PATH: &str = "src/auth.rs";
const CURRENT_MARKER: &str = "BF5_CURRENT";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultMode {
    EmptyVault,
    PartialCoverage,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MappingJobObservation {
    pub cache_hits: u32,
    pub cache_misses: u32,
    pub mapped_sources: u32,
    pub partial_sources: u32,
    pub unsupported_sources: u32,
    pub removed_sources: u32,
    pub coverage_partial: bool,
    pub elapsed_us: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LanguageObservation {
    pub language: String,
    pub sources: u32,
    pub mapped: u32,
    pub partial: u32,
    pub unsupported: u32,
    pub symbols: u32,
    pub references: u32,
    pub parse_elapsed_us: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolutionObservation {
    pub references: u32,
    pub resolved: u32,
    pub unresolved: u32,
    pub external: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MappingObservation {
    pub cold: MappingJobObservation,
    pub warm: MappingJobObservation,
    pub exact_invalidation: MappingJobObservation,
    pub languages: Vec<LanguageObservation>,
    pub resolution: ResolutionObservation,
    pub inventory_known_sources: u32,
    pub eligible_source_denominator: u32,
    pub extracted_valid_sources: u32,
    pub extracted_partial_sources: u32,
    pub extraction_progress_basis_points: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskObservation {
    pub specification_checkpoint_ms: f64,
    pub provider_calls: u64,
    pub simulated_input_tokens: u64,
    pub simulated_output_tokens: u64,
    pub clarification_questions: u32,
    pub specification_quality_score: u32,
    pub specification_quality_max: u32,
    pub quality_evidence: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrownfieldSample {
    pub mode: VaultMode,
    pub iteration: usize,
    pub warmup: bool,
    pub initial_fixture_sha256: String,
    pub task_repository_sha256: String,
    pub knowledge_root_exists: bool,
    pub mapping: Option<MappingObservation>,
    pub task: TaskObservation,
}

fn initial_target() -> &'static str {
    "pub fn validate_token(expired: bool) -> u16 {\n    if expired { 403 } else { 200 }\n}\n"
}

fn current_target() -> &'static str {
    "pub fn validate_token(expired: bool) -> u16 {\n    // BF5_CURRENT: comportamento legado ainda devolve 403.\n    if expired { 403 } else { 200 }\n}\n"
}

fn write_fixture(root: &Path) -> String {
    fs::create_dir_all(root.join("src")).expect("fixture source directory");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"bf5-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest");
    fs::write(root.join(TARGET_PATH), initial_target()).expect("target fixture");
    for index in 0..16 {
        fs::write(
            root.join(format!("src/rust_{index:02}.rs")),
            format!(
                "use crate::auth::validate_token;\npub fn rust_{index:02}() -> u16 {{ validate_token(false) }}\n"
            ),
        )
        .expect("rust corpus");
        fs::write(
            root.join(format!("src/typescript_{index:02}.ts")),
            format!(
                "import {{ validateToken }} from './token';\nexport function typescript{index:02}(): number {{ return validateToken(false); }}\n"
            ),
        )
        .expect("typescript corpus");
        fs::write(
            root.join(format!("src/component_{index:02}.tsx")),
            format!(
                "import React from 'react';\nexport function Component{index:02}() {{ return <span>{{String(checkToken())}}</span>; }}\n"
            ),
        )
        .expect("tsx corpus");
    }
    for index in 0..8 {
        fs::write(
            root.join(format!("src/python_{index:02}.py")),
            format!("def python_{index:02}():\n    return validate_token(False)\n"),
        )
        .expect("unsupported corpus");
    }
    repository_fingerprint(root)
}

fn repository_fingerprint(root: &Path) -> String {
    let mut revisions = vec![];
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let relative = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative.starts_with("knowledge/") {
            continue;
        }
        let bytes = fs::read(entry.path()).expect("fixture bytes");
        revisions.push((relative, ContentSha256::of(&bytes)));
    }
    revisions.sort_by(|a, b| a.0.cmp(&b.0));
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
            "BF5 timed out waiting for {label}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_mapping_job(service: &PersistenceService, job_id: &str) -> MappingJobObservation {
    let db = Connection::open(service.database_path()).unwrap();
    wait_until("brownfield mapping", || {
        db.query_row(
            "SELECT status='done' FROM jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap_or(false)
    });
    let metrics: String = db
        .query_row(
            "SELECT maintenance_metrics_json FROM jobs WHERE id=?1",
            [job_id],
            |row| row.get(0),
        )
        .unwrap();
    let value: Value = serde_json::from_str(&metrics).unwrap();
    MappingJobObservation {
        cache_hits: value["cacheHits"].as_u64().unwrap() as u32,
        cache_misses: value["cacheMisses"].as_u64().unwrap() as u32,
        mapped_sources: value["mappedSources"].as_u64().unwrap() as u32,
        partial_sources: value["partialSources"].as_u64().unwrap() as u32,
        unsupported_sources: value["unsupportedSources"].as_u64().unwrap() as u32,
        removed_sources: value["removedSources"].as_u64().unwrap() as u32,
        coverage_partial: value["coveragePartial"].as_bool().unwrap(),
        elapsed_us: value["elapsedUs"].as_u64().unwrap(),
    }
}

fn latest_mapping_job(service: &PersistenceService, project_id: &str) -> String {
    Connection::open(service.database_path())
        .unwrap()
        .query_row(
            "SELECT id FROM jobs WHERE project_id=?1 AND kind='brownfield_map' ORDER BY rowid DESC LIMIT 1",
            [project_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn mapping_corpus(service: &PersistenceService, project_id: &str) -> Vec<LanguageObservation> {
    let db = Connection::open(service.database_path()).unwrap();
    let mut grouped: BTreeMap<String, LanguageObservation> = BTreeMap::new();
    let mut statement = db
        .prepare(
            "SELECT language,status,parse_elapsed_us,json_array_length(symbols_json),json_array_length(references_json) FROM brownfield_source_cache WHERE project_id=?1 ORDER BY language,relative_path",
        )
        .unwrap();
    let rows = statement
        .query_map([project_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, u32>(4)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (language, status, elapsed, symbols, references) = row.unwrap();
        let item = grouped
            .entry(language.clone())
            .or_insert_with(|| LanguageObservation {
                language,
                sources: 0,
                mapped: 0,
                partial: 0,
                unsupported: 0,
                symbols: 0,
                references: 0,
                parse_elapsed_us: vec![],
            });
        item.sources += 1;
        match status.as_str() {
            "mapped" => item.mapped += 1,
            "partial" => item.partial += 1,
            "unsupported" => item.unsupported += 1,
            other => panic!("unexpected structural status {other}"),
        }
        item.symbols += symbols;
        item.references += references;
        item.parse_elapsed_us.push(elapsed);
    }
    grouped.into_values().collect()
}

fn resolution(service: &PersistenceService, project_id: &str) -> ResolutionObservation {
    let db = Connection::open(service.database_path()).unwrap();
    let (references, resolved, unresolved, external): (u32, u32, u32, u32) = db
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(resolution='resolved'),0),COALESCE(SUM(resolution='unresolved'),0),COALESCE(SUM(resolution='external'),0) FROM brownfield_edges WHERE project_id=?1",
            [project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    ResolutionObservation {
        references,
        resolved,
        unresolved,
        external,
    }
}

fn benchmark_mapping(harness: &CoreTestHarness, project_id: &str) -> MappingObservation {
    let core = harness.core();
    core.set_brownfield_onboarding_consent(SetBrownfieldConsentRequest {
        project_id: project_id.into(),
        consent: true,
    })
    .unwrap();
    let cold_id = latest_mapping_job(core.persistence(), project_id);
    let cold = wait_mapping_job(core.persistence(), &cold_id);

    let warm_id = core
        .persistence()
        .enqueue_brownfield_mapping(project_id)
        .unwrap();
    let warm = wait_mapping_job(core.persistence(), &warm_id);

    fs::write(harness.workspace_dir().join(TARGET_PATH), current_target()).unwrap();
    let current_sha = ContentSha256::of(current_target().as_bytes());
    let db = Connection::open(core.persistence().database_path()).unwrap();
    wait_until("exact watcher invalidation", || {
        db.query_row(
            "SELECT content_sha256=?3 FROM brownfield_source_cache WHERE project_id=?1 AND relative_path=?2",
            (project_id, TARGET_PATH, current_sha.as_str()),
            |row| row.get(0),
        )
        .unwrap_or(false)
    });
    let invalidation_id = latest_mapping_job(core.persistence(), project_id);
    assert_ne!(invalidation_id, warm_id, "watcher must enqueue a fresh job");
    let exact_invalidation = wait_mapping_job(core.persistence(), &invalidation_id);
    let coverage = core
        .get_brownfield_onboarding(project_id)
        .unwrap()
        .coverage
        .unwrap();

    assert_eq!(cold.cache_misses, coverage.inventory_known_sources);
    assert_eq!((warm.cache_hits, warm.cache_misses), (0, 0));
    assert_eq!(exact_invalidation.cache_misses, 1);
    assert_eq!(
        exact_invalidation.mapped_sources
            + exact_invalidation.partial_sources
            + exact_invalidation.unsupported_sources,
        1
    );
    MappingObservation {
        cold,
        warm,
        exact_invalidation,
        languages: mapping_corpus(core.persistence(), project_id),
        resolution: resolution(core.persistence(), project_id),
        inventory_known_sources: coverage.inventory_known_sources,
        eligible_source_denominator: coverage.eligible_source_denominator,
        extracted_valid_sources: coverage.extracted_valid_sources,
        extracted_partial_sources: coverage.extracted_partial_sources,
        extraction_progress_basis_points: coverage.extraction_progress_basis_points,
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

fn benchmark_task(service: &PersistenceService, project_id: &str) -> TaskObservation {
    let started = Instant::now();
    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project_id.into(),
            feature_name: TASK_DESCRIPTION.into(),
        })
        .unwrap()
        .pulsation
        .id;
    let checkpoint = specification_checkpoint(service, &pulse);
    let db = Connection::open(service.database_path()).unwrap();
    let handoff: String = db
        .query_row(
            "SELECT acceptance.handoff_json FROM stage_acceptances acceptance JOIN jobs job ON job.id=acceptance.job_id WHERE job.pulsation_id=?1 AND job.kind='run_stage::SPECIFICATION'",
            [&pulse],
            |row| row.get(0),
        )
        .unwrap();
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
        .or_else(|error| {
            // V6 executions predate pulsation_id; this benchmark has exactly one foreground task.
            if matches!(error, rusqlite::Error::SqliteFailure(_, _)) {
                db.query_row(
                    "SELECT COUNT(*),COALESCE(SUM(input_tokens),0),COALESCE(SUM(output_tokens),0) FROM llm_attempts",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            } else {
                Err(error)
            }
        })
        .unwrap();
    let mut quality_evidence = BTreeMap::new();
    quality_evidence.insert(
        "context_has_target_path".into(),
        handoff.contains(TARGET_PATH),
    );
    quality_evidence.insert(
        "context_has_current_revision".into(),
        handoff.contains(CURRENT_MARKER),
    );
    quality_evidence.insert(
        "context_has_target_symbol".into(),
        handoff.contains("validate_token"),
    );
    quality_evidence.insert("specification_states_401".into(), artifact.contains("401"));
    quality_evidence.insert(
        "no_clarification_needed".into(),
        clarification_questions == 0,
    );
    let specification_quality_score =
        quality_evidence.values().filter(|value| **value).count() as u32;
    TaskObservation {
        specification_checkpoint_ms: checkpoint.duration_since(started).as_secs_f64() * 1000.0,
        provider_calls,
        simulated_input_tokens,
        simulated_output_tokens,
        clarification_questions,
        specification_quality_score,
        specification_quality_max: quality_evidence.len() as u32,
        quality_evidence,
    }
}

pub fn run_sample(mode: VaultMode, iteration: usize, warmup: bool) -> BrownfieldSample {
    eprintln!("BF5 fixture {mode:?} iteration={iteration} warmup={warmup}");
    let harness = CoreTestHarness::new();
    let initial_fixture_sha256 = write_fixture(harness.workspace_dir());
    let server = ScriptedHttpServer::respond_with(|request| {
        std::thread::sleep(Duration::from_millis(
            pulsation_benchmark::PROVIDER_DELAY_MS,
        ));
        pulsation_benchmark::stage_response(request)
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
            preferred_name: Some("BF5 isolated fixture".into()),
        })
        .unwrap()
        .project
        .id;

    let mapping = match mode {
        VaultMode::EmptyVault => {
            fs::write(harness.workspace_dir().join(TARGET_PATH), current_target()).unwrap();
            None
        }
        VaultMode::PartialCoverage => Some(benchmark_mapping(&harness, &project_id)),
    };
    let task_repository_sha256 = repository_fingerprint(harness.workspace_dir());
    let knowledge_root_exists = harness.workspace_dir().join("knowledge").exists();
    assert_eq!(knowledge_root_exists, mode == VaultMode::PartialCoverage);
    let task = benchmark_task(core.persistence(), &project_id);
    assert_eq!(task.provider_calls, 2);
    assert_eq!(
        task.specification_quality_score,
        task.specification_quality_max
    );

    BrownfieldSample {
        mode,
        iteration,
        warmup,
        initial_fixture_sha256,
        task_repository_sha256,
        knowledge_root_exists,
        mapping,
        task,
    }
}

pub fn percentile_u64(values: &[u64], percentile: usize) -> u64 {
    assert!(!values.is_empty() && (1..=100).contains(&percentile));
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * percentile).div_ceil(100) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentile_is_auditable() {
        assert_eq!(percentile_u64(&[4, 1, 3, 2], 50), 2);
        assert_eq!(percentile_u64(&[4, 1, 3, 2], 95), 4);
    }
}
