//! ADR-037 M2: each writer of the event inventory appends exactly one
//! exchange log item per change, in order, with a stable key. A test module
//! of `persistence_service`, for its private transition helper.

use std::path::Path;

use rusqlite::TransactionBehavior;

use super::*;
use crate::companion_drawings::service::CompanionDrawingsService;
use crate::llm::AgentRole;
use crate::terminals::{
    AgentExecutionMode, CliBackend, CreateTerminalRunRequest, ExecutionOrigin, TerminalRepository,
    TerminalRunStatus,
};

fn project(service: &PersistenceService, workspace: &Path) -> String {
    service
        .create_project(CreateProjectRequest {
            workspace_path: workspace.to_string_lossy().into_owned(),
            name: Some("Exchange writers".into()),
        })
        .expect("project")
        .project
        .id
}

/// `(kind, seq, key)` of every item of one stream, in order.
fn stream(service: &PersistenceService, kind: &str, subject: &str) -> Vec<(String, u64, String)> {
    service
        .connection()
        .unwrap()
        .prepare(
            "SELECT i.kind,i.seq,i.idempotency_key FROM exchange_items i
             JOIN exchange_streams s ON s.id=i.stream_id
             WHERE s.kind=?1 AND s.subject_id=?2 ORDER BY i.seq",
        )
        .unwrap()
        .query_map(params![kind, subject], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn transition(
    connection: &mut Connection,
    current: &Pulsation,
    status: PulsationStatus,
    event_type: &str,
) -> Result<Pulsation, PersistenceServiceError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let next = PersistenceService::transition_pulsation(
        &transaction,
        current,
        status,
        current.stage,
        event_type,
        None,
    )?;
    transaction.commit().unwrap();
    Ok(next)
}

#[test]
fn every_pulsation_transition_records_one_item_named_after_it() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = project(&service, workspace.path());
    let created = service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.clone(),
            feature_name: "Exchange writers".into(),
        })
        .unwrap()
        .pulsation;
    let mut connection = service.connection().unwrap();
    let mut current = created.clone();
    for (status, event) in [
        (PulsationStatus::Running, "stage_started"),
        (PulsationStatus::Checkpoint, "stage_failure_opened"),
        (PulsationStatus::Running, "stage_failure_retry"),
        (PulsationStatus::Checkpoint, "checkpoint_opened"),
        (PulsationStatus::Running, "hitl_approved"),
    ] {
        if current.status == status {
            continue;
        }
        current = transition(&mut connection, &current, status, event).unwrap();
    }
    current = transition(&mut connection, &current, PulsationStatus::Done, "done").unwrap();
    // A refused transition leaves no item behind.
    assert!(transition(&mut connection, &current, PulsationStatus::Running, "late").is_err());

    let items = stream(&service, "pulsation", &created.id);
    let kinds: Vec<&str> = items.iter().map(|(kind, _, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "pulsation_transition", // created: idle -> planning
            "pulsation_transition", // planning -> running
            "checkpoint_opened",
            "stage_failure_resolved",
            "checkpoint_opened",
            "checkpoint_resolved",
            "pulsation_transition", // running -> done
        ]
    );
    let events: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pulse_events WHERE pulsation_id=?1 AND to_status IS NOT NULL AND event_type<>'pulse_created'",
            [&created.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(items.len() as u64, events, "one item per transition");
    assert!(
        items
            .iter()
            .enumerate()
            .all(|(index, (_, seq, key))| *seq == index as u64 + 1
                && key.starts_with(&format!("pulsation:{}:state:", created.id)))
    );
}

#[test]
fn every_cli_run_status_change_records_one_item() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = project(&service, workspace.path());
    let repository = TerminalRepository::new(service.database_path());
    repository.ensure_project_configs(&project, true).unwrap();
    let create = || {
        repository
            .create_run(CreateTerminalRunRequest {
                project_id: project.clone(),
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: None,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap()
    };
    let finished = create();
    for status in [
        TerminalRunStatus::Starting,
        TerminalRunStatus::Running,
        TerminalRunStatus::Running, // unchanged: no item
        TerminalRunStatus::Completed,
    ] {
        repository
            .set_run_status(&finished.id, status, Some(0), None, None)
            .unwrap();
    }
    let items = stream(&service, "terminal_run", &finished.id);
    assert_eq!(
        items
            .iter()
            .map(|(_, _, key)| key.clone())
            .collect::<Vec<_>>(),
        ["starting", "running", "completed"]
            .iter()
            .map(|status| format!("terminal_run:{}:{status}", finished.id))
            .collect::<Vec<_>>()
    );

    let interrupted = create();
    repository
        .set_run_status(
            &interrupted.id,
            TerminalRunStatus::Starting,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(repository.recover_interrupted_runs().unwrap(), 1);
    let items = stream(&service, "terminal_run", &interrupted.id);
    assert_eq!(
        items.last().map(|(_, _, key)| key.clone()),
        Some(format!("terminal_run:{}:interrupted", interrupted.id))
    );
}

#[test]
fn drawing_changes_record_one_item_in_their_transaction() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(data.path()).unwrap();
    let project = project(&service, workspace.path());
    let drawings = CompanionDrawingsService::new(service.clone());
    let drawing = drawings.create(&project, "Exchange").unwrap();
    let saved = drawings
        .save(
            &project,
            &drawing.id,
            drawing.revision,
            "Exchange, renamed",
            &drawing.scene,
            &drawing.viewport,
        )
        .unwrap();
    // A stale save is refused and records nothing.
    assert!(
        drawings
            .save(
                &project,
                &drawing.id,
                1,
                "Stale",
                &drawing.scene,
                &drawing.viewport
            )
            .is_err()
    );
    drawings
        .delete(&project, &drawing.id, saved.revision)
        .unwrap();
    let keys: Vec<String> = stream(&service, "drawings", &drawing.id)
        .into_iter()
        .map(|(_, _, key)| key)
        .collect();
    assert_eq!(
        keys,
        vec![
            format!("drawing:{}:rev:1", drawing.id),
            format!("drawing:{}:rev:2", drawing.id),
            format!("drawing:{}:deleted:2", drawing.id),
        ]
    );
}

#[test]
fn a_projects_log_goes_with_the_project() {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let service = PersistenceService::initialize(&fixture.path().join("data")).unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: root.to_string_lossy().into_owned(),
            preferred_name: Some("Exchange".into()),
        })
        .unwrap()
        .project;
    service
        .create_pulsation(CreatePulsationRequest {
            project_id: project.id.clone(),
            feature_name: "Deleted with its project".into(),
        })
        .unwrap();
    let count = |table: &str| -> u32 {
        service
            .connection()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    assert!(count("exchange_items") > 0);
    service
        .delete_project_permanently(&project.id, &project.name)
        .unwrap();
    assert_eq!((count("exchange_streams"), count("exchange_items")), (0, 0));
}
