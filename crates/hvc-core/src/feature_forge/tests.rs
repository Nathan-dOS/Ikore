use super::*;
use crate::orchestration::{ContentSha256, RelativeSourcePath, SourceRevision};
use serde_json::json;

struct Fixture {
    _data: tempfile::TempDir,
    workspace: tempfile::TempDir,
    service: PersistenceService,
    project: String,
}
impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = PersistenceService::initialize(data.path()).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.path().to_string_lossy().into(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        Self {
            _data: data,
            workspace,
            service,
            project,
        }
    }
    fn tasks(&self) -> TaskService {
        TaskService::new(self.service.clone())
    }
    fn forge(&self) -> FeatureForgeService {
        FeatureForgeService::new(self.service.clone())
    }
    fn add(&self, name: &str) -> WorkTask {
        self.tasks()
            .create(CreateTasksRequest {
                project_id: self.project.clone(),
                request_id: id(),
                tasks: vec![draft(name)],
            })
            .unwrap()
            .remove(0)
    }
    fn feature(&self, name: &str) -> Feature {
        let feature = self
            .forge()
            .create(CreateFeatureRequest {
                project_id: self.project.clone(),
                request_id: id(),
                title: name.into(),
                idea: format!("Add {name} with explicit acceptance criteria"),
            })
            .unwrap();
        let brief = FeatureBrief {
            objective: format!("Deliver {name}"),
            audience: "Project developers".into(),
            behavior: "Validate input and display observable results".into(),
            scope: "Bounded validated workflow".into(),
            criteria: vec![criterion()],
            tasks: vec![draft(name)],
            coverage: "unavailable".into(),
            ..Default::default()
        };
        self.forge()
            .revise(&self.project, &feature.id, feature.revision, brief)
            .unwrap()
    }
    fn approved(&self, name: &str) -> Feature {
        let f = self.feature(name);
        self.forge()
            .approve(&self.project, &f.id, f.revision)
            .unwrap();
        self.forge()
            .move_to_wave(&self.project, &f.id, Wave::Current, 0)
            .unwrap()
    }
    fn dispatch(&self, tasks: &[WorkTask]) -> CreatePulsationFromTasksRequest {
        CreatePulsationFromTasksRequest {
            project_id: self.project.clone(),
            request_id: id(),
            title: "Validate the selected acceptance criteria".into(),
            tasks: tasks
                .iter()
                .map(|t| TaskSelection {
                    task_id: t.id.clone(),
                    expected_revision: t.revision,
                })
                .collect(),
        }
    }
}
fn criterion() -> AcceptanceCriterion {
    AcceptanceCriterion {
        id: "ac-1".into(),
        description: "Reject invalid input and preserve valid results".into(),
    }
}

#[test]
fn view_snapshot_keeps_scope_tasks_and_selected_history_project_scoped() {
    let fixture = Fixture::new();
    let feature = fixture.approved("Export workflow");
    let db = fixture.service.connection().unwrap();
    db.execute("INSERT INTO forge_turns(id,feature_id,feature_revision,ordinal,request_id,status,user_message,assistant_message,created_at,updated_at) VALUES('view-turn',?1,?2,1,'view-request','completed','Explain the scope','Scope is ready','now','now')", rusqlite::params![feature.id, feature.revision]).unwrap();
    let view = fixture
        .forge()
        .view_snapshot(&fixture.project, Some(&feature.id))
        .unwrap();
    assert_eq!(view.selected_feature.as_deref(), Some(feature.id.as_str()));
    assert_eq!(view.history.len(), 1);
    assert_eq!(
        view.history[0].feature_revision,
        view.snapshot.features[0].revision
    );
    assert_eq!(view.snapshot.tasks.len(), 1);
    assert_eq!(
        view.snapshot.tasks[0].draft.feature_id.as_deref(),
        Some(feature.id.as_str())
    );
    let fallback = fixture
        .forge()
        .view_snapshot(&fixture.project, Some("removed-feature"))
        .unwrap();
    assert_eq!(fallback.selected_feature, view.selected_feature);
    let other = Fixture::new();
    let absent = fixture
        .forge()
        .view_snapshot(&other.project, Some(&feature.id))
        .unwrap();
    assert!(absent.snapshot.features.is_empty());
    assert!(absent.snapshot.tasks.is_empty());
    assert!(absent.history.is_empty());
    assert!(absent.selected_feature.is_none());
}
fn draft(name: &str) -> TaskDraft {
    TaskDraft {
        title: name.into(),
        kind: TaskKind::Bug,
        description:
            "When the input is invalid, reject it; when valid, preserve the expected result".into(),
        criteria: vec![criterion()],
        dependencies: vec![],
        feature_id: None,
    }
}

#[test]
fn task_capture_is_idempotent_and_does_not_enqueue_work() {
    let f = Fixture::new();
    let request = CreateTasksRequest {
        project_id: f.project.clone(),
        request_id: id(),
        tasks: vec![draft("Fix parsing"), draft("Fix rendering")],
    };
    let first = f.tasks().create(request.clone()).unwrap();
    let second = f.tasks().create(request.clone()).unwrap();
    assert_eq!(first, second);
    let mut changed = request;
    changed.tasks[0].title = "Different request".into();
    assert!(f.tasks().create(changed).is_err());
    let count: i64 = f
        .service
        .connection()
        .unwrap()
        .query_row("SELECT count(*) FROM pulsations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn feature_approval_is_explicit_and_does_not_dispatch() {
    let f = Fixture::new();
    let feature = f.feature("Export records");
    assert!(f.tasks().list(&f.project).unwrap().is_empty());
    f.forge()
        .approve(&f.project, &feature.id, feature.revision)
        .unwrap();
    f.forge()
        .approve(&f.project, &feature.id, feature.revision)
        .unwrap();
    assert_eq!(f.tasks().list(&f.project).unwrap().len(), 1);
    assert_eq!(
        f.service
            .list_pulsations(ListPulsationsRequest {
                project_id: f.project.clone()
            })
            .unwrap()
            .pulsations
            .len(),
        0
    );
    assert!(
        f.forge()
            .complete(&f.project, &feature.id, feature.revision)
            .is_err()
    );
}
#[test]
fn unresolved_decisions_and_changed_sources_block_approval() {
    let f = Fixture::new();
    let feature = f.feature("Validate input");
    let mut brief = feature.brief.clone();
    brief
        .pending_decisions
        .push("Choose the rejection behavior".into());
    let revised = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief)
        .unwrap();
    assert!(
        f.forge()
            .approve(&f.project, &feature.id, revised.revision)
            .is_err()
    );
    let mut brief = revised.brief;
    brief.pending_decisions.clear();
    std::fs::write(f.workspace.path().join("main.rs"), "fn before() {}\n").unwrap();
    brief.sources.push(SourceRevision {
        project_id: f.project.clone(),
        relative_path: RelativeSourcePath::new("main.rs").unwrap(),
        content_sha256: ContentSha256::of(b"fn before() {}\n"),
    });
    let revised = f
        .forge()
        .revise(&f.project, &feature.id, revised.revision, brief)
        .unwrap();
    std::fs::write(f.workspace.path().join("main.rs"), "fn after() {}\n").unwrap();
    assert!(
        f.forge()
            .approve(&f.project, &feature.id, revised.revision)
            .is_err()
    );
}
#[test]
fn task_dependencies_validate_cycles_and_dispatch_membership() {
    let f = Fixture::new();
    let a = f.add("Validate parser");
    let b = f.add("Validate renderer");
    let mut bd = b.draft.clone();
    bd.dependencies = vec![a.id.clone()];
    let b = f.tasks().revise(&f.project, &b.id, b.revision, bd).unwrap();
    let mut ad = a.draft.clone();
    ad.dependencies = vec![b.id.clone()];
    assert!(f.tasks().revise(&f.project, &a.id, a.revision, ad).is_err());
    assert!(
        f.tasks()
            .dispatch(f.dispatch(std::slice::from_ref(&b)))
            .is_err()
    );
    let pulse = f.tasks().dispatch(f.dispatch(&[b, a.clone()])).unwrap();
    let manifest =
        crate::feature_forge::pipeline::manifest(&f.service.connection().unwrap(), &pulse.id)
            .unwrap()
            .unwrap();
    assert_eq!(manifest.tasks[0].id, a.id);
}
#[test]
fn feature_dependencies_and_wave_capacity_are_enforced() {
    let f = Fixture::new();
    let a = f.approved("Foundation");
    f.forge()
        .move_to_wave(&f.project, &a.id, Wave::Next, 0)
        .unwrap();
    let b = f.feature("Interface");
    f.forge().approve(&f.project, &b.id, b.revision).unwrap();
    f.forge()
        .set_dependency(&f.project, &b.id, &a.id, Some("Uses foundation contracts"))
        .unwrap();
    assert!(
        f.forge()
            .set_dependency(&f.project, &a.id, &b.id, Some("Cycle"))
            .is_err()
    );
    assert!(
        f.forge()
            .move_to_wave(&f.project, &b.id, Wave::Current, 0)
            .is_err()
    );
    f.forge()
        .set_dependency(&f.project, &b.id, &a.id, None)
        .unwrap();
    f.forge()
        .move_to_wave(&f.project, &a.id, Wave::Current, 0)
        .unwrap();
    f.forge().set_wave_limit(&f.project, 1).unwrap();
    assert!(
        f.forge()
            .move_to_wave(&f.project, &b.id, Wave::Current, 0)
            .is_err()
    );
    f.forge()
        .move_to_wave(&f.project, &b.id, Wave::Next, 0)
        .unwrap();
    assert!(f.forge().set_wave_limit(&f.project, 0).is_err());
}
#[test]
fn dispatch_freezes_mixed_sources_and_rejects_duplicate_reservations() {
    let f = Fixture::new();
    let a = f.approved("Authentication");
    let b = f.approved("Export");
    f.add("Fix validation");
    let tasks = f.tasks().list(&f.project).unwrap();
    let request = f.dispatch(&tasks);
    let p = f.tasks().dispatch(request.clone()).unwrap();
    let replay = f.tasks().dispatch(request).unwrap();
    assert_eq!(p.id, replay.id);
    assert!(f.tasks().dispatch(f.dispatch(&tasks)).is_err());
    assert!(
        f.tasks()
            .revise(
                &f.project,
                &tasks[0].id,
                tasks[0].revision,
                tasks[0].draft.clone()
            )
            .is_err()
    );
    let manifest =
        crate::feature_forge::pipeline::manifest(&f.service.connection().unwrap(), &p.id)
            .unwrap()
            .unwrap();
    assert_eq!(manifest.features.len(), 2);
    assert_eq!(manifest.tasks.len(), 3);
    assert!(manifest.features.iter().any(|x| x.id == a.id));
    assert!(manifest.features.iter().any(|x| x.id == b.id));
}
#[test]
fn foreign_project_ids_are_rejected() {
    let f = Fixture::new();
    let task = f.add("Fix parser");
    let other = tempfile::tempdir().unwrap();
    let project = f
        .service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: other.path().to_string_lossy().into(),
            preferred_name: None,
        })
        .unwrap()
        .project
        .id;
    let mut request = f.dispatch(&[task]);
    request.project_id = project;
    assert!(f.tasks().dispatch(request).is_err());
}
#[test]
fn complete_six_stage_batch_requires_approved_integration() {
    let f = Fixture::new();
    let feature = f.approved("Validate parser");
    f.add("Fix rendering");
    let tasks = f.tasks().list(&f.project).unwrap();
    let pulse = f.tasks().dispatch(f.dispatch(&tasks)).unwrap();
    let db = f.service.connection().unwrap();
    db.execute(
        "UPDATE pulsation_preparation SET review_state='not_required' WHERE pulsation_id=?1",
        [&pulse.id],
    )
    .unwrap();
    for (stage, name, _) in crate::orchestration::catalog::STAGES {
        let (job, _) = f.service.claim_next_pending_job().unwrap().expect(name);
        let tx = db.unchecked_transaction().unwrap();
        let current = PersistenceService::find_pulsation_by_id(&tx, &pulse.id)
            .unwrap()
            .unwrap();
        PersistenceService::transition_pulsation(
            &tx,
            &current,
            PulsationStatus::Running,
            stage,
            "test_start",
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        let run = f
            .service
            .start_agent_run_for_job(&job, &AgentRuntimeConfig::default())
            .unwrap();
        let fence = PersistenceService::execution_fence(&db, &job.id).unwrap();
        let handoff = f.service.load_stage_handoff(&fence, stage, 80_000).unwrap();
        let evidence=tasks.iter().map(|t|json!({"taskId":t.id,"criterionIds":["ac-1"],"artifactPositions":[0],"summary":"Reviewed acceptance criteria; no command execution claimed"})).collect::<Vec<_>>();
        let structured = json!({"outputLanguage":"en","summary":"Traceable batch result","statusUpdate":if matches!(stage,PulsationStage::Intake|PulsationStage::Code){"advance"}else{"checkpoint_ready"},"notes":[],"contextUsage":[],"componentProposals":[],"artifacts":[{"name":"review","kind":"stage_output","relativePath":null,"content":"# Reviewed task batch\nEvidence is ready for human review."}],"taskEvidence":evidence});
        let result = StageExecutionResult {
            provider_used: "fixture".into(),
            model_used: "fixture".into(),
            route_id: None,
            connection_id: None,
            llm_execution_id: None,
            input_tokens: None,
            output_tokens: None,
            estimated_cost_usd: None,
            request_payload: json!({}),
            response_payload: json!({}),
            raw_output: structured.to_string(),
            structured_output: structured,
            schema_json: json!({}),
            tool_trace: json!([]),
            context_trace: Default::default(),
            artifacts: vec![crate::agent_runtime::GeneratedArtifact {
                name: "review".into(),
                kind: "stage_output".into(),
                relative_path: None,
                content: "# Reviewed task batch\nEvidence is ready for human review.".into(),
            }],
        };
        if stage == PulsationStage::Test {
            let mut bad = result.clone();
            bad.structured_output["taskEvidence"][0]["criterionIds"] = json!([]);
            assert!(
                f.service
                    .accept_stage_result(&job, &run, &handoff, &bad, f.workspace.path())
                    .is_err()
            );
        }
        f.service
            .accept_stage_result(&job, &run, &handoff, &result, f.workspace.path())
            .unwrap();
        f.service
            .release_runtime_slot(&PersistenceService::job_slot_token(&job))
            .unwrap();
        f.service.unregister_job_cancellation(&job.id);
        assert!(
            f.tasks()
                .list(&f.project)
                .unwrap()
                .iter()
                .all(|t| t.state != "done")
        );
        if stage.requires_checkpoint() {
            let checkpoint = f.service.get_hitl_checkpoint(&pulse.id).unwrap();
            f.service
                .apply_hitl_decision(ApplyHitlDecisionRequest {
                    pulsation_id: pulse.id.clone(),
                    checkpoint_id: checkpoint.checkpoint_id,
                    expected_state_version: checkpoint.state_version,
                    decision: HitlDecision::Approve,
                    feedback: None,
                })
                .unwrap();
        }
        f.service.complete_integrations().unwrap();
    }
    assert!(
        f.tasks()
            .list(&f.project)
            .unwrap()
            .iter()
            .all(|t| t.state == "done")
    );
    assert_eq!(
        f.forge().get(&f.project, &feature.id).unwrap().state,
        FeatureState::ReadyForReview
    );
    f.forge()
        .complete(&f.project, &feature.id, feature.revision)
        .unwrap();
    let snapshot = f.forge().snapshot(&f.project).unwrap();
    assert_eq!(
        snapshot.integration_evidence.len(),
        snapshot.tasks.iter().filter(|t| t.state == "done").count()
    );
}

/// Field incident, 2026-09-08: a free OpenCode model produced ONE artifact
/// for a SPECIFICATION stage but referenced `artifactPositions: [0, 1]` in
/// taskEvidence - apparently padding the position count to match its two
/// `criterionIds`, rather than counting what it actually returned. The host
/// correctly rejected it; this locks that rejection and its diagnostic.
#[test]
fn evidence_referencing_an_artifact_position_the_model_never_produced_is_rejected_with_a_diagnostic()
 {
    let f = Fixture::new();
    f.approved("Lightweight local browser architecture");
    let tasks = f.tasks().list(&f.project).unwrap();
    let pulse = f.tasks().dispatch(f.dispatch(&tasks)).unwrap();
    let db = f.service.connection().unwrap();
    db.execute(
        "UPDATE pulsation_preparation SET review_state='not_required' WHERE pulsation_id=?1",
        [&pulse.id],
    )
    .unwrap();
    let (job, _) = f
        .service
        .claim_next_pending_job()
        .unwrap()
        .expect("Intake job");
    let tx = db.unchecked_transaction().unwrap();
    let current = PersistenceService::find_pulsation_by_id(&tx, &pulse.id)
        .unwrap()
        .unwrap();
    PersistenceService::transition_pulsation(
        &tx,
        &current,
        PulsationStatus::Running,
        PulsationStage::Intake,
        "test_start",
        None,
    )
    .unwrap();
    tx.commit().unwrap();
    let run = f
        .service
        .start_agent_run_for_job(&job, &AgentRuntimeConfig::default())
        .unwrap();
    let fence = PersistenceService::execution_fence(&db, &job.id).unwrap();
    let handoff = f
        .service
        .load_stage_handoff(&fence, PulsationStage::Intake, 80_000)
        .unwrap();

    // Two criteria, one artifact - the model padded artifactPositions to [0, 1]
    // instead of leaving it at [0].
    let task = &tasks[0];
    let evidence = json!([{
        "taskId": task.id,
        "criterionIds": ["ac-1"],
        "artifactPositions": [0, 1],
        "summary": "Documented the architecture decision for this task."
    }]);
    let structured = json!({
        "outputLanguage": "en",
        "summary": "Intake result",
        "statusUpdate": "advance",
        "notes": [],
        "contextUsage": [],
        "artifacts": [{
            "name": "decision",
            "kind": "stage_note",
            "relativePath": null,
            "content": "# Decision\nOnly one artifact was actually produced."
        }],
        "taskEvidence": evidence
    });
    let result = StageExecutionResult {
        provider_used: "fixture".into(),
        model_used: "fixture".into(),
        route_id: None,
        connection_id: None,
        llm_execution_id: None,
        input_tokens: None,
        output_tokens: None,
        estimated_cost_usd: None,
        request_payload: json!({}),
        response_payload: json!({}),
        raw_output: structured.to_string(),
        structured_output: structured,
        schema_json: json!({}),
        tool_trace: json!([]),
        context_trace: Default::default(),
        artifacts: vec![crate::agent_runtime::GeneratedArtifact {
            name: "decision".into(),
            kind: "stage_note".into(),
            relative_path: None,
            content: "# Decision\nOnly one artifact was actually produced.".into(),
        }],
    };

    let error = f
        .service
        .accept_stage_result(&job, &run, &handoff, &result, f.workspace.path())
        .expect_err("an out-of-range artifactPositions must be rejected");
    // Model-output-caused, and therefore worth a corrective retry - not the
    // generic Validation a host-side check (e.g. a missing pulsation) would
    // return, which `should_retry` deliberately never retries.
    assert!(
        matches!(error, PersistenceServiceError::ModelOutputInvalid(_)),
        "accept_evidence rejections must be ModelOutputInvalid, got: {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains(&task.id),
        "diagnostic should name the task, got: {message}"
    );
    assert!(
        message.contains('1') && message.contains("does not exist"),
        "diagnostic should name the missing index, got: {message}"
    );
    assert!(
        message.contains("independent of criterionIds"),
        "diagnostic should warn against padding positions to match criteria, got: {message}"
    );
}

#[test]
fn conversion_preserves_original_request_without_duplicate_work() {
    let f = Fixture::new();
    let task = f.add("Create a new feature");
    let key = id();
    let feature = f
        .tasks()
        .convert_to_feature(
            &f.project,
            &task.id,
            1,
            &key,
            "New workflow",
            "Explore a richer workflow",
        )
        .unwrap();
    assert!(feature.original_idea.contains(&task.draft.description));
    assert_eq!(f.tasks().list(&f.project).unwrap()[0].state, "superseded");
    assert_eq!(
        feature.id,
        f.tasks()
            .convert_to_feature(
                &f.project,
                &task.id,
                1,
                &key,
                "New workflow",
                "Explore a richer workflow"
            )
            .unwrap()
            .id
    );
    assert!(f.tasks().dispatch(f.dispatch(&[task])).is_err());
}
#[test]
fn failed_batch_requires_reconciliation_before_retry() {
    let f = Fixture::new();
    let task = f.add("Validate parser");
    let pulse = f
        .tasks()
        .dispatch(f.dispatch(std::slice::from_ref(&task)))
        .unwrap();
    let db = f.service.connection().unwrap();
    db.execute(
        "UPDATE pulsations SET status='failed' WHERE id=?1",
        [&pulse.id],
    )
    .unwrap();
    db.execute(
        "UPDATE project_pipeline_state SET base_state='needs_review' WHERE project_id=?1",
        [&f.project],
    )
    .unwrap();
    assert!(
        f.tasks()
            .dispatch(f.dispatch(std::slice::from_ref(&task)))
            .is_err()
    );
    db.execute("UPDATE project_pipeline_state SET base_state='ready',active_pulsation_id=NULL WHERE project_id=?1",[&f.project]).unwrap();
    let retried = f.tasks().dispatch(f.dispatch(&[task])).unwrap();
    assert_ne!(pulse.id, retried.id);
}
#[test]
fn plan_publication_preserves_human_content_and_replays_after_restart() {
    let f = Fixture::new();
    let feature = f.feature("Validate parser");
    let path = f.workspace.path().join(format!(
        "knowledge/obsidian/product/features/{}/revision-{}.md",
        feature.id, feature.revision
    ));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "Human owned content").unwrap();
    f.forge()
        .approve(&f.project, &feature.id, feature.revision)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "Human owned content"
    );
    assert_eq!(f.forge().publication_issues(&f.project).unwrap().len(), 1);
    let service = PersistenceService::initialize(f._data.path()).unwrap();
    assert_eq!(
        FeatureForgeService::new(service)
            .get(&f.project, &feature.id)
            .unwrap()
            .approved_revision,
        Some(feature.revision)
    );
}

struct PlanningGateway {
    response: String,
    calls: std::sync::atomic::AtomicUsize,
    entered: tokio::sync::Semaphore,
    release: tokio::sync::Semaphore,
}
impl crate::llm::LlmGateway for PlanningGateway {
    fn execute<'a>(
        &'a self,
        request: &'a crate::llm::LlmRequest,
        cancellation: &'a CancellationToken,
    ) -> crate::llm::GatewayFuture<'a> {
        Box::pin(async move {
            use crate::llm::*;
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.add_permits(1);
            tokio::select! {_=cancellation.cancelled()=>return Err(LlmError::new(LlmErrorKind::Cancelled,"Cancelled planning")),permit=self.release.acquire()=>permit.unwrap().forget()}
            assert!(request.pulsation_id.is_none());
            assert!(request.job_id.is_none());
            Ok(LlmResponse {
                execution_id: request.execution_id.clone(),
                generation_id: request.generation_id.clone(),
                route_id: RouteId::new("fixture"),
                connection_id: ConnectionId::new("fixture"),
                provider_id: ProviderId::new("fixture"),
                model: "fixture".into(),
                request_payload: json!({}),
                response_payload: json!({}),
                content: if request.target.agent_role() == Some(AgentRole::IntakeRouter) {
                    r#"{"summary":"Observed export behavior","risks":[],"existingComponents":["Export function"]}"#.into()
                } else {
                    self.response.clone()
                },
                usage: LlmUsage::default(),
                finish_reason: None,
                estimated_cost_usd: None,
            })
        })
    }
}
fn planning_runtime(
    f: &Fixture,
    feature: &Feature,
) -> (Arc<FeatureAgentRuntime>, Arc<PlanningGateway>) {
    let mut brief = feature.brief.clone();
    brief.pending_decisions = vec!["Choose export format".into()];
    let response = FeatureAgentResponse {
        output_language: "en".into(),
        message: "Vamos definir o formato da exportação.".into(),
        questions: vec!["Qual formato você prefere?".into()],
        brief,
        evaluation: Default::default(),
        suggested_dependencies: vec![],
    };
    let gateway = Arc::new(PlanningGateway {
        response: json(&response).unwrap(),
        calls: 0.into(),
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let mut runtime = FeatureAgentRuntime::new(
        f.service.clone(),
        Arc::new(crate::AppEventBus::default()),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    runtime.gateway_override = Some(gateway.clone());
    (Arc::new(runtime), gateway)
}
#[tokio::test]
async fn feature_conversation_is_durable_idempotent_and_separate_from_execution() {
    let f = Fixture::new();
    let feature = f.feature("Export records");
    let (runtime, gateway) = planning_runtime(&f, &feature);
    let key = id();
    let turn = runtime
        .start(
            &f.project,
            &feature.id,
            feature.revision,
            &key,
            "Vamos especificar",
        )
        .unwrap();
    gateway.entered.acquire().await.unwrap().forget();
    assert_eq!(
        runtime
            .start(
                &f.project,
                &feature.id,
                feature.revision,
                &key,
                "Vamos especificar"
            )
            .unwrap(),
        turn
    );
    gateway.release.add_permits(1);
    runtime.wait_for_idle().await;
    let history = runtime.history(&f.project, &feature.id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, "completed", "{:?}", history[0].error);
    assert!(
        history[0]
            .assistant_message
            .as_ref()
            .unwrap()
            .contains("Qual formato")
    );
    assert_eq!(gateway.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(f.tasks().list(&f.project).unwrap().is_empty());
    let updated = f.forge().get(&f.project, &feature.id).unwrap();
    assert_eq!(updated.revision, feature.revision + 1);
    assert!(
        f.forge()
            .approve(&f.project, &feature.id, updated.revision)
            .is_err()
    );
}
#[tokio::test]
async fn cancelled_feature_response_never_updates_the_brief() {
    let f = Fixture::new();
    let feature = f.feature("Export records");
    let (runtime, gateway) = planning_runtime(&f, &feature);
    let turn = runtime
        .start(
            &f.project,
            &feature.id,
            feature.revision,
            &id(),
            "Discuss the scope",
        )
        .unwrap();
    gateway.entered.acquire().await.unwrap().forget();
    assert!(runtime.cancel(&turn));
    runtime.wait_for_idle().await;
    assert_eq!(
        runtime.history(&f.project, &feature.id).unwrap()[0].status,
        "cancelled"
    );
    assert_eq!(
        f.forge().get(&f.project, &feature.id).unwrap().revision,
        feature.revision
    );
}

#[test]
fn approved_task_proposals_resolve_dependencies_and_rollback_cycles() {
    let f = Fixture::new();
    let feature = f.feature("Build export workflow");
    let mut brief = feature.brief.clone();
    brief.tasks = vec![draft("Export interface"), draft("Export foundation")];
    brief.tasks[0].dependencies = vec!["proposal:2".into()];
    let feature = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief.clone())
        .unwrap();
    f.forge()
        .approve(&f.project, &feature.id, feature.revision)
        .unwrap();
    let tasks = f.tasks().list(&f.project).unwrap();
    let dependent = tasks
        .iter()
        .find(|t| t.draft.title == "Export interface")
        .unwrap();
    let prerequisite = tasks
        .iter()
        .find(|t| t.draft.title == "Export foundation")
        .unwrap();
    assert_eq!(dependent.draft.dependencies, vec![prerequisite.id.clone()]);
    brief.tasks[1].dependencies = vec!["proposal:1".into()];
    let feature = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief)
        .unwrap();
    assert!(
        f.forge()
            .approve(&f.project, &feature.id, feature.revision)
            .is_err()
    );
    assert_eq!(
        f.tasks().list(&f.project).unwrap(),
        tasks,
        "Approval must roll back every task and superseding change"
    );
}

#[test]
fn stale_dispatch_and_tampered_manifest_are_rejected() {
    let f = Fixture::new();
    let task = f.add("Validate input");
    let stale = f.dispatch(std::slice::from_ref(&task));
    let revised = f
        .tasks()
        .revise(
            &f.project,
            &task.id,
            task.revision,
            draft("Validate query input"),
        )
        .unwrap();
    assert!(f.tasks().dispatch(stale).is_err());
    let pulse = f.tasks().dispatch(f.dispatch(&[revised])).unwrap();
    let db = f.service.connection().unwrap();
    db.execute(
        "UPDATE pulsation_batches SET manifest_json='{}' WHERE pulsation_id=?1",
        [&pulse.id],
    )
    .unwrap();
    assert!(pipeline::manifest(&db, &pulse.id).is_err());
}

#[test]
fn restarting_recovers_interrupted_discovery_without_creating_a_second_turn() {
    let f = Fixture::new();
    let feature = f.feature("Plan export");
    let db = f.service.connection().unwrap();
    db.execute("INSERT INTO forge_turns(id,feature_id,feature_revision,ordinal,request_id,status,user_message,created_at,updated_at) VALUES('interrupted',?1,?2,1,'request','running','Discuss export','now','now')",params![feature.id,feature.revision]).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let agent = Arc::new(
        FeatureAgentRuntime::new(
            f.service.clone(),
            Arc::new(crate::AppEventBus::default()),
            runtime.handle().clone(),
        )
        .unwrap(),
    );
    assert_eq!(
        agent.history(&f.project, &feature.id).unwrap()[0].status,
        "interrupted"
    );
    assert_eq!(
        agent
            .start(
                &f.project,
                &feature.id,
                feature.revision,
                "request",
                "Discuss export"
            )
            .unwrap(),
        "interrupted"
    );
    assert_eq!(agent.history(&f.project, &feature.id).unwrap().len(), 1);
}

#[tokio::test]
async fn existing_repository_intake_cache_is_reused_only_while_sources_match() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.workspace.path().join("src")).unwrap();
    let source = f.workspace.path().join("src/export.rs");
    std::fs::write(&source, "pub fn export_records() -> bool { true }\n").unwrap();
    let feature = f.feature("Export records");
    let (runtime, gateway) = planning_runtime(&f, &feature);
    gateway.release.add_permits(10);
    for round in 0..3 {
        if round == 2 {
            std::fs::write(&source, "pub fn export_records() -> bool { false }\n").unwrap();
        }
        let feature = f.forge().get(&f.project, &feature.id).unwrap();
        runtime
            .start(
                &f.project,
                &feature.id,
                feature.revision,
                &id(),
                "Discuss export records",
            )
            .unwrap();
        runtime.wait_for_idle().await;
        let history = runtime.history(&f.project, &feature.id).unwrap();
        let last = history.last().unwrap();
        assert_eq!(last.status, "completed", "{:?}", last.error);
        assert_eq!(
            gateway.calls.load(std::sync::atomic::Ordering::SeqCst),
            [2, 3, 5][round]
        );
    }
    assert!(
        !f.forge()
            .get(&f.project, &feature.id)
            .unwrap()
            .brief
            .sources
            .is_empty()
    );
}

#[tokio::test]
async fn feature_agent_rejects_non_english_artifacts_and_excess_questions() {
    for malformed in ["language", "questions"] {
        let f = Fixture::new();
        let feature = f.feature("Export records");
        let (mut runtime, mut gateway) = planning_runtime(&f, &feature);
        let engine = Arc::get_mut(&mut runtime).unwrap();
        engine.gateway_override = None;
        let provider = Arc::get_mut(&mut gateway).unwrap();
        let mut response: serde_json::Value = serde_json::from_str(&provider.response).unwrap();
        if malformed == "language" {
            response["brief"]["objective"] = json!(
                "Esta funcionalidade deve permitir que o usuário faça a exportação dos registros"
            );
        } else {
            response["questions"] = json!(["One?", "Two?", "Three?", "Four?"]);
        }
        provider.response = response.to_string();
        provider.release.add_permits(1);
        engine.gateway_override = Some(gateway.clone());
        runtime
            .start(
                &f.project,
                &feature.id,
                feature.revision,
                &id(),
                "Discuss export",
            )
            .unwrap();
        runtime.wait_for_idle().await;
        assert_eq!(
            runtime.history(&f.project, &feature.id).unwrap()[0].status,
            "failed"
        );
        assert_eq!(
            f.forge().get(&f.project, &feature.id).unwrap().revision,
            feature.revision
        );
    }
}

#[test]
fn navigation_restores_the_feature_and_forge_section_without_queueing_work() {
    let f = Fixture::new();
    let feature = f.feature("Export records");
    let navigation = ForgeNavigation {
        selected_feature_id: Some(feature.id),
        section: 2,
    };
    f.forge().save_navigation(&f.project, &navigation).unwrap();
    let reopened = PersistenceService::initialize(f._data.path()).unwrap();
    assert_eq!(
        FeatureForgeService::new(reopened)
            .snapshot(&f.project)
            .unwrap()
            .navigation,
        navigation
    );
    assert!(f.tasks().list(&f.project).unwrap().is_empty());
}

#[tokio::test]
async fn shutdown_closes_discovery_admission_before_waiting_for_turns() {
    let f = Fixture::new();
    let feature = f.feature("Export records");
    let (runtime, _) = planning_runtime(&f, &feature);
    runtime.cancel_all();
    assert!(
        runtime
            .start(
                &f.project,
                &feature.id,
                feature.revision,
                &id(),
                "Discuss export"
            )
            .is_err()
    );
    runtime.wait_for_idle().await;
    assert!(runtime.history(&f.project, &feature.id).unwrap().is_empty());
}

#[test]
fn feature_prerequisites_can_share_a_complete_atomic_batch() {
    let f = Fixture::new();
    let a = f.approved("Foundation");
    let b = f.approved("Export interface");
    f.forge()
        .set_dependency(&f.project, &b.id, &a.id, Some("Uses foundation contracts"))
        .unwrap();
    let tasks = f.tasks().list(&f.project).unwrap();
    let dependent = tasks
        .iter()
        .find(|t| t.draft.feature_id.as_ref() == Some(&b.id))
        .unwrap();
    assert!(
        f.tasks()
            .dispatch(f.dispatch(std::slice::from_ref(dependent)))
            .is_err()
    );
    let pulse = f.tasks().dispatch(f.dispatch(&tasks)).unwrap();
    let manifest = pipeline::manifest(&f.service.connection().unwrap(), &pulse.id)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.feature_dependencies.len(), 1);
    assert_eq!(manifest.tasks.first().unwrap().draft.feature_id, Some(a.id));
    assert_eq!(manifest.tasks.last().unwrap().draft.feature_id, Some(b.id));
}

#[test]
fn feature_discovery_response_schema_is_strictly_compliant_for_cli_adapters() {
    fn assert_strict(schema: &Value) {
        if schema["type"] == "object" {
            assert_eq!(schema["additionalProperties"], false);
            let properties = schema["properties"].as_object().expect("object properties");
            let required = schema["required"].as_array().expect("object required");
            assert_eq!(properties.len(), required.len());
            assert!(properties.keys().all(|key| required.contains(&json!(key))));
            for property in properties.values() {
                assert_strict(property);
            }
        }
        assert!(schema.get("oneOf").is_none());
        if let Some(branches) = schema["anyOf"].as_array() {
            for branch in branches {
                assert_strict(branch);
            }
        }
        if let Some(items) = schema.get("items") {
            assert_strict(items);
        }
    }
    use crate::terminals::CliBackend;

    // OpenAI Codex receives the specialized strict schema
    let codex_schema = response_schema_for_backend(Some(CliBackend::Codex));
    assert_eq!(codex_schema["type"], "object");
    assert_strict(&codex_schema);
    let codex_validator =
        jsonschema::validator_for(&codex_schema).expect("codex schema must be valid");

    // All other backends retain the canonical schema with isolated branches
    let canonical = canonical_response_schema();
    assert_eq!(
        response_schema_for_backend(Some(CliBackend::Claude)),
        canonical
    );
    assert_eq!(
        response_schema_for_backend(Some(CliBackend::Gemini)),
        canonical
    );
    assert_eq!(
        response_schema_for_backend(Some(CliBackend::Opencode)),
        canonical
    );
    assert_eq!(
        response_schema_for_backend(Some(CliBackend::Custom)),
        canonical
    );
    assert_eq!(response_schema_for_backend(None), canonical);
    assert_eq!(response_schema(), canonical);
    let canonical_validator =
        jsonschema::validator_for(&canonical).expect("canonical schema must be valid");

    // The analysis schemas are backend-agnostic: mandatory non-nullable ratings
    // and an `enum` output language remove the two things that force a Codex
    // variant, so one shape has to satisfy the strict rules on its own.
    for schema in [triage_analysis_schema(), dependency_analysis_schema()] {
        assert_eq!(schema["type"], "object");
        assert_strict(&schema);
        assert!(jsonschema::validator_for(&schema).is_ok());
    }

    let sample = json!({
        "outputLanguage": "en",
        "message": "Hello",
        "questions": ["Q1?"],
        "brief": {
            "objective": "Obj",
            "audience": "Aud",
            "behavior": "Beh",
            "scope": "Scope",
            "exclusions": "None",
            "alternatives": [],
            "decisions": [],
            "pendingDecisions": [],
            "criteria": [{"id": "c1", "description": "Crit 1"}],
            "tasks": [{
                "title": "Task 1",
                "kind": "feature",
                "description": "Desc 1",
                "criteria": [{"id": "c1", "description": "Crit 1"}],
                "dependencies": [],
                "featureId": null
            }],
            "sources": [],
            "coverage": "complete",
            "foundation": false
        },
        "evaluation": {
            "value": "Val",
            "alignment": "Align",
            "feasibility": "Feas",
            "usability": "Usab",
            "impact": 5,
            "confidence": 4,
            "effort": null
        },
        "suggestedDependencies": []
    });
    assert!(codex_validator.validate(&sample).is_ok());
    assert!(canonical_validator.validate(&sample).is_ok());
}

#[test]
fn active_agent_cli_backend_resolves_only_for_configured_cli_process_agents() {
    use crate::terminals::{
        AgentExecutionMode, CliBackend, CliModelSelectionSource, TerminalRepository, ThinkingLevel,
        UpdateCliAgentConfigRequest,
    };

    let fixture = Fixture::new();
    let repository = TerminalRepository::new(fixture.service.database_path());
    let defaults = repository
        .ensure_project_configs(&fixture.project, false)
        .unwrap();
    let backend_agent = defaults
        .iter()
        .find(|config| config.agent_role == AgentRole::BackendAgent)
        .unwrap()
        .clone();

    // Default seeding leaves execution_mode=api_direct, so no CLI backend applies yet.
    assert_eq!(
        fixture
            .service
            .active_agent_cli_backend(&fixture.project, AgentRole::BackendAgent)
            .unwrap(),
        None
    );

    repository
        .update_config(UpdateCliAgentConfigRequest {
            project_id: fixture.project.clone(),
            agent_role: AgentRole::BackendAgent,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Codex,
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Medium,
            custom_executable: None,
            custom_args: vec![],
            custom_risk_ack: false,
            expected_version: backend_agent.version,
        })
        .unwrap();

    assert_eq!(
        fixture
            .service
            .active_agent_cli_backend(&fixture.project, AgentRole::BackendAgent)
            .unwrap(),
        Some(CliBackend::Codex)
    );

    // A different, untouched role stays on the api_direct default.
    assert_eq!(
        fixture
            .service
            .active_agent_cli_backend(&fixture.project, AgentRole::TestAgent)
            .unwrap(),
        None
    );
}

/// Returns one canned payload for every FeatureAgent call, without the
/// handshake semaphores `PlanningGateway` uses for its concurrency assertions.
struct AnalysisGateway {
    response: String,
}
impl crate::llm::LlmGateway for AnalysisGateway {
    fn execute<'a>(
        &'a self,
        request: &'a crate::llm::LlmRequest,
        _cancellation: &'a CancellationToken,
    ) -> crate::llm::GatewayFuture<'a> {
        Box::pin(async move {
            use crate::llm::*;
            Ok(LlmResponse {
                execution_id: request.execution_id.clone(),
                generation_id: request.generation_id.clone(),
                route_id: RouteId::new("fixture"),
                connection_id: ConnectionId::new("fixture"),
                provider_id: ProviderId::new("fixture"),
                model: "fixture".into(),
                request_payload: json!({}),
                response_payload: json!({}),
                content: self.response.clone(),
                usage: LlmUsage::default(),
                finish_reason: None,
                estimated_cost_usd: None,
            })
        })
    }
}

fn analysis_runtime(f: &Fixture, response: Value) -> Arc<FeatureAgentRuntime> {
    let mut runtime = FeatureAgentRuntime::new(
        f.service.clone(),
        Arc::new(crate::AppEventBus::default()),
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    runtime.gateway_override = Some(Arc::new(AnalysisGateway {
        response: json(&response).unwrap(),
    }));
    Arc::new(runtime)
}

fn triage_payload(recommendation: &str) -> Value {
    json!({
        "outputLanguage": "en",
        "message": "The scope is well bounded and the risk is contained.",
        "evaluation": {
            "value": "Developers currently repeat this by hand every week.",
            "alignment": "Supports the stated planning direction.",
            "feasibility": "The storage layer already exposes what is needed.",
            "usability": "Reachable from the existing screen without new concepts.",
            "impact": 4, "confidence": 4, "effort": 2
        },
        "structuralLevel": 1,
        "recommendation": recommendation,
        "rationale": "The measured benefit clearly exceeds the implementation cost."
    })
}

#[tokio::test]
async fn triage_analysis_waits_for_the_open_incubation_questions() {
    let f = Fixture::new();
    let feature = f.feature("Export workflow");
    let mut brief = feature.brief.clone();
    brief.pending_decisions = vec!["Choose the export format".into()];
    let feature = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief)
        .unwrap();
    let runtime = analysis_runtime(&f, triage_payload("approve"));

    assert!(
        runtime
            .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
            .is_err(),
        "an open question must gate the analysis"
    );

    // Answering the question is what unblocks it.
    let mut brief = feature.brief.clone();
    brief.pending_decisions.clear();
    let feature = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief)
        .unwrap();
    runtime
        .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
        .expect("analysis runs once nothing is pending");
    runtime.wait_for_idle().await;
}

#[tokio::test]
async fn triage_analysis_records_the_verdict_without_reopening_the_feature() {
    let f = Fixture::new();
    let feature = f.feature("Export workflow");
    let runtime = analysis_runtime(&f, triage_payload("archive"));
    runtime
        .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    let stored = f.forge().get(&f.project, &feature.id).unwrap();
    // The whole point of not routing this through `save_brief`: a discovery turn
    // would have bumped the revision and pushed the feature back to incubating.
    assert_eq!(stored.revision, feature.revision);
    assert_eq!(stored.state, FeatureState::InTriage);
    assert_eq!(stored.structural_level, 1);
    assert_eq!(stored.evaluation.impact, Some(4));
    assert_eq!(stored.evaluation.effort, Some(2));
    assert!(stored.evaluation.value.contains("every week"));
    // Archive is only ever a recommendation; the feature is still live.
    assert!(stored.archive_reason.is_none());

    let turn = runtime
        .history(&f.project, &feature.id)
        .unwrap()
        .into_iter()
        .find(|turn| turn.kind == TurnKind::Triage)
        .expect("the analysis is recorded as its own turn");
    assert_eq!(turn.status, "completed");
    assert!(turn.assistant_message.unwrap().contains("archive"));
}

#[tokio::test]
async fn triage_analysis_recommending_approval_advances_a_ready_feature() {
    let f = Fixture::new();
    let feature = f.feature("Export workflow");
    let runtime = analysis_runtime(&f, triage_payload("approve"));
    runtime
        .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    let stored = f.forge().get(&f.project, &feature.id).unwrap();
    assert_eq!(stored.state, FeatureState::Approved);
    assert_eq!(stored.approved_revision, Some(feature.revision));
    assert_eq!(stored.evaluation.confidence, Some(4));
}

#[tokio::test]
async fn triage_analysis_keeps_its_verdict_when_approval_is_not_yet_possible() {
    let f = Fixture::new();
    let feature = f.feature("Export workflow");
    // A brief with no tasks cannot be approved, but it can still be triaged.
    let mut brief = feature.brief.clone();
    brief.tasks.clear();
    let feature = f
        .forge()
        .revise(&f.project, &feature.id, feature.revision, brief)
        .unwrap();
    let runtime = analysis_runtime(&f, triage_payload("approve"));
    runtime
        .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    let stored = f.forge().get(&f.project, &feature.id).unwrap();
    // The evaluation survives even though the approval it recommended could not
    // be applied -- a turn has no retry, so failing here would lose both.
    assert_eq!(stored.state, FeatureState::InTriage);
    assert_eq!(stored.approved_revision, None);
    assert_eq!(stored.evaluation.impact, Some(4));
    assert_eq!(
        f.forge()
            .snapshot(&f.project)
            .unwrap()
            .tasks
            .iter()
            .filter(|t| t.draft.feature_id.as_deref() == Some(feature.id.as_str()))
            .count(),
        0,
        "a rolled back approval must not leave half-inserted tasks behind"
    );
    let turn = runtime
        .history(&f.project, &feature.id)
        .unwrap()
        .into_iter()
        .find(|turn| turn.kind == TurnKind::Triage)
        .unwrap();
    assert_eq!(turn.status, "completed");
    assert!(turn.assistant_message.unwrap().contains("not applied"));
}

#[tokio::test]
async fn dependency_analysis_applies_edges_without_touching_confirmed_ones() {
    let f = Fixture::new();
    let storage = f.feature("Storage layer");
    let export = f.feature("Export workflow");
    let reports = f.feature("Reporting");
    // Triage is the gate for the dependency pass.
    let runtime = analysis_runtime(&f, triage_payload("archive"));
    runtime
        .start_triage_analysis(&f.project, &export.id, export.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    // A relation the developer already confirmed, with their own wording.
    f.forge()
        .set_dependency(
            &f.project,
            &export.id,
            &storage.id,
            Some("Confirmed by hand after reviewing the schema"),
        )
        .unwrap();
    // ...and one that makes the proposed reverse edge a cycle.
    f.forge()
        .set_dependency(
            &f.project,
            &reports.id,
            &export.id,
            Some("Reporting reads the exported records"),
        )
        .unwrap();

    let export = f.forge().get(&f.project, &export.id).unwrap();
    let runtime = analysis_runtime(
        &f,
        json!({
            "outputLanguage": "en",
            "message": "Export builds on storage and cannot precede reporting.",
            "dependencies": [
                {"prerequisiteId": storage.id, "rationale": "Rewritten by the agent"},
                {"prerequisiteId": reports.id, "rationale": "Would invert the reporting order"}
            ]
        }),
    );
    runtime
        .start_dependency_analysis(&f.project, &export.id, export.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    let edges = f
        .forge()
        .snapshot(&f.project)
        .unwrap()
        .dependencies
        .into_iter()
        .filter(|edge| edge.feature_id == export.id)
        .collect::<Vec<_>>();
    assert_eq!(
        edges.len(),
        1,
        "the cyclic proposal is skipped, not applied"
    );
    let kept = &edges[0];
    assert_eq!(kept.prerequisite_id, storage.id);
    assert_eq!(kept.origin, DependencyOrigin::Human);
    assert_eq!(
        kept.rationale, "Confirmed by hand after reviewing the schema",
        "an agent pass must never rewrite a human rationale"
    );
    let turn = runtime
        .history(&f.project, &export.id)
        .unwrap()
        .into_iter()
        .find(|turn| turn.kind == TurnKind::Dependencies)
        .unwrap();
    assert_eq!(turn.status, "completed");
    assert!(turn.assistant_message.unwrap().contains("Skipped"));
}

#[tokio::test]
async fn dependency_analysis_waits_for_triage() {
    let f = Fixture::new();
    let storage = f.feature("Storage layer");
    let export = f.feature("Export workflow");
    let runtime = analysis_runtime(
        &f,
        json!({"outputLanguage":"en","message":"n/a","dependencies":[{"prerequisiteId":storage.id,"rationale":"Needed first"}]}),
    );
    assert!(
        runtime
            .start_dependency_analysis(&f.project, &export.id, export.revision, &id())
            .is_err(),
        "an untriaged feature has no structural level to reason about"
    );
}

#[tokio::test]
async fn an_analysis_and_a_chat_turn_cannot_run_at_the_same_time() {
    let f = Fixture::new();
    let feature = f.feature("Export workflow");
    let (runtime, gateway) = planning_runtime(&f, &feature);
    let turn = runtime
        .start(&f.project, &feature.id, feature.revision, &id(), "Refine")
        .unwrap();
    gateway.entered.acquire().await.unwrap().forget();

    // The chat turn holds the feature, so the analysis is refused rather than
    // racing it -- both write the same evaluation column.
    assert!(
        runtime
            .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
            .is_err()
    );
    assert!(runtime.cancel(&turn));
    gateway.release.add_permits(1);
    runtime.wait_for_idle().await;
}

#[tokio::test]
async fn analysis_turns_stay_out_of_the_discovery_transcript() {
    let f = Fixture::new();
    let feature = f.feature("Export workflow");
    let runtime = analysis_runtime(&f, triage_payload("archive"));
    runtime
        .start_triage_analysis(&f.project, &feature.id, feature.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    let history = runtime.history(&f.project, &feature.id).unwrap();
    // Stored alongside the chat, but tagged so the transcript and, more
    // importantly, the six-turn conversation window can exclude it.
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].kind, TurnKind::Triage);
    assert_eq!(
        history
            .iter()
            .filter(|turn| turn.kind == TurnKind::Discovery)
            .count(),
        0
    );
}

#[tokio::test]
async fn triage_analysis_never_recreates_tasks_for_an_approved_feature() {
    let f = Fixture::new();
    let subject = f.feature("Export workflow");
    f.forge()
        .approve(&f.project, &subject.id, subject.revision)
        .unwrap();
    let before = f.tasks().list(&f.project).unwrap();
    assert_eq!(before.len(), 1);

    // The exact shape of the regression: the feature is already approved and its
    // tasks may be in delivery, and the analysis recommends approving again.
    let runtime = analysis_runtime(&f, triage_payload("approve"));
    runtime
        .start_triage_analysis(&f.project, &subject.id, subject.revision, &id())
        .unwrap();
    runtime.wait_for_idle().await;

    let after = f.tasks().list(&f.project).unwrap();
    assert_eq!(
        after, before,
        "re-approving supersedes the delivered tasks and inserts a duplicate of each"
    );
    assert!(after.iter().all(|t| t.state != "superseded"));

    // The evaluation still lands -- declining the approval must not cost the
    // developer the judgement they asked for.
    let stored = f.forge().get(&f.project, &subject.id).unwrap();
    assert_eq!(stored.evaluation.impact, Some(4));
    assert_eq!(stored.approved_revision, Some(subject.revision));

    let turn = runtime
        .history(&f.project, &subject.id)
        .unwrap()
        .into_iter()
        .find(|turn| turn.kind == TurnKind::Triage)
        .unwrap();
    assert_eq!(turn.status, "completed");
    assert!(turn.assistant_message.unwrap().contains("already approved"));
}

#[test]
fn apply_approval_is_idempotent_on_the_approved_revision() {
    let f = Fixture::new();
    let subject = f.feature("Export workflow");
    f.forge()
        .approve(&f.project, &subject.id, subject.revision)
        .unwrap();
    let before = f.tasks().list(&f.project).unwrap();

    // Called directly, bypassing the guard `approve` keeps for its own callers.
    // This is the path the agent turn commit takes, so the protection has to
    // live here too and not only in the public method.
    let db = f.service.connection().unwrap();
    let approved = feature(&db, &f.project, &subject.id).unwrap();
    apply_approval(&db, &f.project, &approved, subject.revision).unwrap();

    assert_eq!(f.tasks().list(&f.project).unwrap(), before);
}
