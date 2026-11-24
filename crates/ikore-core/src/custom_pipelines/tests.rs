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
            "host_tool_unavailable",
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
    assert_eq!(offered.available_globals.len(), 1);
    assert_eq!(offered.available_globals[0].id, global.id);
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
    let globals = f
        .service
        .custom_pipelines_snapshot(&f.project)
        .unwrap()
        .available_globals;
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
