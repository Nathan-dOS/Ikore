//! Feature 25, M3: the events of the Ikore pipeline that start the
//! developer's pipelines (outbox, dispatch, payload, a project's own trigger).

use rusqlite::{Connection, params};

use super::custom_pipeline_tests::{Fixture, agent, fixture, gate, saved, second_project};
use super::{CreatePulsationRequest, PersistenceService, PulsationStage, PulsationStatus};
use crate::custom_pipelines::{
    CustomPipelineDefinitionV1, CustomPipelineView, DefaultEvent, PipelineRunStatus,
    PipelineTrigger, SaveCustomPipelineOutcome, SaveCustomPipelineRequest,
    SetPipelineTriggerRequest,
};

fn passed(stage: &str) -> PipelineTrigger {
    PipelineTrigger::DefaultEvent {
        event: DefaultEvent::StagePassed,
        stage: Some(stage.into()),
    }
}

fn done() -> PipelineTrigger {
    PipelineTrigger::DefaultEvent {
        event: DefaultEvent::PulsationDone,
        stage: None,
    }
}

impl Fixture {
    fn listening(&self, project: &str, name: &str, trigger: PipelineTrigger) -> CustomPipelineView {
        saved(
            self.service
                .save_custom_pipeline(SaveCustomPipelineRequest {
                    project_id: project.into(),
                    pipeline_id: None,
                    expected_version: None,
                    scope: None,
                    parent_pipeline_id: None,
                    name: name.into(),
                    description: String::new(),
                    definition: CustomPipelineDefinitionV1 {
                        schema_version: 1,
                        trigger,
                        steps: vec![gate("look")],
                    },
                })
                .unwrap(),
        )
    }

    fn pulsation(&self, project: &str, title: &str) -> String {
        self.service
            .create_pulsation(CreatePulsationRequest {
                project_id: project.into(),
                feature_name: title.into(),
            })
            .unwrap()
            .pulsation
            .id
    }

    /// One transition, in its own transaction, as the pipeline makes them.
    fn moves(&self, pulse: &str, status: PulsationStatus, stage: PulsationStage, event: &str) {
        let mut db = self.service.connection().unwrap();
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let current = PersistenceService::find_pulsation_by_id(&tx, pulse)
            .unwrap()
            .unwrap();
        PersistenceService::transition_pulsation(&tx, &current, status, stage, event, None)
            .unwrap();
        tx.commit().unwrap();
    }

    /// The stage left forward, the way `stage_auto_advanced` does.
    fn passes(&self, pulse: &str, stage: PulsationStage) {
        self.moves(pulse, PulsationStatus::Running, stage, "job_started");
        self.moves(
            pulse,
            PulsationStatus::Planning,
            stage.next().unwrap(),
            "stage_auto_advanced",
        );
    }

    /// An accepted artifact of `stage` with a publication in `state`, as the
    /// stage's acceptance writes it before the transition.
    fn publishes(&self, pulse: &str, stage: PulsationStage, path: &str, state: &str) -> String {
        let db = self.db();
        let id = uuid::Uuid::new_v4().to_string();
        let (project, version): (String, u64) = db
            .query_row(
                "SELECT project_id,state_version FROM pulsations WHERE id=?1",
                [pulse],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let sha = format!("{:0>64}", path.len());
        db.execute(
            "INSERT INTO artifacts(id,pulsation_id,name,kind,relative_path,content_sha256,created_at,
               updated_at,content,producer_stage,generation,input_revision,producer_state_version,
               output_position,acceptance,publication_state)
             VALUES(?1,?2,'file','stage_output',?3,?4,'t','t','secret body',?5,1,1,?6,0,'accepted',?7)",
            params![
                id,
                pulse,
                path,
                sha,
                stage.as_db_value(),
                version,
                if state == "cancelled" { "pending" } else { state }
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,
               desired_sha256,state,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'t')",
            params![id, project, pulse, path, sha, state],
        )
        .unwrap();
        id
    }

    fn publication(&self, artifact: &str, state: &str) {
        self.db()
            .execute(
                "UPDATE artifact_publications SET state=?2 WHERE artifact_id=?1",
                params![artifact, state],
            )
            .unwrap();
    }

    fn outbox(&self) -> Vec<(String, Option<String>, String, Option<String>)> {
        self.db()
            .prepare(
                "SELECT event,stage,state,discard_reason FROM pipeline_trigger_outbox
                  ORDER BY created_at,rowid",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn runs_of(
        &self,
        project: &str,
        pipeline: &str,
    ) -> Vec<crate::custom_pipelines::PipelineRunView> {
        self.service
            .custom_pipelines_snapshot(project)
            .unwrap()
            .runs
            .into_iter()
            .filter(|run| run.pipeline_id == pipeline)
            .collect()
    }
}

fn event(
    event: &str,
    stage: Option<&str>,
    state: &str,
) -> (String, Option<String>, String, Option<String>) {
    (event.into(), stage.map(str::to_owned), state.into(), None)
}

#[test]
fn a_transition_records_an_event_only_when_a_ready_box_listens_to_it() {
    let f = fixture();
    f.listening(&f.project, "By hand", PipelineTrigger::Manual);
    let pulse = f.pulsation(&f.project, "Token expiry");
    f.passes(&pulse, PulsationStage::Intake);
    assert!(f.outbox().is_empty(), "nothing listens to Intake");

    let after_spec = f.listening(&f.project, "After spec", passed("SPECIFICATION"));
    let archived = f.listening(&f.project, "Archived", passed("Contract"));
    saved(
        f.service
            .set_custom_pipeline_archived(&f.project, &archived.id, true)
            .unwrap(),
    );
    // A checkpoint opening is not a passed stage; its approval is.
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Specification,
        "job_started",
    );
    f.moves(
        &pulse,
        PulsationStatus::Checkpoint,
        PulsationStage::Specification,
        "checkpoint_ready",
    );
    assert!(f.outbox().is_empty());
    f.moves(
        &pulse,
        PulsationStatus::Planning,
        PulsationStage::Contract,
        "hitl_approved",
    );
    assert_eq!(
        f.outbox(),
        [event("stage_passed", Some("SPECIFICATION"), "pending")]
    );
    // The archived box listens to Contract: nothing is recorded for it.
    f.passes(&pulse, PulsationStage::Contract);
    assert_eq!(f.outbox().len(), 1);

    // A revision goes back without an event; passing again is a new event.
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Code,
        "job_started",
    );
    f.moves(
        &pulse,
        PulsationStatus::Planning,
        PulsationStage::Specification,
        "hitl_revision_requested",
    );
    assert_eq!(f.outbox().len(), 1);
    f.passes(&pulse, PulsationStage::Specification);
    assert_eq!(
        f.outbox(),
        [
            event("stage_passed", Some("SPECIFICATION"), "pending"),
            event("stage_passed", Some("SPECIFICATION"), "pending"),
        ]
    );

    // `Pulsation done`, and nothing on a failure.
    f.listening(&f.project, "After done", done());
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Integrator,
        "job_started",
    );
    f.moves(
        &pulse,
        PulsationStatus::Done,
        PulsationStage::Integrator,
        "hitl_approved_final",
    );
    assert_eq!(f.outbox()[2], event("pulsation_done", None, "pending"));
    let failing = f.pulsation(&f.project, "Fails");
    f.moves(
        &failing,
        PulsationStatus::Running,
        PulsationStage::Specification,
        "job_started",
    );
    f.moves(
        &failing,
        PulsationStatus::Failed,
        PulsationStage::Specification,
        "stage_failed",
    );
    assert_eq!(f.outbox().len(), 3);

    // The ordinary dispatch: three runs of the two boxes, each once.
    assert!(f.service.dispatch_pipeline_triggers().unwrap());
    assert!(!f.service.dispatch_pipeline_triggers().unwrap());
    assert_eq!(f.runs_of(&f.project, &after_spec.id).len(), 2);
    assert!(f.outbox().iter().all(|row| row.2 == "dispatched"));
}

#[test]
fn dispatch_waits_for_applied_publications_and_discards_what_a_failure_left() {
    let f = fixture();
    let pipeline = f.listening(&f.project, "After code", passed("Code"));
    let pulse = f.pulsation(&f.project, "Token expiry");
    f.publishes(&pulse, PulsationStage::Intake, "docs/intake.md", "applied");
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Code,
        "job_started",
    );
    let artifact = f.publishes(&pulse, PulsationStage::Code, "src/auth.rs", "pending");
    f.moves(
        &pulse,
        PulsationStatus::Planning,
        PulsationStage::Test,
        "stage_auto_advanced",
    );

    // Accepted, not applied yet: no run is born.
    assert!(!f.service.dispatch_pipeline_triggers().unwrap());
    assert_eq!(f.outbox(), [event("stage_passed", Some("Code"), "pending")]);
    assert!(f.runs_of(&f.project, &pipeline.id).is_empty());
    f.publication(&artifact, "applying");
    assert!(!f.service.dispatch_pipeline_triggers().unwrap());

    f.publication(&artifact, "applied");
    assert!(f.service.dispatch_pipeline_triggers().unwrap());
    let runs = f.runs_of(&f.project, &pipeline.id);
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.trigger_kind, "default_event");
    assert_eq!(
        run.trigger_label.as_deref(),
        Some("Stage passed \u{b7} Code")
    );
    assert_eq!(run.source_pulsation_id.as_deref(), Some(pulse.as_str()));
    assert_eq!(run.source_pulsation_title.as_deref(), Some("Token expiry"));
    assert!(!run.source_pulsation_failed);
    assert_eq!(run.status, PipelineRunStatus::WaitingGate);

    // The payload: paths and hashes of what was published until then.
    let payload: serde_json::Value = serde_json::from_str(
        &f.db()
            .query_row(
                "SELECT trigger_payload_json FROM custom_pipeline_runs WHERE id=?1",
                [&run.id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["event"], "stage_passed");
    assert_eq!(payload["stage"], "Code");
    assert_eq!(payload["pulsation"]["title"], "Token expiry");
    let files = payload["files"].as_array().unwrap();
    let paths: Vec<_> = files
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["docs/intake.md", "src/auth.rs"]);
    assert_eq!(files[1]["stage"], "Code");
    assert_eq!(files[1]["sha256"].as_str().unwrap().len(), 64);
    assert!(
        !payload.to_string().contains("secret body"),
        "no file content"
    );

    // The pulsation fails after the dispatch: the run goes on, marked.
    // Code runs again after a revision, then the pulsation fails before its
    // publication is applied: that event is discarded with the reason.
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Test,
        "job_started",
    );
    f.moves(
        &pulse,
        PulsationStatus::Planning,
        PulsationStage::Code,
        "hitl_revision_requested",
    );
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Code,
        "job_started",
    );
    f.publishes(&pulse, PulsationStage::Code, "src/auth.rs", "pending");
    f.moves(
        &pulse,
        PulsationStatus::Planning,
        PulsationStage::Test,
        "stage_auto_advanced",
    );
    f.moves(
        &pulse,
        PulsationStatus::Running,
        PulsationStage::Test,
        "job_started",
    );
    f.moves(
        &pulse,
        PulsationStatus::Failed,
        PulsationStage::Test,
        "stage_failed",
    );
    assert!(!f.service.dispatch_pipeline_triggers().unwrap());
    let rows = f.outbox();
    assert_eq!(rows[1].2, "discarded");
    assert_eq!(rows[1].3.as_deref(), Some("source_pulsation_failed"));
    let runs = f.runs_of(&f.project, &pipeline.id);
    assert_eq!(runs.len(), 1);
    assert!(runs[0].source_pulsation_failed, "Source pulsation failed");
}

#[test]
fn cancelled_or_conflicting_publications_discard_the_event() {
    let f = fixture();
    f.listening(&f.project, "After code", passed("Code"));
    for (state, reason) in [
        ("cancelled", "publications_cancelled"),
        ("conflict", "publication_conflict"),
    ] {
        let pulse = f.pulsation(&f.project, state);
        f.moves(
            &pulse,
            PulsationStatus::Running,
            PulsationStage::Code,
            "job_started",
        );
        f.publishes(&pulse, PulsationStage::Code, "src/a.rs", state);
        f.moves(
            &pulse,
            PulsationStatus::Planning,
            PulsationStage::Test,
            "stage_auto_advanced",
        );
        f.service.dispatch_pipeline_triggers().unwrap();
        let last = f.outbox().pop().unwrap();
        assert_eq!(
            (last.2.as_str(), last.3.as_deref()),
            ("discarded", Some(reason))
        );
    }
}

#[test]
fn an_event_recorded_before_the_app_closed_starts_its_run_on_the_next_start() {
    let f = fixture();
    let pipeline = f.listening(&f.project, "After intake", passed("Intake"));
    let pulse = f.pulsation(&f.project, "Token expiry");
    f.passes(&pulse, PulsationStage::Intake);
    assert_eq!(f.outbox()[0].2, "pending");
    // A new service on the same data, as the next start of the app.
    let data = f.service.database_path().parent().unwrap().to_path_buf();
    let reopened = PersistenceService::initialize(&data).unwrap();
    reopened.recover_automation_lane().unwrap();
    assert!(reopened.dispatch_pipeline_triggers().unwrap());
    assert_eq!(f.runs_of(&f.project, &pipeline.id).len(), 1);
}

#[test]
fn each_event_starts_each_pipeline_once() {
    let f = fixture();
    let pipeline = f.listening(&f.project, "After intake", passed("Intake"));
    let pulse = f.pulsation(&f.project, "Token expiry");
    f.passes(&pulse, PulsationStage::Intake);
    let db = f.db();
    let (id, version): (String, u64) = db
        .query_row(
            "SELECT id,state_version FROM pipeline_trigger_outbox",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let duplicate = db.execute(
        "INSERT INTO pipeline_trigger_outbox(id,project_id,pulsation_id,event,stage,state_version,created_at)
         VALUES('again',?1,?2,'stage_passed','Intake',?3,'t')",
        params![f.project, pulse, version],
    );
    assert!(duplicate.is_err(), "one row per event");
    let done_twice = |db: &Connection, row: &str| {
        db.execute(
            "INSERT INTO pipeline_trigger_outbox(id,project_id,pulsation_id,event,state_version,created_at)
             VALUES(?1,?2,?3,'pulsation_done',99,'t')",
            params![row, f.project, pulse],
        )
    };
    done_twice(&db, "done-1").unwrap();
    assert!(
        done_twice(&db, "done-2").is_err(),
        "COALESCE keeps a NULL stage unique"
    );
    f.service.dispatch_pipeline_triggers().unwrap();
    let run = f.runs_of(&f.project, &pipeline.id)[0].id.clone();
    let copy = |new: &str, outbox: &str| {
        db.execute(
            "INSERT INTO custom_pipeline_runs(id,pipeline_id,version,project_id,trigger_kind,
               trigger_payload_json,resolved_definition_json,resolved_sha256,status,created_at,
               updated_at,trigger_outbox_id)
             SELECT ?2,pipeline_id,version,project_id,trigger_kind,trigger_payload_json,
                    resolved_definition_json,resolved_sha256,'queued',created_at,updated_at,?3
               FROM custom_pipeline_runs WHERE id=?1",
            params![run, new, outbox],
        )
    };
    copy("other-event", "done-1").unwrap();
    assert!(
        copy("twice", &id).is_err(),
        "one run per pipeline and event"
    );
}

#[test]
fn a_global_pipeline_follows_the_trigger_each_project_chose() {
    let f = fixture();
    let (_workspace, other) = second_project(&f, "Other");
    let global = saved(f.create_in(&f.project, "Shared", "global", None, vec![gate("look")]));
    saved(
        f.service
            .activate_custom_pipeline(&other, &global.id)
            .unwrap(),
    );
    let set = |project: &str, pipeline: &str, trigger: Option<PipelineTrigger>| {
        f.service
            .set_custom_pipeline_trigger(SetPipelineTriggerRequest {
                project_id: project.into(),
                pipeline_id: pipeline.into(),
                trigger,
            })
            .unwrap()
    };
    let shown = saved(set(&other, &global.id, Some(passed("Intake"))));
    assert_eq!(shown.trigger_override, Some(passed("Intake")));
    assert_eq!(shown.effective_trigger(), &passed("Intake"));
    assert_eq!(shown.current_version, 1, "not a new version");

    let here = f.pulsation(&f.project, "Here");
    f.passes(&here, PulsationStage::Intake);
    assert!(
        f.outbox().is_empty(),
        "this project keeps the manual trigger"
    );
    let there = f.pulsation(&other, "There");
    f.passes(&there, PulsationStage::Intake);
    assert!(f.service.dispatch_pipeline_triggers().unwrap());
    assert_eq!(f.runs_of(&other, &global.id).len(), 1);
    assert!(f.runs_of(&f.project, &global.id).is_empty());

    let codes = |outcome: SaveCustomPipelineOutcome| match outcome {
        SaveCustomPipelineOutcome::Invalid { errors } => errors
            .into_iter()
            .map(|error| error.code)
            .collect::<Vec<_>>(),
        SaveCustomPipelineOutcome::Saved { .. } => Vec::new(),
    };
    assert_eq!(
        codes(set(&other, &global.id, Some(passed("Integrator")))),
        ["stage_unknown"]
    );
    let own = saved(f.create_in(&f.project, "Own", "project", None, vec![agent("a")]));
    assert_eq!(
        codes(set(&f.project, &own.id, Some(done()))),
        ["trigger_not_global"]
    );
    let cleared = saved(set(&other, &global.id, None));
    assert_eq!(cleared.trigger_override, None);
    assert_eq!(cleared.effective_trigger(), &PipelineTrigger::Manual);

    // Deleting the project takes its events and its runs of the global one.
    f.moves(
        &there,
        PulsationStatus::Running,
        PulsationStage::Specification,
        "job_started",
    );
    f.moves(
        &there,
        PulsationStatus::Failed,
        PulsationStage::Specification,
        "stage_failed",
    );
    f.service
        .delete_project_permanently(&other, "Other")
        .unwrap();
    let left: u32 = f
        .db()
        .query_row("SELECT count(*) FROM pipeline_trigger_outbox", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(left, 0);
}
