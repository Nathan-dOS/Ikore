//! Feature 25 (M2a, M2b): the store, global pipelines and children, the gates and the automation lane's
//! admission, without a CLI (the CLI and API paths are exercised with the
//! test-support fixture in `ikore-test-support/tests/custom_pipelines.rs`).

use rusqlite::{Connection, params};

use super::{CreateProjectRequest, PersistenceService, embedded};
use crate::custom_pipelines::{
    AgentStep, CustomPipelineDefinitionV1, CustomPipelineView, DecidePipelineGateRequest, GateStep,
    HostTool, HostToolStep, MAX_ACTIVE_PIPELINES, MAX_GLOBAL_PIPELINES, MAX_PROJECT_PIPELINES,
    PipelineRunStatus, PipelineStep, PipelineTrigger, SaveCustomPipelineOutcome,
    SaveCustomPipelineRequest, StepAgent, StepOutput, StepRunStatus, StepTransport,
};

pub(super) struct Fixture {
    _data: tempfile::TempDir,
    _workspace: tempfile::TempDir,
    pub(super) service: PersistenceService,
    pub(super) project: String,
}

pub(super) fn fixture() -> Fixture {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = service
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("Pipelines fixture".into()),
        })
        .unwrap()
        .project
        .id;
    Fixture {
        _data: data,
        _workspace: workspace,
        service,
        project,
    }
}

pub(super) fn agent(id: &str) -> PipelineStep {
    PipelineStep::Agent(AgentStep {
        id: id.into(),
        label: format!("Agent {id}"),
        agent: StepAgent {
            transport: StepTransport::Cli,
            backend: Some("claude".into()),
            model: Some("haiku".into()),
            model_profile: None,
            thinking_level: "default".into(),
            api_route_id: None,
        },
        instructions: "Review the change.".into(),
        output: StepOutput::Text,
    })
}

pub(super) fn gate(id: &str) -> PipelineStep {
    PipelineStep::Gate(GateStep {
        id: id.into(),
        label: format!("Gate {id}"),
        prompt: "Approve?".into(),
    })
}

fn definition(steps: Vec<PipelineStep>) -> CustomPipelineDefinitionV1 {
    CustomPipelineDefinitionV1 {
        schema_version: 1,
        trigger: PipelineTrigger::Manual,
        steps,
    }
}

impl Fixture {
    fn save(
        &self,
        id: Option<&CustomPipelineView>,
        name: &str,
        steps: Vec<PipelineStep>,
    ) -> SaveCustomPipelineOutcome {
        self.service
            .save_custom_pipeline(SaveCustomPipelineRequest {
                project_id: self.project.clone(),
                pipeline_id: id.map(|view| view.id.clone()),
                expected_version: id.map(|view| view.current_version),
                scope: None,
                parent_pipeline_id: None,
                name: name.into(),
                description: String::new(),
                definition: definition(steps),
            })
            .unwrap()
    }

    fn create(&self, name: &str, steps: Vec<PipelineStep>) -> CustomPipelineView {
        match self.save(None, name, steps) {
            SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
            SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
        }
    }

    pub(super) fn db(&self) -> Connection {
        Connection::open(self.service.database_path()).unwrap()
    }
}

fn codes(outcome: SaveCustomPipelineOutcome) -> Vec<String> {
    match outcome {
        SaveCustomPipelineOutcome::Invalid { errors } => {
            errors.into_iter().map(|error| error.code).collect()
        }
        SaveCustomPipelineOutcome::Saved { .. } => Vec::new(),
    }
}

#[test]
fn pipelines_are_created_versioned_archived_and_deleted() {
    let f = fixture();
    let created = f.create("Release checks", vec![agent("review")]);
    assert_eq!((created.current_version, created.adopted_version), (1, 1));
    assert_eq!((created.scope.as_str(), created.position), ("project", 0));
    let SaveCustomPipelineOutcome::Saved { pipeline: edited } = f.save(
        Some(&created),
        "Release checks",
        vec![agent("review"), gate("ok")],
    ) else {
        panic!("edit refused");
    };
    assert_eq!((edited.current_version, edited.adopted_version), (2, 2));
    // Renaming alone keeps the version.
    let SaveCustomPipelineOutcome::Saved { pipeline: renamed } = f.save(
        Some(&edited),
        "Release review",
        vec![agent("review"), gate("ok")],
    ) else {
        panic!("rename refused");
    };
    assert_eq!(
        (renamed.name.as_str(), renamed.current_version),
        ("Release review", 2)
    );
    // A form opened on an older version is refused.
    assert_eq!(
        codes(f.save(Some(&created), "Release review", vec![agent("review")])),
        ["version_conflict"]
    );
    let snapshot = f.service.custom_pipelines_snapshot(&f.project).unwrap();
    assert_eq!(snapshot.pipelines.len(), 1);
    assert_eq!(snapshot.max_concurrent, 2);
    let archived = f
        .service
        .set_custom_pipeline_archived(&f.project, &created.id, true)
        .unwrap();
    assert!(matches!(archived, SaveCustomPipelineOutcome::Saved { pipeline } if pipeline.archived));
    let snapshot = f.service.custom_pipelines_snapshot(&f.project).unwrap();
    assert!(snapshot.pipelines.is_empty());
    assert_eq!(snapshot.archived.len(), 1);
    assert!(
        f.service
            .start_custom_pipeline_run(&f.project, &created.id)
            .is_err()
    );
    f.service
        .set_custom_pipeline_archived(&f.project, &created.id, false)
        .unwrap();
    f.service
        .start_custom_pipeline_run(&f.project, &created.id)
        .unwrap();
    let refused = f
        .service
        .delete_custom_pipeline(&f.project, &created.id)
        .unwrap_err();
    assert!(refused.to_string().contains("Archive it instead"));
    let unused = f.create("Unused", vec![gate("only")]);
    f.service
        .delete_custom_pipeline(&f.project, &unused.id)
        .unwrap();
    let versions: u32 = f
        .db()
        .query_row(
            "SELECT count(*) FROM custom_pipeline_versions WHERE pipeline_id=?1",
            [&unused.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(versions, 0);
}

#[test]
fn limits_and_names_are_enforced() {
    let f = fixture();
    assert_eq!(
        codes(f.save(None, "pipeline", vec![gate("a")])),
        ["name_reserved"]
    );
    let first = f.create("One", vec![gate("a")]);
    assert_eq!(
        codes(f.save(None, " one ", vec![gate("a")])),
        ["name_taken"]
    );
    for index in 2..=MAX_PROJECT_PIPELINES {
        f.create(&format!("Pipeline {index}"), vec![gate("a")]);
    }
    assert_eq!(
        codes(f.save(None, "Sixth", vec![gate("a")])),
        ["project_limit"]
    );
    // Archived pipelines do not count, and restoring is held to the limits.
    f.service
        .set_custom_pipeline_archived(&f.project, &first.id, true)
        .unwrap();
    let sixth = f.create("One", vec![gate("a")]);
    let restore = f
        .service
        .set_custom_pipeline_archived(&f.project, &first.id, false)
        .unwrap();
    let restored = codes(restore);
    assert!(
        restored.contains(&"project_limit".to_owned()),
        "{restored:?}"
    );
    assert!(restored.contains(&"name_taken".to_owned()), "{restored:?}");
    f.service
        .set_custom_pipeline_archived(&f.project, &sixth.id, true)
        .unwrap();
    assert!(matches!(
        f.service
            .set_custom_pipeline_archived(&f.project, &first.id, false)
            .unwrap(),
        SaveCustomPipelineOutcome::Saved { .. }
    ));
}

#[test]
fn refused_definitions_report_every_problem() {
    let f = fixture();
    let PipelineStep::Agent(mut custom) = agent("custom") else {
        unreachable!();
    };
    custom.agent.backend = Some("custom".into());
    let PipelineStep::Agent(mut api) = agent("api") else {
        unreachable!();
    };
    api.agent.transport = StepTransport::Api;
    api.agent.api_route_id = Some("no-such-route".into());
    let PipelineStep::Agent(mut default_model) = agent("default-model") else {
        unreachable!();
    };
    default_model.agent.model = None;
    let mut refused = definition(vec![
        PipelineStep::Agent(custom),
        PipelineStep::Agent(api),
        PipelineStep::Agent(default_model),
        PipelineStep::HostTool(HostToolStep {
            id: "commit".into(),
            label: "Commit".into(),
            tool: HostTool::GitCommit,
        }),
    ]);
    refused.trigger = PipelineTrigger::DefaultEvent {
        event: crate::custom_pipelines::DefaultEvent::StagePassed,
        stage: Some("Integrator".into()),
    };
    let outcome = f
        .service
        .save_custom_pipeline(SaveCustomPipelineRequest {
            project_id: f.project.clone(),
            pipeline_id: None,
            expected_version: None,
            scope: None,
            parent_pipeline_id: None,
            name: "Refused".into(),
            description: "d".repeat(501),
            definition: refused,
        })
        .unwrap();
    let mut found = codes(outcome);
    found.sort();
    assert_eq!(
        found,
        [
            "api_route_unknown",
            "backend_not_allowed",
            "description_too_long",
            "git_commit_requires_pulsation_done",
            "model_required",
            "stage_unknown"
        ]
    );
}

#[test]
fn gates_wait_for_the_developer_and_a_rejection_ends_the_run() {
    let f = fixture();
    let pipeline = f.create("Approvals", vec![gate("first"), gate("second")]);
    let run = f
        .service
        .start_custom_pipeline_run(&f.project, &pipeline.id)
        .unwrap();
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.status, PipelineRunStatus::WaitingGate);
    assert_eq!(view.step(0).unwrap().status, StepRunStatus::WaitingGate);
    let decide = |index: u32, approve: bool, note: Option<String>| {
        f.service
            .decide_custom_pipeline_gate(DecidePipelineGateRequest {
                project_id: f.project.clone(),
                run_id: run.clone(),
                step_index: index,
                approve,
                note,
            })
    };
    // The wrong step and an oversized note are refused.
    assert!(decide(1, true, None).is_err());
    assert!(decide(0, true, Some("n".repeat(2049))).is_err());
    decide(0, true, Some("  Looks right.  ".into())).unwrap();
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.status, PipelineRunStatus::WaitingGate);
    assert_eq!(view.current_step, 1);
    let first = view.step(0).unwrap();
    assert_eq!(first.status, StepRunStatus::Passed);
    assert_eq!(first.gate_decision.as_deref(), Some("approved"));
    assert_eq!(first.gate_feedback.as_deref(), Some("Looks right."));
    decide(1, false, None).unwrap();
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.status, PipelineRunStatus::Failed);
    assert_eq!(view.error.as_deref(), Some("Rejected at Gate second."));
    assert_eq!(view.step(1).unwrap().status, StepRunStatus::Rejected);
    assert!(view.finished_at.is_some());
    assert!(decide(1, true, None).is_err());
}

#[test]
fn a_run_keeps_the_definition_it_started_with() {
    let f = fixture();
    let pipeline = f.create("Pinned", vec![gate("first"), agent("review")]);
    let run = f
        .service
        .start_custom_pipeline_run(&f.project, &pipeline.id)
        .unwrap();
    let SaveCustomPipelineOutcome::Saved { pipeline: edited } = f.save(
        Some(&pipeline),
        "Pinned",
        vec![gate("first"), gate("other")],
    ) else {
        panic!("edit refused");
    };
    assert_eq!(edited.current_version, 2);
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.version, 1);
    assert_eq!(view.definition.steps[1].id(), "review");
    f.service
        .decide_custom_pipeline_gate(DecidePipelineGateRequest {
            project_id: f.project.clone(),
            run_id: run.clone(),
            step_index: 0,
            approve: true,
            note: None,
        })
        .unwrap();
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    // The next step is version 1's agent, queued for the lane.
    assert_eq!(view.status, PipelineRunStatus::Running);
    let next = view.step(1).unwrap();
    assert_eq!(
        (next.step_id.as_str(), next.status),
        ("review", StepRunStatus::Queued)
    );
}

#[test]
fn cancel_closes_the_run_and_its_waiting_steps() {
    let f = fixture();
    let pipeline = f.create("Cancelled", vec![gate("first")]);
    let run = f
        .service
        .start_custom_pipeline_run(&f.project, &pipeline.id)
        .unwrap();
    f.service
        .cancel_custom_pipeline_run(&f.project, &run)
        .unwrap();
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.status, PipelineRunStatus::Cancelled);
    assert_eq!(view.step(0).unwrap().status, StepRunStatus::Cancelled);
    assert!(
        f.service
            .cancel_custom_pipeline_run(&f.project, &run)
            .is_err()
    );
    // Archiving waits for no run once it ended; a run in progress blocks it.
    let busy = f.create("Busy", vec![gate("first")]);
    f.service
        .start_custom_pipeline_run(&f.project, &busy.id)
        .unwrap();
    assert_eq!(
        codes(
            f.service
                .set_custom_pipeline_archived(&f.project, &busy.id, true)
                .unwrap()
        ),
        ["run_in_progress"]
    );
}

#[test]
fn the_lane_admits_one_step_per_project_within_the_ceiling() {
    let f = fixture();
    let other_workspace = tempfile::tempdir().unwrap();
    let other = f
        .service
        .create_project(CreateProjectRequest {
            workspace_path: other_workspace.path().to_string_lossy().into_owned(),
            name: Some("Other".into()),
        })
        .unwrap()
        .project
        .id;
    let first = f.create("First", vec![agent("a")]);
    let second = f.create("Second", vec![agent("a")]);
    let first_run = f
        .service
        .start_custom_pipeline_run(&f.project, &first.id)
        .unwrap();
    let second_run = f
        .service
        .start_custom_pipeline_run(&f.project, &second.id)
        .unwrap();
    let foreign = match f
        .service
        .save_custom_pipeline(SaveCustomPipelineRequest {
            project_id: other.clone(),
            pipeline_id: None,
            expected_version: None,
            scope: None,
            parent_pipeline_id: None,
            name: "Foreign".into(),
            description: String::new(),
            definition: definition(vec![agent("a")]),
        })
        .unwrap()
    {
        SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
        SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
    };
    let foreign_run = f
        .service
        .start_custom_pipeline_run(&other, &foreign.id)
        .unwrap();
    f.service.set_automation_max_concurrent(1).unwrap();
    assert!(f.service.set_automation_max_concurrent(4).is_err());
    let claim = f.service.claim_next_automation_step().unwrap().unwrap();
    assert_eq!(claim.run_id, first_run);
    // The app-wide ceiling of one holds the other project back.
    assert!(f.service.claim_next_automation_step().unwrap().is_none());
    f.service.set_automation_max_concurrent(2).unwrap();
    // One step per project: the second run of this project waits.
    let next = f.service.claim_next_automation_step().unwrap().unwrap();
    assert_eq!(
        (next.run_id.as_str(), next.project_id.as_str()),
        (foreign_run.as_str(), other.as_str())
    );
    assert!(f.service.claim_next_automation_step().unwrap().is_none());
    let second_view = f
        .service
        .custom_pipeline_run(&f.project, &second_run)
        .unwrap();
    assert_eq!(second_view.status, PipelineRunStatus::Queued);
    let first_view = f
        .service
        .custom_pipeline_run(&f.project, &first_run)
        .unwrap();
    assert_eq!(first_view.status, PipelineRunStatus::Running);
    assert_eq!(first_view.step(0).unwrap().status, StepRunStatus::Running);
    // The main lane's places are untouched.
    let main: u32 = f
        .db()
        .query_row("SELECT count(*) FROM runtime_slots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(main, 0);
}

#[test]
fn a_closed_app_interrupts_running_steps_and_resume_starts_a_new_attempt() {
    let f = fixture();
    let pipeline = f.create("Long", vec![agent("a"), gate("ok")]);
    let run = f
        .service
        .start_custom_pipeline_run(&f.project, &pipeline.id)
        .unwrap();
    let cancelled = f.create("Gone", vec![agent("a")]);
    let cancelled_run = f
        .service
        .start_custom_pipeline_run(&f.project, &cancelled.id)
        .unwrap();
    f.service.claim_next_automation_step().unwrap().unwrap();
    // The other run is cancelled while its step "runs" (a second place).
    f.db()
        .execute(
            "UPDATE custom_pipeline_step_runs SET status='running' WHERE run_id=?1",
            [&cancelled_run],
        )
        .unwrap();
    f.db()
        .execute(
            "UPDATE custom_pipeline_runs SET status='cancelled' WHERE id=?1",
            [&cancelled_run],
        )
        .unwrap();
    assert!(
        f.service
            .resume_custom_pipeline_run(&f.project, &run)
            .is_err()
    );
    f.service.recover_automation_lane().unwrap();
    let slots: u32 = f
        .db()
        .query_row("SELECT count(*) FROM automation_slots", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(slots, 0);
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.status, PipelineRunStatus::Interrupted);
    assert_eq!(view.step(0).unwrap().status, StepRunStatus::Interrupted);
    let gone = f
        .service
        .custom_pipeline_run(&f.project, &cancelled_run)
        .unwrap();
    assert_eq!(gone.step(0).unwrap().status, StepRunStatus::Cancelled);
    f.service
        .resume_custom_pipeline_run(&f.project, &run)
        .unwrap();
    let view = f.service.custom_pipeline_run(&f.project, &run).unwrap();
    assert_eq!(view.status, PipelineRunStatus::Queued);
    let retry = view.step(0).unwrap();
    assert_eq!((retry.attempt, retry.status), (2, StepRunStatus::Queued));
    let claim = f.service.claim_next_automation_step().unwrap().unwrap();
    assert_eq!(claim.run_id, run);
}

#[test]
fn v69_databases_keep_their_slots_runs_and_executions() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join(crate::app_names::DATABASE_FILE_NAME);
    let mut db = Connection::open(&path).unwrap();
    embedded::migrations::runner()
        .set_target(refinery::Target::Version(69))
        .run(&mut db)
        .unwrap();
    let now = "2026-11-22T00:00:00Z";
    db.execute(
        "INSERT INTO projects(id,name,workspace_path,created_at,updated_at)
         VALUES('p69','Project',?1,?2,?2)",
        params![data.path().to_string_lossy(), now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO provider_routes(id,name,created_at,updated_at)
         VALUES('route-69','Route V69',?1,?1)",
        [now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,created_at,updated_at)
         VALUES('free-69','p69','free_form',NULL,'Free',?1,?1)",
        [now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,free_form_slot_id,free_form_label,
           origin,execution_mode,backend,status,created_at,updated_at,slot_id)
         VALUES('run-69','p69','free_form',NULL,'free-69','Free','manual','cli_process','codex',
           'completed',?1,?1,'free-69')",
        [now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,agent_role,stage,
           strategy,mode,status,decision_json,started_at)
         VALUES('exec-69','gen','p69','route-69','backend_agent','Code','priority','on',
           'completed','{}',?1)",
        [now],
    )
    .unwrap();
    drop(db);
    let service = PersistenceService::initialize(data.path()).unwrap();
    let db = service.connection().unwrap();
    let kept: (String, String, String) = db
        .query_row(
            "SELECT s.kind,r.origin,e.agent_role FROM terminal_slots s
               JOIN terminal_runs r ON r.slot_id=s.id
               JOIN llm_route_executions e ON e.project_id=s.project_id
              WHERE s.id='free-69'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        kept,
        ("free_form".into(), "manual".into(), "backend_agent".into())
    );
    // The new kind and origins are accepted, with no role.
    db.execute(
        "INSERT INTO terminal_slots(id,project_id,kind,agent_role,label,created_at,updated_at)
         VALUES('automation:p69:x:y','p69','automation',NULL,'X \u{b7} Y',?1,?1)",
        [now],
    )
    .unwrap();
    for (id, origin) in [("run-a", "automation"), ("run-v", "verification")] {
        db.execute(
            "INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,free_form_slot_id,
               origin,execution_mode,backend,status,created_at,updated_at,slot_id)
             VALUES(?1,'p69','automation',NULL,NULL,?2,'cli_process','codex','completed',?3,?3,
               'automation:p69:x:y')",
            params![id, origin, now],
        )
        .unwrap();
    }
    // An automation run names no role and no free-form slot.
    assert!(
        db.execute(
            "INSERT INTO terminal_runs(id,project_id,slot_kind,agent_role,free_form_slot_id,
               origin,execution_mode,backend,status,created_at,updated_at,slot_id)
             VALUES('run-bad','p69','automation','backend_agent',NULL,'automation','cli_process',
               'codex','completed',?1,?1,'automation:p69:x:y')",
            [now],
        )
        .is_err()
    );
    db.execute(
        "INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,stage,strategy,mode,
           status,decision_json,started_at,automation_slot_id)
         VALUES('exec-a','gen','p69','route-69','Automation','priority','on','running','{}',?1,
           'automation:p69:x:y')",
        [now],
    )
    .unwrap();
    // Exactly one target per execution.
    assert!(
        db.execute(
            "INSERT INTO llm_route_executions(id,generation_id,project_id,route_id,agent_role,stage,
               strategy,mode,status,decision_json,started_at,automation_slot_id)
             VALUES('exec-b','gen','p69','route-69','backend_agent','Automation','priority',
               'on','running','{}',?1,'automation:p69:x:y')",
            [now],
        )
        .is_err()
    );
    let violations: u32 = db
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

// --- M2b: global pipelines, children and adoption -------------------------

pub(super) fn second_project(f: &Fixture, name: &str) -> (tempfile::TempDir, String) {
    let workspace = tempfile::tempdir().unwrap();
    let id = f
        .service
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some(name.into()),
        })
        .unwrap()
        .project
        .id;
    (workspace, id)
}

fn agent_with(id: &str, instructions: &str) -> PipelineStep {
    let PipelineStep::Agent(mut step) = agent(id) else {
        unreachable!()
    };
    step.instructions = instructions.into();
    PipelineStep::Agent(step)
}

fn instructions_of(view: &CustomPipelineView, id: &str) -> String {
    view.definition
        .steps
        .iter()
        .find_map(|step| match step {
            PipelineStep::Agent(agent) if agent.id == id => Some(agent.instructions.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no step {id}"))
}

impl Fixture {
    pub(super) fn create_in(
        &self,
        project: &str,
        name: &str,
        scope: &str,
        parent: Option<&str>,
        steps: Vec<PipelineStep>,
    ) -> SaveCustomPipelineOutcome {
        self.service
            .save_custom_pipeline(SaveCustomPipelineRequest {
                project_id: project.into(),
                pipeline_id: None,
                expected_version: None,
                scope: Some(scope.into()),
                parent_pipeline_id: parent.map(str::to_owned),
                name: name.into(),
                description: String::new(),
                definition: definition(steps),
            })
            .unwrap()
    }

    fn edit_in(
        &self,
        project: &str,
        view: &CustomPipelineView,
        steps: Vec<PipelineStep>,
    ) -> CustomPipelineView {
        saved(
            self.service
                .save_custom_pipeline(SaveCustomPipelineRequest {
                    project_id: project.into(),
                    pipeline_id: Some(view.id.clone()),
                    expected_version: Some(view.current_version),
                    scope: None,
                    parent_pipeline_id: None,
                    name: view.name.clone(),
                    description: view.description.clone(),
                    definition: definition(steps),
                })
                .unwrap(),
        )
    }

    pub(super) fn shown(&self, project: &str, id: &str) -> CustomPipelineView {
        let snapshot = self.service.custom_pipelines_snapshot(project).unwrap();
        snapshot
            .pipelines
            .into_iter()
            .chain(snapshot.archived)
            .find(|view| view.id == id)
            .unwrap_or_else(|| panic!("{id} not shown"))
    }

    pub(super) fn open(&self, project: &str) {
        self.service
            .open_project(super::OpenProjectRequest {
                project_id: project.into(),
            })
            .unwrap();
    }
}

pub(super) fn saved(outcome: SaveCustomPipelineOutcome) -> CustomPipelineView {
    match outcome {
        SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
        SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
    }
}

#[test]
fn a_global_pipeline_serves_each_project_that_adds_it_and_is_adopted_when_it_opens() {
    let f = fixture();
    let (_other_workspace, other) = second_project(&f, "Other");
    let global = saved(f.create_in(
        &f.project,
        "Shared review",
        "global",
        None,
        vec![agent_with("review", "First wording.")],
    ));
    assert_eq!(
        (global.scope.as_str(), global.project_id.as_deref()),
        ("global", None)
    );
    let offered = f.service.custom_pipelines_snapshot(&other).unwrap();
    assert!(offered.pipelines.is_empty());
    let offered_user_globals: Vec<_> = offered
        .available_globals
        .iter()
        .filter(|g| !g.is_builtin)
        .collect();
    assert_eq!(offered_user_globals.len(), 1);
    assert_eq!(offered_user_globals[0].id, global.id);
    assert!(
        offered
            .available_globals
            .iter()
            .filter(|g| g.is_builtin)
            .count()
            == 5
    );
    let added = saved(
        f.service
            .activate_custom_pipeline(&other, &global.id)
            .unwrap(),
    );
    assert_eq!((added.adopted_version, added.projects), (1, 2));
    assert!(
        f.service
            .custom_pipelines_snapshot(&other)
            .unwrap()
            .available_globals
            .into_iter()
            .filter(|g| !g.is_builtin)
            .collect::<Vec<_>>()
            .is_empty()
    );
    // A run in the other project before it adopts the edit follows v1 (T4).
    let run = f
        .service
        .start_custom_pipeline_run(&other, &global.id)
        .unwrap();

    // Edited where it is open: adopted there at once (P2) ...
    let edited = f.edit_in(
        &f.project,
        &global,
        vec![agent_with("review", "Second wording.")],
    );
    assert_eq!((edited.current_version, edited.adopted_version), (2, 2));
    // ... and by the other project only when it opens.
    let waiting = f.shown(&other, &global.id);
    assert_eq!((waiting.adopted_version, waiting.current_version), (1, 2));
    assert!(waiting.behind());
    assert_eq!(instructions_of(&waiting, "review"), "First wording.");
    f.open(&other);
    let adopted = f.shown(&other, &global.id);
    assert_eq!(adopted.adopted_version, 2);
    assert_eq!(instructions_of(&adopted, "review"), "Second wording.");
    let run = f.service.custom_pipeline_run(&other, &run).unwrap();
    assert_eq!(run.version, 1);

    // Removing it from a project keeps the pipeline and the other box; a
    // global pipeline used elsewhere is not deleted from one project.
    f.service
        .cancel_custom_pipeline_run(&other, &run.id)
        .unwrap();
    f.service
        .deactivate_custom_pipeline(&other, &global.id)
        .unwrap();
    assert!(
        f.service
            .custom_pipelines_snapshot(&other)
            .unwrap()
            .pipelines
            .is_empty()
    );
    assert_eq!(f.shown(&f.project, &global.id).projects, 1);
    saved(
        f.service
            .activate_custom_pipeline(&other, &global.id)
            .unwrap(),
    );
    assert!(
        f.service
            .delete_custom_pipeline(&f.project, &global.id)
            .is_err()
    );
    assert!(
        f.service
            .deactivate_custom_pipeline(&f.project, &f.create("Own", vec![agent("a")]).id)
            .is_err()
    );
}

#[test]
fn a_child_inherits_its_parent_and_needs_edit_when_its_changes_no_longer_fit() {
    let f = fixture();
    let parent = saved(f.create_in(
        &f.project,
        "Base",
        "global",
        None,
        vec![agent_with("a", "Parent a."), agent_with("b", "Parent b.")],
    ));
    // The child changes `b` and adds `c` after it.
    let child = saved(f.create_in(
        &f.project,
        "Base plus",
        "project",
        Some(&parent.id),
        vec![
            agent_with("a", "Parent a."),
            agent_with("b", "Child b."),
            agent_with("c", "Child c."),
        ],
    ));
    assert_eq!(
        child.parent_pipeline_id.as_deref(),
        Some(parent.id.as_str())
    );
    assert_eq!(child.parent_name.as_deref(), Some("Base"));
    assert_eq!(child.adopted_parent_version, Some(1));
    assert_eq!(f.shown(&f.project, &parent.id).children, 1);

    // A change of the parent the child does not touch is inherited.
    let parent = f.edit_in(
        &f.project,
        &parent,
        vec![
            agent_with("a", "Parent a, v2."),
            agent_with("b", "Parent b."),
        ],
    );
    let inherited = f.shown(&f.project, &child.id);
    assert!(inherited.ready, "{:?}", inherited.needs_edit_reason);
    assert_eq!(inherited.adopted_parent_version, Some(2));
    assert_eq!(instructions_of(&inherited, "a"), "Parent a, v2.");
    assert_eq!(instructions_of(&inherited, "b"), "Child b.");
    assert_eq!(instructions_of(&inherited, "c"), "Child c.");

    // The parent drops `b`, which the child changes and follows: needs edit.
    let parent = f.edit_in(&f.project, &parent, vec![agent_with("a", "Parent a, v2.")]);
    let broken = f.shown(&f.project, &child.id);
    assert!(!broken.ready);
    assert!(
        broken
            .needs_edit_reason
            .as_deref()
            .unwrap()
            .contains("\"b\"")
    );
    assert!(
        f.service
            .start_custom_pipeline_run(&f.project, &child.id)
            .is_err()
    );

    // Saving the child against the new parent fixes it.
    let fixed = f.edit_in(
        &f.project,
        &broken,
        vec![
            agent_with("a", "Parent a, v2."),
            agent_with("c", "Child c."),
        ],
    );
    assert!(fixed.ready);
    f.service
        .start_custom_pipeline_run(&f.project, &child.id)
        .unwrap();

    // A parent with children is not deleted; a detached child keeps what it
    // resolved to.
    assert!(
        f.service
            .delete_custom_pipeline(&f.project, &parent.id)
            .is_err()
    );
    let detached = saved(
        f.service
            .detach_custom_pipeline(&f.project, &child.id)
            .unwrap(),
    );
    assert_eq!(detached.parent_pipeline_id, None);
    assert_eq!(detached.definition.steps.len(), 2);
    assert_eq!(instructions_of(&detached, "c"), "Child c.");
    assert_eq!(f.shown(&f.project, &parent.id).children, 0);
}

#[test]
fn global_limits_scope_moves_and_project_parents_are_enforced() {
    let f = fixture();
    let (_other_workspace, other) = second_project(&f, "Other");
    // P3: a child of a project's pipeline stays in that project.
    let own = f.create("Own", vec![agent("a")]);
    assert_eq!(
        codes(f.create_in(
            &f.project,
            "Global child",
            "global",
            Some(&own.id),
            vec![agent("a")]
        )),
        ["parent_in_project"]
    );
    assert_eq!(
        codes(f.create_in(
            &other,
            "Foreign child",
            "project",
            Some(&own.id),
            vec![agent("a")]
        )),
        ["parent_in_project"]
    );
    let child = saved(f.create_in(
        &f.project,
        "Own child",
        "project",
        Some(&own.id),
        vec![agent("a")],
    ));
    assert_eq!(
        codes(
            f.service
                .make_custom_pipeline_global(&f.project, &child.id)
                .unwrap()
        ),
        ["parent_in_project"]
    );
    // No grandchildren.
    assert_eq!(
        codes(f.create_in(
            &f.project,
            "Grandchild",
            "project",
            Some(&child.id),
            vec![agent("a")]
        )),
        ["child_of_child"]
    );

    // Make global, then move back while only this project shows it.
    let made = saved(
        f.service
            .make_custom_pipeline_global(&f.project, &own.id)
            .unwrap(),
    );
    assert_eq!(
        (made.scope.as_str(), made.project_id.as_deref()),
        ("global", None)
    );
    saved(f.service.activate_custom_pipeline(&other, &own.id).unwrap());
    assert_eq!(
        codes(
            f.service
                .move_custom_pipeline_to_project(&f.project, &own.id)
                .unwrap()
        ),
        ["active_elsewhere"]
    );
    f.service
        .deactivate_custom_pipeline(&other, &own.id)
        .unwrap();
    let moved = saved(
        f.service
            .move_custom_pipeline_to_project(&f.project, &own.id)
            .unwrap(),
    );
    assert_eq!(moved.project_id.as_deref(), Some(f.project.as_str()));
    let made = saved(
        f.service
            .make_custom_pipeline_global(&f.project, &own.id)
            .unwrap(),
    );
    assert!(made.is_global());

    // At most five global pipelines: `Own` and four more.
    for index in 1..MAX_GLOBAL_PIPELINES {
        saved(f.create_in(
            &other,
            &format!("Global {index}"),
            "global",
            None,
            vec![agent("a")],
        ));
    }
    assert_eq!(
        codes(f.create_in(&other, "One more", "global", None, vec![agent("a")])),
        ["global_limit"]
    );
    // At most seven boxes in a project: three of its own and four globals
    // besides `Own`.
    f.create("Extra", vec![agent("a")]);
    let globals: Vec<_> = f
        .service
        .custom_pipelines_snapshot(&f.project)
        .unwrap()
        .available_globals
        .into_iter()
        .filter(|g| !g.is_builtin)
        .collect();
    assert_eq!(globals.len(), MAX_GLOBAL_PIPELINES - 1);
    for global in &globals {
        saved(
            f.service
                .activate_custom_pipeline(&f.project, &global.id)
                .unwrap(),
        );
    }
    assert_eq!(
        f.service
            .custom_pipelines_snapshot(&f.project)
            .unwrap()
            .pipelines
            .len(),
        MAX_ACTIVE_PIPELINES
    );
    assert_eq!(
        codes(f.save(None, "Eighth", vec![agent("a")])),
        ["active_limit"]
    );
}

#[test]
fn deleting_a_project_keeps_the_global_pipelines_it_used() {
    let f = fixture();
    let (_other_workspace, other) = second_project(&f, "Doomed");
    let global = saved(f.create_in(&other, "Kept", "global", None, vec![agent("a")]));
    saved(f.create_in(&other, "Gone", "project", None, vec![agent("a")]));
    let child = saved(f.create_in(
        &other,
        "Gone child",
        "project",
        Some(&global.id),
        vec![agent("a")],
    ));
    saved(
        f.service
            .activate_custom_pipeline(&f.project, &global.id)
            .unwrap(),
    );
    f.service
        .delete_project_permanently(&other, "Doomed")
        .unwrap();
    let kept = f.shown(&f.project, &global.id);
    assert_eq!((kept.projects, kept.children), (1, 0));
    let remaining: u32 = f
        .db()
        .query_row(
            "SELECT count(*) FROM custom_pipelines WHERE id=?1",
            params![child.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

fn run_git_test_cmd(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git command");
    assert!(
        out.status.success(),
        "git {:?} failed: stdout={}, stderr={}",
        args,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn sha256_hex_str(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn seed_done_pulsation_for_pipeline(
    service: &PersistenceService,
    project_id: &str,
    repo: &std::path::Path,
    pulsation_id: &str,
    completed_at: &str,
    file_rel: &str,
    file_content: &str,
    test_rel: &str,
    test_content: &str,
) {
    let db = service.connection().unwrap();
    db.execute(
        "INSERT INTO pulsations(id, project_id, feature_name, stage, status, created_at, updated_at, queue_sequence)
         VALUES (?1, ?2, ?3, 'Integrator', 'done', ?4, ?4, (SELECT COALESCE(MAX(queue_sequence),0)+1 FROM pulsations WHERE project_id=?2))",
        params![pulsation_id, project_id, format!("Feature {pulsation_id}"), completed_at],
    )
    .unwrap();
    let job_code = format!("job-code-{pulsation_id}");
    let job_test = format!("job-test-{pulsation_id}");
    for (jid, stage) in [(&job_code, "Code"), (&job_test, "Test")] {
        db.execute(
            "INSERT INTO jobs(id, pulsation_id, kind, status, created_at, updated_at, project_id, input_revision, generation, state_version, job_class, idempotency_key)
             VALUES (?1, ?2, 'stage_run', 'done', ?3, ?3, ?4, 1, 1, 1, 'main', ?5)",
            params![jid, pulsation_id, completed_at, project_id, format!("idem:{jid}")],
        )
        .unwrap();
        let _ = stage;
    }
    let items = [
        (
            format!("art-c-{pulsation_id}"),
            file_rel,
            file_content,
            "Code",
            job_code,
        ),
        (
            format!("art-t-{pulsation_id}"),
            test_rel,
            test_content,
            "Test",
            job_test,
        ),
    ];
    for (aid, rel, content, stage, jid) in items {
        let sha = sha256_hex_str(content);
        db.execute(
            "INSERT INTO artifacts(
                id, pulsation_id, name, kind, relative_path, content_sha256, created_at, updated_at,
                content, producer_job_id, producer_stage, generation, input_revision,
                producer_state_version, output_position, acceptance, publication_state
             ) VALUES (?1, ?2, ?3, 'file', ?3, ?4, ?5, ?5, ?6, ?7, ?8, 1, 1, 1, 0, 'accepted', 'applied')",
            params![aid, pulsation_id, rel, sha, completed_at, content, jid, stage],
        )
        .unwrap();
        db.execute(
            "INSERT INTO artifact_publications(
                artifact_id, project_id, pulsation_id, relative_path, expected_sha256, desired_sha256, state, updated_at
             ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'applied', ?6)",
            params![aid, project_id, pulsation_id, rel, sha, completed_at],
        )
        .unwrap();
        db.execute(
            "INSERT INTO pulsation_file_bases(
                pulsation_id, path_key, relative_path, state, content_sha256, content, first_artifact_id, captured_at
             ) VALUES (?1, ?2, ?3, 'absent', NULL, NULL, ?4, ?5)",
            params![pulsation_id, rel.to_ascii_lowercase(), rel, aid, completed_at],
        )
        .unwrap();
        let full = repo.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }
}

#[tokio::test]
async fn host_tool_commit_and_push_pipeline_runs_in_automation_lane_and_links_git_operation() {
    let bare = tempfile::tempdir().unwrap();
    run_git_test_cmd(bare.path(), &["init", "--bare", "-b", "main"]);
    let bare_str = bare.path().to_string_lossy().replace('\\', "/");

    let repo = tempfile::tempdir().unwrap();
    run_git_test_cmd(repo.path(), &["init", "-b", "main"]);
    run_git_test_cmd(repo.path(), &["config", "user.name", "Pipeline Tester"]);
    run_git_test_cmd(repo.path(), &["config", "user.email", "pipe@example.com"]);
    run_git_test_cmd(repo.path(), &["config", "core.autocrlf", "false"]);
    std::fs::write(repo.path().join("README.md"), "# Init\n").unwrap();
    run_git_test_cmd(repo.path(), &["add", "--all"]);
    run_git_test_cmd(repo.path(), &["commit", "-m", "chore: init"]);
    run_git_test_cmd(repo.path(), &["remote", "add", "origin", &bare_str]);
    run_git_test_cmd(repo.path(), &["push", "-u", "origin", "main"]);

    let data = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = service
        .create_project(CreateProjectRequest {
            workspace_path: repo.path().to_string_lossy().into_owned(),
            name: Some("HostTool M5".into()),
        })
        .unwrap()
        .project
        .id;

    // Enable git tool for the project.
    let outcome = service
        .save_project_git_rules(crate::git_tool::SaveProjectGitRulesRequest {
            project_id: project.clone(),
            enabled: true,
            rules: crate::git_tool::CommitRulesV1::default(),
        })
        .unwrap();
    assert!(matches!(
        outcome,
        crate::git_tool::SaveProjectGitRulesOutcome::Saved(_)
    ));

    // Seed two done pulsations in completion order (P8 test).
    seed_done_pulsation_for_pipeline(
        &service,
        &project,
        repo.path(),
        "pulse-stack-1",
        "2026-11-26T00:01:00Z",
        "src/one.rs",
        "pub fn one() {}\n",
        "tests/one_test.rs",
        "#[test] fn one_ok() {}\n",
    );
    seed_done_pulsation_for_pipeline(
        &service,
        &project,
        repo.path(),
        "pulse-stack-2",
        "2026-11-26T00:02:00Z",
        "src/two.rs",
        "pub fn two() {}\n",
        "tests/two_test.rs",
        "#[test] fn two_ok() {}\n",
    );

    let done_def = CustomPipelineDefinitionV1 {
        schema_version: 1,
        trigger: PipelineTrigger::DefaultEvent {
            event: crate::custom_pipelines::DefaultEvent::PulsationDone,
            stage: None,
        },
        steps: vec![
            PipelineStep::HostTool(HostToolStep {
                id: "commit-step".into(),
                label: "Commit".into(),
                tool: HostTool::GitCommit,
            }),
            PipelineStep::HostTool(HostToolStep {
                id: "push-step".into(),
                label: "Push".into(),
                tool: HostTool::GitPush,
            }),
        ],
    };
    let saved_pipe = match service
        .save_custom_pipeline(SaveCustomPipelineRequest {
            project_id: project.clone(),
            pipeline_id: None,
            expected_version: None,
            scope: Some("global".into()),
            parent_pipeline_id: None,
            name: "Commit and push".into(),
            description: String::new(),
            definition: done_def,
        })
        .unwrap()
    {
        SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
        SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
    };

    // Global pipeline with git.commit cannot be overridden to Manual trigger.
    let bad_override = service
        .set_custom_pipeline_trigger(crate::custom_pipelines::SetPipelineTriggerRequest {
            project_id: project.clone(),
            pipeline_id: saved_pipe.id.clone(),
            trigger: Some(PipelineTrigger::Manual),
        })
        .unwrap();
    assert_eq!(codes(bad_override), ["git_commit_requires_pulsation_done"]);

    // Enqueue a PulsationDone outbox event for pulse-stack-2 and dispatch.
    {
        let db = service.connection().unwrap();
        db.execute(
            "INSERT INTO pipeline_trigger_outbox(id,project_id,pulsation_id,event,stage,state_version,state,created_at)
             VALUES('outbox-m5',?1,'pulse-stack-2','pulsation_done',NULL,1,'pending','2026-11-26T00:02:01Z')",
            [&project],
        )
        .unwrap();
    }
    service.dispatch_pipeline_triggers().unwrap();

    let runs = service.custom_pipelines_snapshot(&project).unwrap().runs;
    assert_eq!(runs.len(), 1);
    let run_id = runs[0].id.clone();

    let shutdown = tokio_util::sync::CancellationToken::new();
    // Execute step 0: GitCommit (must commit pulse-stack-1 first, then pulse-stack-2 stacked).
    let claim0 = service.claim_next_automation_step().unwrap().unwrap();
    service.run_automation_step(claim0, shutdown.clone()).await;
    // Execute step 1: GitPush.
    let claim1 = service.claim_next_automation_step().unwrap().unwrap();
    service.run_automation_step(claim1, shutdown).await;

    let run = service.custom_pipeline_run(&project, &run_id).unwrap();
    assert_eq!(
        run.status,
        PipelineRunStatus::Done,
        "run error: {:?}",
        run.error
    );
    let s0 = run.step(0).unwrap();
    let s1 = run.step(1).unwrap();
    assert_eq!(s0.status, StepRunStatus::Done);
    assert!(s0.git_operation_id.is_some());
    assert_eq!(s1.status, StepRunStatus::Done);
    assert!(s1.git_operation_id.is_some());

    // Queue entries are committed and remote bare repo received the second branch.
    let queue = service.list_pulsation_commit_queue(&project).unwrap();
    assert_eq!(queue.len(), 2);
    assert_eq!(
        queue[0].state,
        crate::git_tool::PulsationCommitQueueState::Committed
    );
    assert_eq!(
        queue[1].state,
        crate::git_tool::PulsationCommitQueueState::Committed
    );
    let remote_heads = run_git_test_cmd(
        bare.path(),
        &["for-each-ref", "refs/heads", "--format=%(refname:short)"],
    );
    let p2_branch = queue[1].branch.as_deref().unwrap();
    assert!(remote_heads.lines().any(|b| b == p2_branch));
}

#[tokio::test]
async fn host_tool_pull_step_ignores_its_own_automation_slot_and_fast_forwards() {
    let bare = tempfile::tempdir().unwrap();
    run_git_test_cmd(bare.path(), &["init", "--bare", "-b", "main"]);
    let bare_str = bare.path().to_string_lossy().replace('\\', "/");

    let repo = tempfile::tempdir().unwrap();
    run_git_test_cmd(repo.path(), &["init", "-b", "main"]);
    run_git_test_cmd(repo.path(), &["config", "user.name", "Pull Step Tester"]);
    run_git_test_cmd(repo.path(), &["config", "user.email", "pull@example.com"]);
    std::fs::write(repo.path().join("file.txt"), "one\n").unwrap();
    run_git_test_cmd(repo.path(), &["add", "--all"]);
    run_git_test_cmd(repo.path(), &["commit", "-m", "chore: initial"]);
    run_git_test_cmd(repo.path(), &["remote", "add", "origin", &bare_str]);
    run_git_test_cmd(repo.path(), &["switch", "-c", "feat/sync"]);
    run_git_test_cmd(repo.path(), &["push", "-u", "origin", "feat/sync"]);

    let writer = tempfile::tempdir().unwrap();
    run_git_test_cmd(writer.path(), &["clone", "-b", "feat/sync", &bare_str, "."]);
    run_git_test_cmd(writer.path(), &["config", "user.name", "Writer"]);
    run_git_test_cmd(
        writer.path(),
        &["config", "user.email", "writer@example.com"],
    );
    std::fs::write(writer.path().join("file.txt"), "two\n").unwrap();
    run_git_test_cmd(writer.path(), &["add", "--all"]);
    run_git_test_cmd(writer.path(), &["commit", "-m", "feat: upstream change"]);
    run_git_test_cmd(writer.path(), &["push", "origin", "feat/sync"]);
    let expected_head = run_git_test_cmd(writer.path(), &["rev-parse", "HEAD"]);

    let data = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = service
        .create_project(CreateProjectRequest {
            workspace_path: repo.path().to_string_lossy().into_owned(),
            name: Some("Pull Step M5".into()),
        })
        .unwrap()
        .project
        .id;
    service
        .save_project_git_rules(crate::git_tool::SaveProjectGitRulesRequest {
            project_id: project.clone(),
            enabled: true,
            rules: crate::git_tool::CommitRulesV1::default(),
        })
        .unwrap();

    let saved = match service
        .save_custom_pipeline(SaveCustomPipelineRequest {
            project_id: project.clone(),
            pipeline_id: None,
            expected_version: None,
            scope: Some("project".into()),
            parent_pipeline_id: None,
            name: "Sync pull".into(),
            description: String::new(),
            definition: CustomPipelineDefinitionV1 {
                schema_version: 1,
                trigger: PipelineTrigger::Manual,
                steps: vec![PipelineStep::HostTool(HostToolStep {
                    id: "pull-step".into(),
                    label: "Pull".into(),
                    tool: HostTool::GitPull,
                })],
            },
        })
        .unwrap()
    {
        SaveCustomPipelineOutcome::Saved { pipeline } => *pipeline,
        SaveCustomPipelineOutcome::Invalid { errors } => panic!("{errors:?}"),
    };

    let run_id = service
        .start_custom_pipeline_run(&project, &saved.id)
        .unwrap();
    let claim = service.claim_next_automation_step().unwrap().unwrap();
    service
        .run_automation_step(claim, tokio_util::sync::CancellationToken::new())
        .await;

    let run = service.custom_pipeline_run(&project, &run_id).unwrap();
    assert_eq!(
        run.status,
        PipelineRunStatus::Done,
        "run error: {:?}",
        run.error
    );
    assert!(run.step(0).unwrap().git_operation_id.is_some());
    assert_eq!(
        run_git_test_cmd(repo.path(), &["rev-parse", "HEAD"]),
        expected_head
    );
}

#[test]
fn builtin_default_pipelines_seed_idempotently_and_keep_user_global_quota() {
    let f = fixture();
    let snapshot = f.service.custom_pipelines_snapshot(&f.project).unwrap();
    let builtins: Vec<_> = snapshot
        .available_globals
        .iter()
        .filter(|g| g.is_builtin)
        .collect();
    assert_eq!(builtins.len(), 5);
    assert!(
        snapshot
            .available_globals
            .iter()
            .take(5)
            .all(|g| g.is_builtin)
    );

    // Reopening leaves the five templates unique and preserves zero user global count.
    drop(snapshot);
    let _reopened = PersistenceService::initialize(f._data.path()).unwrap();
    let db = f.db();
    let template_count: i64 = db
        .query_row(
            "SELECT count(*) FROM custom_pipelines WHERE template_key IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(template_count, 5);

    // User can still create all MAX_GLOBAL_PIPELINES user global pipelines.
    for idx in 0..MAX_GLOBAL_PIPELINES {
        let out = f.create_in(
            &f.project,
            &format!("User Global {idx}"),
            "global",
            None,
            vec![agent("a")],
        );
        assert!(
            matches!(out, SaveCustomPipelineOutcome::Saved { .. }),
            "idx={idx}"
        );
    }
    assert_eq!(
        codes(f.create_in(&f.project, "One too many", "global", None, vec![agent("a")])),
        ["global_limit"]
    );
}

#[test]
fn builtin_templates_are_immutable_but_support_activate_trigger_override_archive_and_children() {
    let f = fixture();
    let activated = saved(
        f.service
            .activate_custom_pipeline(&f.project, "builtin-companion-notes")
            .unwrap(),
    );
    assert!(activated.is_builtin());
    assert!(activated.is_global());

    // Direct edits, deletes, and move-to-project are refused with builtin_immutable / error.
    let edit_err = f
        .service
        .save_custom_pipeline(SaveCustomPipelineRequest {
            project_id: f.project.clone(),
            pipeline_id: Some(activated.id.clone()),
            expected_version: Some(activated.current_version),
            name: "Renamed".into(),
            description: String::new(),
            definition: activated.definition.clone(),
            scope: None,
            parent_pipeline_id: None,
        })
        .unwrap();
    assert_eq!(codes(edit_err), ["builtin_immutable"]);

    let move_err = f
        .service
        .move_custom_pipeline_to_project(&f.project, &activated.id)
        .unwrap();
    assert_eq!(codes(move_err), ["builtin_immutable"]);

    assert!(
        f.service
            .delete_custom_pipeline(&f.project, &activated.id)
            .is_err()
    );

    // Per-project trigger override, archive/restore, and creating an editable child work.
    let overridden = saved(
        f.service
            .set_custom_pipeline_trigger(crate::custom_pipelines::SetPipelineTriggerRequest {
                project_id: f.project.clone(),
                pipeline_id: activated.id.clone(),
                trigger: Some(PipelineTrigger::DomainEvent {
                    event: crate::custom_pipelines::DomainEvent::FeatureApproved,
                    stage: None,
                }),
            })
            .unwrap(),
    );
    assert_eq!(
        overridden.trigger_override,
        Some(PipelineTrigger::DomainEvent {
            event: crate::custom_pipelines::DomainEvent::FeatureApproved,
            stage: None,
        })
    );
    let archived = saved(
        f.service
            .set_custom_pipeline_archived(&f.project, &activated.id, true)
            .unwrap(),
    );
    assert!(archived.archived);
    let restored = saved(
        f.service
            .set_custom_pipeline_archived(&f.project, &activated.id, false)
            .unwrap(),
    );
    assert!(!restored.archived);

    let mut child_def = activated.definition.clone();
    if let PipelineStep::Canonical(c) = &mut child_def.steps[0] {
        c.instructions_append = "Keep bullet summary under five items.".into();
    }
    let child = saved(f.create_in(
        &f.project,
        "My Companion Notes",
        "project",
        Some(&activated.id),
        child_def.steps,
    ));
    assert!(!child.is_builtin());
    assert_eq!(
        child.parent_pipeline_id.as_deref(),
        Some(activated.id.as_str())
    );
}

#[tokio::test]
async fn canonical_pipeline_step_builds_hardened_context_and_refuses_custom_cli_backend() {
    let f = fixture();
    let activated = saved(
        f.service
            .activate_custom_pipeline(&f.project, "builtin-companion-notes")
            .unwrap(),
    );
    let run_id = f
        .service
        .start_custom_pipeline_run(&f.project, &activated.id)
        .unwrap();
    let claim = f.service.claim_next_automation_step().unwrap().unwrap();
    assert_eq!(claim.run_id, run_id);

    // Inspect hardened no-tools context for NotesScribe.
    let PipelineStep::Canonical(ref canon) = activated.definition.steps[0] else {
        panic!("expected canonical step");
    };
    let (system, user) = f.service.canonical_step_context(&claim, canon).unwrap();
    assert!(system.contains("strict isolation"));
    assert!(system.contains("All tools, filesystem access, and commands are disabled"));
    assert!(user.contains("## Trigger"));

    // Configure custom CLI backend for notes_scribe and ensure execution fails cleanly.
    f.db()
        .execute(
            "INSERT INTO cli_agent_configs(project_id,agent_role,execution_mode,backend,custom_executable,created_at,updated_at)
             VALUES(?1,'notes_scribe','cli_process','custom','/bin/echo','2026-11-26T00:00:00Z','2026-11-26T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
    f.service
        .run_automation_step(claim, tokio_util::sync::CancellationToken::new())
        .await;
    let run = f.service.custom_pipeline_run(&f.project, &run_id).unwrap();
    assert_eq!(run.status, PipelineRunStatus::Failed);
    assert!(
        run.step(0)
            .unwrap()
            .error
            .as_deref()
            .is_some_and(|e| e.contains("refuses the custom CLI backend"))
    );
}

#[test]
fn domain_trigger_outbox_fires_on_all_four_events_and_dispatches_idempotently() {
    use crate::companion_notes::{CaptureNoteRequest, NoteScribeRuntime, NoteSource};
    use crate::custom_pipelines::DomainEvent;
    use crate::doc_reconciliation::DriftOrigin;
    use crate::doc_reconciliation::store::record_signal;
    use crate::feature_forge::{
        AcceptanceCriterion, CreateFeatureRequest, FeatureBrief, FeatureForgeService, TaskDraft,
        TaskKind,
    };

    let f = fixture();
    // Activate all four domain-event built-in pipelines in this project.
    for id in [
        "builtin-companion-notes",
        "builtin-docs-reconciliation",
        "builtin-checkpoint-visualizer",
        "builtin-feature-handoff",
    ] {
        saved(f.service.activate_custom_pipeline(&f.project, id).unwrap());
    }

    // 1. FeatureApproved via FeatureForgeService::approve
    let forge = FeatureForgeService::new(f.service.clone());
    let created = forge
        .create(CreateFeatureRequest {
            project_id: f.project.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            title: "Approve trigger check".into(),
            idea: "Add domain trigger support with explicit verification".into(),
        })
        .unwrap();
    let brief = FeatureBrief {
        objective: "Ship domain triggers cleanly".into(),
        audience: "Project developers".into(),
        behavior: "Emit domain event inside transaction".into(),
        scope: "Bounded workflow".into(),
        criteria: vec![AcceptanceCriterion {
            id: "ac-1".into(),
            description: "Fires once per approval".into(),
        }],
        tasks: vec![TaskDraft {
            title: "Wire emitter".into(),
            kind: TaskKind::Bug,
            description: "When approved, queue FeatureApproved event cleanly".into(),
            criteria: vec![AcceptanceCriterion {
                id: "ac-1".into(),
                description: "Fires once per approval".into(),
            }],
            dependencies: vec![],
            feature_id: None,
        }],
        coverage: "unavailable".into(),
        ..Default::default()
    };
    let revised = forge
        .revise(&f.project, &created.id, created.revision, brief)
        .unwrap();
    forge
        .approve(&f.project, &revised.id, revised.revision)
        .unwrap();

    // 2. DocsStale via doc_reconciliation::store::record_signal
    let db = f.db();
    record_signal(
        &db,
        &f.project,
        "Vault/architecture.md",
        "src/lib.rs",
        Some("old"),
        Some("new"),
        DriftOrigin::Watcher,
        None,
    )
    .unwrap();

    // 3. CheckpointReady (Integrator) via PersistenceService::open_checkpoint
    db.execute(
        "INSERT INTO pulsations(id,project_id,feature_name,status,stage,queue_sequence,created_at,updated_at)
         VALUES('pulse-dom',?1,'Feature X','running','Integrator',1,'2026-11-26T00:00:00Z','2026-11-26T00:00:00Z')",
        [&f.project],
    )
    .unwrap();
    PersistenceService::open_checkpoint(
        &db,
        "pulse-dom",
        ikore_contracts::stages::PulsationStage::Integrator,
    )
    .unwrap();
    drop(db);

    // 4. NoteCaptured via NoteScribeRuntime::capture
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let bus = std::sync::Arc::new(crate::AppEventBus::default());
    let scribe = std::sync::Arc::new(
        NoteScribeRuntime::new(f.service.clone(), bus, rt.handle().clone()).unwrap(),
    );
    scribe
        .capture(CaptureNoteRequest {
            project_id: f.project.clone(),
            request_id: "req-note-cap".into(),
            source_type: NoteSource::Text,
            raw_input: "Remember to wire domain triggers cleanly.".into(),
        })
        .unwrap();

    let outbox_count: i64 = f
        .db()
        .query_row(
            "SELECT count(*) FROM domain_trigger_outbox WHERE project_id=?1 AND state='pending'",
            [&f.project],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outbox_count, 4);

    // First dispatch creates 4 runs (1 per listener); second dispatch is idempotent (0).
    let started_first = f.service.dispatch_domain_triggers().unwrap();
    assert_eq!(started_first, 4);
    assert_eq!(f.service.dispatch_domain_triggers().unwrap(), 0);

    let runs = f
        .service
        .custom_pipelines_snapshot(&f.project)
        .unwrap()
        .runs;
    assert_eq!(runs.len(), 4);
    assert!(runs.iter().all(|r| r.trigger_kind == "domain_event"));
    assert!(
        runs.iter()
            .any(|r| r.trigger_label.as_deref() == Some("Feature approved"))
    );
    assert!(
        runs.iter()
            .any(|r| r.trigger_label.as_deref() == Some("Note captured"))
    );
    assert!(
        runs.iter()
            .any(|r| r.trigger_label.as_deref() == Some("Docs stale"))
    );
    assert!(
        runs.iter()
            .any(|r| r.trigger_label.as_deref() == Some("Checkpoint ready \u{b7} Integrator"))
    );

    // Non-Integrator checkpoint opens no outbox event when listener asks for Integrator only.
    let db = f.db();
    let emitted_code = PersistenceService::record_domain_event_in_tx(
        &db,
        &f.project,
        DomainEvent::CheckpointReady,
        Some("Code"),
        Some("cp-code"),
        serde_json::json!({"event":"checkpoint_ready","stage":"Code"}),
        "2026-11-26T00:00:00Z",
    )
    .unwrap();
    assert!(!emitted_code);
}

#[test]
fn deleting_a_project_purges_domain_trigger_outbox_and_keeps_builtins() {
    let f = fixture();
    let (_other_ws, other) = second_project(&f, "Temp Outbox");
    saved(
        f.service
            .activate_custom_pipeline(&other, "builtin-feature-handoff")
            .unwrap(),
    );
    let db = f.db();
    PersistenceService::record_domain_event_in_tx(
        &db,
        &other,
        crate::custom_pipelines::DomainEvent::FeatureApproved,
        None,
        Some("feat-temp"),
        serde_json::json!({"kind":"domain_event","event":"feature_approved"}),
        "2026-11-26T00:00:00Z",
    )
    .unwrap();
    drop(db);
    assert_eq!(f.service.dispatch_domain_triggers().unwrap(), 1);

    f.service
        .delete_project_permanently(&other, "Temp Outbox")
        .unwrap();

    let remaining_outbox: i64 = f
        .db()
        .query_row(
            "SELECT count(*) FROM domain_trigger_outbox WHERE project_id=?1",
            [&other],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_outbox, 0);

    let remaining_builtins: i64 = f
        .db()
        .query_row(
            "SELECT count(*) FROM custom_pipelines WHERE template_key IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_builtins, 5);
}
