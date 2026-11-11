use super::*;
use crate::persistence_service::EnsureProjectForWorkspaceRequest;
use rusqlite::params;

struct Fixture {
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
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
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        Self {
            _data: data,
            _workspace: workspace,
            service,
            project,
        }
    }

    fn checkpoint(&self, id: &str, purpose: &str, sequence: i64) {
        let db = self.service.connection().unwrap();
        let pulse = format!("pulse-{id}");
        db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version,input_revision,state_version) VALUES(?1,?2,?3,'SPECIFICATION','checkpoint','now','now',?4,4,2,5)", params![pulse, self.project, format!("Feature {id}"), sequence]).unwrap();
        db.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at) VALUES(?1,?2,'run_stage','completed','now','now')", params![format!("job-{id}"), pulse]).unwrap();
        db.execute("INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,purpose,input_revision,state_version,producer_job_id,generation) VALUES(?1,?2,'SPECIFICATION','waiting','now','now',?3,?3,2,5,?4,1)", params![id, pulse, purpose, format!("job-{id}")]).unwrap();
    }
}

#[test]
fn pending_decisions_are_read_only_current_and_project_scoped() {
    let f = Fixture::new();
    f.checkpoint("review", "artifact_review", 1);
    f.checkpoint("paths", "path_confirmation", 2);
    let db = f.service.connection().unwrap();
    let changes = db.total_changes();
    let decisions = list_pending_decisions_on(&db, &f.project).unwrap();
    assert_eq!(db.total_changes(), changes);
    assert_eq!(decisions.len(), 2);
    let review = decisions
        .iter()
        .find(|decision| decision.id == "checkpoint:review")
        .unwrap();
    assert_eq!(review.kind, PendingDecisionKind::ArtifactReview);
    assert_eq!(review.input_revision, Some(2));
    assert_eq!(review.state_version, 5);
    assert!(review.actions.contains(&PendingDecisionAction::Approve));
    assert!(read_pending_decision_on(&db, "other-project", "checkpoint:review").is_err());
    db.execute(
        "UPDATE checkpoints SET status='approved' WHERE id='review'",
        [],
    )
    .unwrap();
    assert!(
        f.service
            .read_pending_decision(&f.project, "checkpoint:review")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.service.list_pending_decisions(&f.project).unwrap().len(),
        1
    );
}

#[test]
fn review_waiting_count_does_not_include_path_or_clarification_waits() {
    let f = Fixture::new();
    f.checkpoint("review", "artifact_review", 1);
    f.checkpoint("paths", "path_confirmation", 2);
    f.checkpoint("questions", "clarification", 3);
    let snapshot = f
        .service
        .get_pulsation_runtime_snapshot_for_detail(
            crate::persistence_service::GetPulsationRuntimeSnapshotRequest {
                project_id: f.project.clone(),
            },
            Some("pulse-review"),
        )
        .unwrap()
        .snapshot;
    assert_eq!(snapshot.hitl_waiting_count, 1);
    assert_eq!(snapshot.hitl_checkpoints.len(), 1);
}

#[test]
fn stale_checkpoint_is_not_advertised_as_an_actionable_decision() {
    let f = Fixture::new();
    f.checkpoint("review", "artifact_review", 1);
    f.service
        .connection()
        .unwrap()
        .execute(
            "UPDATE pulsations SET input_revision=3,state_version=6 WHERE id='pulse-review'",
            [],
        )
        .unwrap();
    assert!(
        f.service
            .list_pending_decisions(&f.project)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn clarification_projection_keeps_exact_question_identity_and_fence() {
    let f = Fixture::new();
    f.checkpoint("question", "clarification", 1);
    let db = f.service.connection().unwrap();
    db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('host-question',?1,'specification','host-presented','Questions','now','now')", [&f.project]).unwrap();
    db.execute("INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES('set-a','question','pulse-question',2,1,5,'host-question',?1)", [r#"[{"id":"choice-a","question":"Which option?","rationale":"A product decision."}]"#]).unwrap();
    let decision = f
        .service
        .read_pending_decision(&f.project, "checkpoint:question")
        .unwrap()
        .unwrap();
    assert_eq!(decision.kind, PendingDecisionKind::Clarification);
    let questions = decision.question_set.unwrap();
    assert_eq!(questions.id, "set-a");
    assert_eq!(questions.questions[0].id, "choice-a");
    assert_eq!(questions.fence.state_version, 5);
    assert_eq!(questions.fence.project_id, f.project);
    let count: i64 = db
        .query_row("SELECT count(*) FROM chat_messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn pending_decision_conversation_is_lazy_reused_and_cannot_cross_projects() {
    let f = Fixture::new();
    f.checkpoint("review", "artifact_review", 1);
    let db = f.service.connection().unwrap();
    db.execute("INSERT INTO provider_connections(id,provider_id,display_name,credential_ref,default_model,enabled,state,created_at,updated_at) VALUES('fixture-connection','openai','Fixture','fixture-secret-value','fixture-model',1,'active','now','now')", []).unwrap();
    let before: i64 = db
        .query_row("SELECT count(*) FROM chat_conversations", [], |row| {
            row.get(0)
        })
        .unwrap();
    f.service.list_pending_decisions(&f.project).unwrap();
    assert_eq!(
        before,
        db.query_row("SELECT count(*) FROM chat_conversations", [], |row| row
            .get::<_, i64>(0))
            .unwrap()
    );
    let first = f
        .service
        .ensure_decision_conversation(&f.project, "checkpoint:review")
        .unwrap();
    let second = f
        .service
        .ensure_decision_conversation(&f.project, "checkpoint:review")
        .unwrap();
    assert_eq!(first, second);
    assert!(
        f.service
            .ensure_decision_conversation("missing", "checkpoint:review")
            .is_err()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM chat_messages", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn failure_and_cli_fallback_actions_follow_the_persisted_host_choices() {
    let f = Fixture::new();
    f.checkpoint("failure", "stage_failure", 1);
    f.checkpoint("fallback", "cli_fallback", 2);
    let db = f.service.connection().unwrap();
    db.execute("INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,error_code,diagnostic,created_at,updated_at) VALUES('failure','pulse-failure','job-failure','SPECIFICATION','provider_transport',1,0,'fixture_failure','Provider unavailable.','now','now')",[]).unwrap();
    db.execute("INSERT INTO cli_fallback_requests(checkpoint_id,pulsation_id,agent_role,error_code,diagnostic,created_at,updated_at) VALUES('fallback','pulse-fallback','task_specifier','fixture_cli','Choose a supported recovery action.','now','now')",[]).unwrap();
    let failure = f
        .service
        .read_pending_decision(&f.project, "checkpoint:failure")
        .unwrap()
        .unwrap();
    assert_eq!(failure.kind, PendingDecisionKind::StageFailure);
    assert!(failure.actions.contains(&PendingDecisionAction::Retry));
    assert!(failure.actions.contains(&PendingDecisionAction::Abort));
    assert!(
        !failure
            .actions
            .contains(&PendingDecisionAction::RequestRevision)
    );
    let fallback = f
        .service
        .read_pending_decision(&f.project, "checkpoint:fallback")
        .unwrap()
        .unwrap();
    assert_eq!(fallback.kind, PendingDecisionKind::CliFallback);
    assert_eq!(fallback.actions, vec![PendingDecisionAction::OpenAgents]);
    db.execute(
        "UPDATE cli_fallback_requests SET decision='cancel' WHERE checkpoint_id='fallback'",
        [],
    )
    .unwrap();
    assert!(
        f.service
            .read_pending_decision(&f.project, "checkpoint:fallback")
            .unwrap()
            .is_none()
    );
}

#[test]
fn forge_questions_delivery_and_base_review_are_project_scoped_link_decisions() {
    let f = Fixture::new();
    let db = f.service.connection().unwrap();
    let brief = serde_json::json!({"pendingDecisions":["Choose an implementation approach."]});
    db.execute("INSERT INTO forge_features(id,project_id,title,original_idea,revision,state,brief_json,evaluation_json,created_at,updated_at) VALUES('feature',?1,'Fixture','Idea',3,'incubating',?2,'{}','now','now')",params![f.project,brief.to_string()]).unwrap();
    db.execute("INSERT INTO forge_turns(id,feature_id,feature_revision,ordinal,request_id,status,user_message,created_at,updated_at) VALUES('turn','feature',2,1,'fixture-request','completed','Question','now','now')",[]).unwrap();
    let pending = f.service.list_pending_decisions(&f.project).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "forge:feature:3:turn");
    assert_eq!(pending[0].kind, PendingDecisionKind::ForgeQuestions);
    db.execute(
        "UPDATE forge_turns SET status='running' WHERE id='turn'",
        [],
    )
    .unwrap();
    assert!(
        f.service
            .list_pending_decisions(&f.project)
            .unwrap()
            .is_empty()
    );
    db.execute(
        "UPDATE forge_turns SET status='completed' WHERE id='turn'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE forge_features SET revision=4 WHERE id='feature'",
        [],
    )
    .unwrap();
    assert!(
        f.service
            .read_pending_decision(&f.project, "forge:feature:3:turn")
            .unwrap()
            .is_none()
    );
    db.execute(
        "UPDATE forge_features SET state='ready_for_review' WHERE id='feature'",
        [],
    )
    .unwrap();
    db.execute("UPDATE project_pipeline_state SET base_state='needs_review',blocking_reason='Fixture reconciliation' WHERE project_id=?1",[&f.project]).unwrap();
    let pending = f.service.list_pending_decisions(&f.project).unwrap();
    assert_eq!(pending.len(), 2);
    assert!(
        pending
            .iter()
            .any(|decision| decision.kind == PendingDecisionKind::Delivery)
    );
    assert!(
        pending
            .iter()
            .any(|decision| decision.kind == PendingDecisionKind::BaseReview)
    );
}
