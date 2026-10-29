//! Hermetic cost probe of the trust layers (ADR-035 M2): reading and deriving
//! them for a pulsation at the batch limits (64 tasks and criteria, 257
//! granted paths, 200 publications, 64 units with two attempts each, 128 usage
//! records), against the plan's 5 ms target. Run with
//! `cargo run --locked --release -p hvc-core --example layers_benchmark`.
use hvc_core::orchestration::SchemaV1;
use hvc_core::orchestration::composition::recipes::{StagePayloadRecord, UnitPayloadRecord};
use hvc_core::orchestration::composition::{
    CompletedPayloadV1, ExecutorKind, Handle, ParentFenceV1, RECIPE_VERSION, RequirementV1,
    Sha256Hex, SpecificationContractV1, Stage, StageBudgetV1, StageExecutionPlanV1,
    UnitModelResponseV1, Uuid, VerificationMode, WorkUnitSpecV1,
};
use hvc_core::orchestration::mandate::{
    self, BudgetParameters, Enforcement, IssueInputs, IssuedBy, StageOutputLimit, WriteClass,
    WriteGrant,
};
use hvc_core::persistence_service::{
    CreatePulsationRequest, EnsureProjectForWorkspaceRequest, PersistenceService,
};
use rusqlite::params;
use serde_json::json;
use std::time::Instant;

const TASKS: usize = 64;
const GRANTS: usize = 257;
const PUBLICATIONS: usize = 200;
const SAMPLES: usize = 200;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let service = PersistenceService::initialize(&root.path().join("data"))?;
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.to_string_lossy().into_owned(),
            preferred_name: Some("Layers benchmark".into()),
        })?
        .project;
    let pulse = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.id.clone(),
            feature_name: "Layers benchmark".into(),
        })?
        .pulsation;
    let db = rusqlite::Connection::open(service.database_path())?;
    // Only the rows the layers read are built; their parents are not.
    db.pragma_update(None, "foreign_keys", "OFF")?;
    let now = "2026-10-26T12:00:00Z";
    db.execute(
        "UPDATE pulsations SET pipeline_version=4,stage='Code',input_revision=1 WHERE id=?1",
        [&pulse.id],
    )?;

    let mut issued = mandate::derive_issued(&IssueInputs {
        pulsation_id: &pulse.id,
        project_id: &project.id,
        enforcement: Enforcement::EnforcedV4,
        title: "Layers benchmark",
        issued_by: IssuedBy {
            checkpoint_id: "cp-spec".into(),
            stage: "SPECIFICATION".into(),
            state_version: 1,
            input_revision: 1,
        },
        previous: None,
        manifest: None,
        clarification_paths: &[],
        specification: None,
        accepted_artifact_sha256s: &[],
        review_policy_snapshot_sha256: None,
        project_sensitive_sha256: None,
        transports: Vec::new(),
        checks: Vec::new(),
        budget: BudgetParameters {
            stage_output_limits: vec![StageOutputLimit {
                stage: "Code".into(),
                output_tokens: 8_000,
            }],
            context_chars_limit: 200_000,
            extra_invocation_limit: 2,
            per_unit_extra_invocation_limit: 1,
            max_in_flight_units: 4,
        },
    })?;
    issued.scope.grants = (0..GRANTS)
        .map(|index| WriteGrant {
            path: if index % 4 == 0 {
                format!("src/auth/session_{index:03}.rs")
            } else {
                format!("src/module_{index:03}.rs")
            },
            origins: vec![WriteClass::TaskPath],
            unit_key: None,
            amendment_revision: None,
        })
        .collect();
    db.execute(
        "INSERT INTO execution_mandates(id,pulsation_id,revision,cause,enforcement,mandate_json,mandate_sha256,previous_sha256,checkpoint_id,created_at) VALUES('mandate-1',?1,1,'issued','enforced_v4',?2,?3,NULL,'cp-spec',?4)",
        params![
            pulse.id,
            issued.canonical_bytes()?,
            issued.sha256()?.as_str(),
            now
        ],
    )?;

    let handle = |text: String| Handle::new(text).map_err(|error| error.message);
    let specification = StagePayloadRecord {
        schema_version: SchemaV1,
        stage: Stage::Specification,
        plan_sha256: Sha256Hex::of(b"plan"),
        references: Vec::new(),
        units: vec![UnitPayloadRecord {
            logical_unit_key: handle("specify".into())?,
            response: UnitModelResponseV1::Completed {
                executor_kind: ExecutorKind::SpecificationAuthor,
                payload: CompletedPayloadV1::Specification(SpecificationContractV1 {
                    objective: "Benchmark the layers.".into(),
                    exclusions: Vec::new(),
                    decisions: Vec::new(),
                    requirements: (0..TASKS)
                        .map(|index| {
                            Ok(RequirementV1 {
                                task_ref: handle(format!("task_{index}"))?,
                                criterion_ref: handle(format!("crit_{index}_1"))?,
                                requirement: "Behave.".into(),
                                verification_intent: "Run the test.".into(),
                                verification_mode: VerificationMode::ExecutedCheck,
                                required_check_refs: Vec::new(),
                                not_applicable_reason: None,
                            })
                        })
                        .collect::<Result<_, String>>()?,
                    existing_file_refs: Vec::new(),
                    planned_file_refs: Vec::new(),
                    interface_intent: Vec::new(),
                    unit_proposals: Vec::new(),
                }),
            },
        }],
    };
    db.execute(
        "INSERT INTO artifacts(id,pulsation_id,name,kind,content,producer_stage,input_revision,acceptance,created_at,updated_at) VALUES('spec-payload',?1,'payload','stage_payload_v1',?2,'SPECIFICATION',1,'accepted',?3,?3)",
        params![pulse.id, serde_json::to_string(&specification)?, now],
    )?;

    db.execute(
        "INSERT INTO composition_policy_snapshots(id,kind,project_id,sha256,snapshot_json,created_at) VALUES('profile-1','executor_profile',?1,?2,'{\"transport\":\"api\"}',?3)",
        params![project.id, "0".repeat(64), now],
    )?;
    db.execute(
        "INSERT INTO stage_budget_accounts(id,pulsation_id,stage,logical_stage_key,policy_sha256,initial_invocation_limit,extra_invocation_limit,initial_output_token_limit,extra_output_token_limit,initial_prompt_char_limit,extra_prompt_char_limit,created_at,updated_at) VALUES('account-1',?1,'Code','code-1',?2,16,2,64000,8000,800000,100000,?3,?3)",
        params![pulse.id, "0".repeat(64), now],
    )?;
    let uuid = |text: &str| Uuid::new(text.to_owned()).map_err(|error| error.message);
    let execution = Uuid::generate();
    let plan = StageExecutionPlanV1 {
        schema_version: SchemaV1,
        stage_execution_id: execution.clone(),
        budget_account_id: Uuid::generate(),
        parent_fence: ParentFenceV1 {
            project_id: uuid(&project.id)?,
            pulsation_id: uuid(&pulse.id)?,
            input_revision: 1,
            job_id: Uuid::generate(),
            generation: 1,
            state_version: 1,
        },
        stage: Stage::Code,
        recipe_version: RECIPE_VERSION,
        policy_sha256: Sha256Hex::of(b"policy"),
        batch_sha256: Sha256Hex::of(b"batch"),
        approved_contract_sha256: None,
        plan_sha256: Sha256Hex::of(b"plan"),
        scope: Vec::new(),
        references: Vec::new(),
        anchors: Vec::new(),
        check_refs: Vec::new(),
        units: Vec::new(),
        profile_bindings: Vec::new(),
        budget: StageBudgetV1 {
            initial_invocation_limit: 16,
            extra_invocation_limit: 2,
            per_unit_extra_invocation_limit: 1,
            initial_output_token_limit: 64_000,
            extra_output_token_limit: 8_000,
            initial_prompt_character_limit: 800_000,
            extra_prompt_character_limit: 100_000,
            max_in_flight_units: 4,
        },
    };
    db.execute(
        "INSERT INTO stage_executions(id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,recipe_version,policy_snapshot_id,plan_json,plan_sha256,status,created_at,updated_at) VALUES(?1,?2,'job-1',1,1,1,'Code','account-1',1,'profile-1',?3,?4,'running',?5,?5)",
        params![
            execution.as_str(),
            pulse.id,
            serde_json::to_string(&plan)?,
            "0".repeat(64),
            now
        ],
    )?;
    for index in 0..TASKS {
        let unit_id = Uuid::generate();
        let spec = WorkUnitSpecV1 {
            schema_version: SchemaV1,
            unit_id: unit_id.clone(),
            logical_unit_key: handle(format!("unit_{index}"))?,
            stage: Stage::Code,
            executor_kind: ExecutorKind::CodeImplementer,
            task_refs: vec![handle(format!("task_{index}"))?],
            criterion_refs: vec![handle(format!("crit_{index}_1"))?],
            dependencies: Vec::new(),
            input_refs: Vec::new(),
            owned_destination_refs: Vec::new(),
            required_check_refs: Vec::new(),
            objective: "Implement.".into(),
            max_output_tokens: 1_000,
        };
        db.execute(
            "INSERT INTO stage_work_units(id,execution_id,logical_unit_key,ordinal,executor_kind,spec_json,spec_sha256,state,created_at,updated_at) VALUES(?1,?8,?2,?3,'code_implementer',?4,?5,?6,?7,?7)",
            params![
                unit_id.as_str(),
                format!("unit_{index}"),
                index as i64,
                serde_json::to_string(&spec)?,
                "0".repeat(64),
                if index % 2 == 0 { "validated" } else { "running" },
                now,
                execution.as_str()
            ],
        )?;
        for ordinal in 1..=2 {
            let attempt = format!("attempt-{index}-{ordinal}");
            db.execute(
                "INSERT INTO stage_work_unit_attempts(id,unit_id,execution_id,unit_generation,ordinal,reservation_id,executor_kind,profile_snapshot_id,effective_model,observed_model,state,created_at,updated_at) VALUES(?1,?2,?6,1,?3,?4,'code_implementer','profile-1','gpt-4.1-mini','gpt-4.1-mini','succeeded',?5,?5)",
                params![attempt, unit_id.as_str(), ordinal, format!("reservation-{index}-{ordinal}"), now, execution.as_str()],
            )?;
            db.execute(
                "INSERT INTO token_usage_records(source_kind,source_id,unit_key,project_id,transport,provider_id,requested_model,observed_model,input_tokens,output_tokens,total_tokens,quality,started_at,updated_at,pulsation_id,attempt_id) VALUES('llm_attempt',?1,'response',?2,'api','openai','gpt-4.1-mini','gpt-4.1-mini',100,50,150,'complete',?3,?3,?4,?1)",
                params![attempt, project.id, now, pulse.id],
            )?;
        }
    }
    for index in 0..PUBLICATIONS {
        db.execute(
            "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,desired_sha256,state,updated_at) VALUES(?1,?2,?3,?4,?5,'applied',?6)",
            params![
                format!("artifact-{index}"),
                project.id,
                pulse.id,
                format!("src/module_{index:03}.rs"),
                "0".repeat(64),
                now
            ],
        )?;
    }

    let layers = service.pulsation_layers(&pulse.id)?;
    // A standalone call opens a connection, and SQLite loads the schema on
    // its first statement. The detail reads the layers on its own open
    // transaction instead, which is the path the target is about.
    let standalone = measure(|| {
        std::hint::black_box(service.pulsation_layers(&pulse.id)?);
        Ok(())
    })?;
    let detail = rusqlite::Connection::open(service.database_path())?;
    detail.busy_timeout(std::time::Duration::from_secs(5))?;
    detail.query_row("SELECT COUNT(*) FROM pulsations", [], |row| {
        row.get::<_, i64>(0)
    })?;
    let in_transaction = measure(|| {
        let tx = detail.unchecked_transaction()?;
        std::hint::black_box(PersistenceService::pulsation_layers_on(
            &tx, &pulse.id, None,
        )?);
        tx.commit()?;
        Ok(())
    })?;
    println!(
        "{}",
        json!({
            "policy": layers.layers_policy_version,
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "tasks": TASKS,
            "grants": GRANTS,
            "publications": PUBLICATIONS,
            "samples": SAMPLES,
            "in_transaction": in_transaction,
            "standalone_call": standalone,
            "target_us": 5_000,
            "states": layers.layers.iter().map(|layer| format!("{}:{}", layer.kind.as_str(), layer.state.as_str())).collect::<Vec<_>>(),
        })
    );
    Ok(())
}

fn measure(
    mut run: impl FnMut() -> Result<(), Box<dyn std::error::Error>>,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    run()?;
    let mut samples = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = Instant::now();
        run()?;
        samples.push(started.elapsed().as_micros() as u64);
    }
    samples.sort_unstable();
    let percentile = |share: usize| samples[(samples.len() * share / 100).min(samples.len() - 1)];
    Ok(json!({
        "p50_us": percentile(50),
        "p95_us": percentile(95),
        "max_us": samples[samples.len() - 1],
    }))
}
