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
                content: if request.agent_role == AgentRole::IntakeRouter {
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
