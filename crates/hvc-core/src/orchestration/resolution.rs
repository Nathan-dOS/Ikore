//! Resolution conversations for the clarification loop (ADR-022, P4/A4.1):
//! one real Companion conversation per `SpecificationQuestionSet`, distinct
//! from the host-rendered `specification_conversations` row (which only ever
//! holds the rendered questions and the developer's raw answer text, never
//! an LLM turn). Sending on this conversation reuses `ChatService`'s
//! existing turn machinery unchanged -- including its `chat_model_calls`
//! accounting with `pulsation_id: None` -- so this module owns no LLM
//! dispatch of its own, only the conversation's identity and lifecycle.
use super::*;
use coordination::invalid;
use rusqlite::OptionalExtension;

impl PersistenceService {
    /// Returns the conversation_id for `question_set_id`'s resolution
    /// conversation, creating one the first time it is opened. The
    /// conversation carries a real provider/model snapshot (never the
    /// `'specification'`/`'host-presented'` pair the rendered questions use),
    /// so it can run ordinary chat turns.
    pub fn ensure_resolution_conversation(
        &self,
        project_id: &str,
        question_set_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(id) = tx
            .query_row(
                "SELECT conversation_id FROM specification_resolution_conversations WHERE question_set_id=?1",
                [question_set_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        let (connection_id, provider_id, model_id) =
            Self::companion_default_connection(&tx, project_id)?;
        let conversation_id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
            params![
                conversation_id,
                project_id,
                connection_id,
                provider_id,
                model_id,
                "Specification clarification",
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO specification_resolution_conversations(question_set_id,conversation_id,created_at) VALUES(?1,?2,?3)",
            params![question_set_id, conversation_id, now],
        )?;
        tx.commit()?;
        Ok(conversation_id)
    }

    /// The Companion has no per-project routing the way agent roles do (see
    /// `provider_route_bindings`): a developer always picks a connection by
    /// hand when starting an ordinary chat. For a resolution conversation
    /// opened automatically (no such moment exists), this reuses the
    /// connection of the project's most recently touched API conversation --
    /// whatever the developer already has working -- falling back to the
    /// first enabled, active provider connection if the Companion has never
    /// been opened for this project yet. The `legacy-openai`/`legacy-anthropic`
    /// placeholders `ensure_legacy_configuration` seeds into every database are
    /// excluded from that fallback: they exist unconditionally regardless of
    /// whether real credentials are configured, so treating them as "active"
    /// would make this fallback pick them ahead of any connection the
    /// developer actually set up.
    fn companion_default_connection(
        db: &Connection,
        project_id: &str,
    ) -> Result<(String, String, String), PersistenceServiceError> {
        if let Some(row) = db
            .query_row(
                "SELECT connection_id,provider_id_snapshot,model_id_snapshot FROM chat_conversations WHERE project_id=?1 AND connection_id IS NOT NULL AND transport_kind='api' ORDER BY updated_at DESC LIMIT 1",
                [project_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            )
            .optional()?
        {
            return Ok(row);
        }
        db.query_row(
            "SELECT id,provider_id,default_model FROM provider_connections WHERE enabled=1 AND state='active' AND id NOT IN ('legacy-openai','legacy-anthropic') ORDER BY created_at LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| {
            invalid("No active provider connection is configured for the Companion.")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_forge::{
        id, AcceptanceCriterion, CreatePulsationFromTasksRequest, CreateTasksRequest, TaskDraft,
        TaskKind, TaskSelection, TaskService,
    };
    use crate::orchestration::composition::settings::CompositionSettings;

    struct Fixture {
        _data: tempfile::TempDir,
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
                service,
                project,
            }
        }
    }

    fn provider_connection(service: &PersistenceService, id: &str) {
        let db = service.connection().unwrap();
        db.execute(
            "INSERT INTO provider_connections(id,provider_id,display_name,credential_ref,default_model,enabled,state,created_at,updated_at) VALUES(?1,'openai',?1,?1,'gpt-fixture',1,'active','now','now')",
            [id],
        )
        .unwrap();
    }

    /// A minimal `specification_question_sets` row and the pulsation/checkpoint
    /// chain `specification_resolution_conversations`'s foreign key requires --
    /// bare-bones (not a real dispatch), just enough for `ensure_resolution_conversation`'s
    /// own tests, which only care about conversation identity/reuse, not the
    /// surrounding pulsation's realism.
    fn bare_question_set(service: &PersistenceService, project: &str, question_set_id: &str) {
        let db = service.connection().unwrap();
        let queue_sequence: i64 = db
            .query_row(
                "SELECT COALESCE(MAX(queue_sequence),0)+1 FROM pulsations WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .unwrap();
        let pulse = format!("pulse-for-{question_set_id}");
        db.execute(
            "INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version) VALUES(?1,?2,'Fixture','SPECIFICATION','checkpoint','now','now',?3,4)",
            params![pulse, project, queue_sequence],
        )
        .unwrap();
        let job = format!("job-for-{question_set_id}");
        db.execute(
            "INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at) VALUES(?1,?2,'run_stage','completed','now','now')",
            params![job, pulse],
        )
        .unwrap();
        let checkpoint = format!("checkpoint-for-{question_set_id}");
        db.execute(
            "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,purpose,input_revision,state_version,producer_job_id) VALUES(?1,?2,'SPECIFICATION','waiting','now','now','clarification','clarification',1,1,?3)",
            params![checkpoint, pulse, job],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES(?1,?2,'specification','host-presented','Specification','now','now')",
            params![format!("host-conv-for-{question_set_id}"), project],
        )
        .unwrap();
        db.execute(
            "INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES(?1,?2,?3,1,1,1,?4,'[]')",
            params![
                question_set_id,
                checkpoint,
                pulse,
                format!("host-conv-for-{question_set_id}")
            ],
        )
        .unwrap();
    }

    #[test]
    fn ensure_resolution_conversation_is_idempotent_and_falls_back_to_an_active_connection() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-a");
        bare_question_set(&f.service, &f.project, "set-a");
        let first = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        let second = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        assert_eq!(
            first, second,
            "must not create a second conversation for the same question set"
        );
        let db = f.service.connection().unwrap();
        let (project, connection, provider, model, transport): (
            String,
            Option<String>,
            String,
            String,
            String,
        ) = db
            .query_row(
                "SELECT project_id,connection_id,provider_id_snapshot,model_id_snapshot,transport_kind FROM chat_conversations WHERE id=?1",
                [&first],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(project, f.project);
        assert_eq!(connection.as_deref(), Some("connection-a"));
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-fixture");
        assert_eq!(transport, "api");
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM specification_resolution_conversations WHERE question_set_id='set-a'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn a_different_question_set_gets_a_distinct_conversation() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-a");
        bare_question_set(&f.service, &f.project, "set-a");
        bare_question_set(&f.service, &f.project, "set-b");
        let a = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        let b = f
            .service
            .ensure_resolution_conversation(&f.project, "set-b")
            .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn prefers_the_projects_most_recently_touched_api_conversation_over_any_active_connection() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-old");
        provider_connection(&f.service, "connection-new");
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-old',?1,'connection-old','openai','gpt-old','Old chat','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-new',?1,'connection-new','openai','gpt-new','Newer chat','2020-01-02T00:00:00Z','2020-01-02T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
        drop(db);
        bare_question_set(&f.service, &f.project, "set-a");
        let conversation_id = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        let model: String = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT model_id_snapshot FROM chat_conversations WHERE id=?1",
                [&conversation_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            model, "gpt-new",
            "must reuse the most recently updated conversation, not the oldest"
        );
    }

    #[test]
    fn with_no_prior_conversation_and_no_active_connection_fails_clearly() {
        let f = Fixture::new();
        let error = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap_err();
        assert!(error.to_string().contains("provider connection"));
    }

    #[test]
    fn list_open_specification_checkpoints_spans_every_pulsation_in_the_project() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-a");
        f.service
            .update_composition_settings(
                &f.project,
                CompositionSettings {
                    enabled: true,
                    ..CompositionSettings::default()
                },
            )
            .unwrap();
        let task_service = TaskService::new(f.service.clone());
        let mut pulses = Vec::new();
        for name in ["First batch", "Second batch"] {
            let created = task_service
                .create(CreateTasksRequest {
                    project_id: f.project.clone(),
                    request_id: id(),
                    tasks: vec![TaskDraft {
                        title: name.into(),
                        kind: TaskKind::Bug,
                        description: "Fix src/lib.rs".into(),
                        criteria: vec![AcceptanceCriterion {
                            id: "ac-1".into(),
                            description: "It is fixed".into(),
                        }],
                        dependencies: vec![],
                        feature_id: None,
                    }],
                })
                .unwrap();
            let pulse = task_service
                .dispatch(CreatePulsationFromTasksRequest {
                    project_id: f.project.clone(),
                    request_id: id(),
                    title: name.into(),
                    tasks: created
                        .iter()
                        .map(|t| TaskSelection {
                            task_id: t.id.clone(),
                            expected_revision: t.revision,
                            manual_path_hint: None,
                        })
                        .collect(),
                })
                .unwrap();
            pulses.push(pulse.id);
        }
        // Two pulsations, each parked on its own clarification checkpoint.
        let db = f.service.connection().unwrap();
        for (index, pulse) in pulses.iter().enumerate() {
            let (input_revision, state_version): (i64, i64) = db
                .query_row(
                    "SELECT input_revision,state_version FROM pulsations WHERE id=?1",
                    [pulse],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            db.execute(
                "UPDATE pulsations SET status='checkpoint',stage='SPECIFICATION' WHERE id=?1",
                [pulse],
            )
            .unwrap();
            let job = format!("job-{index}");
            db.execute(
                "INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at) VALUES(?1,?2,'run_stage','completed','now','now')",
                params![job, pulse],
            )
            .unwrap();
            let checkpoint = format!("checkpoint-{index}");
            db.execute(
                "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,purpose,input_revision,state_version,producer_job_id) VALUES(?1,?2,'SPECIFICATION','waiting','now','now','clarification','clarification',?3,?4,?5)",
                params![checkpoint, pulse, input_revision, state_version, job],
            )
            .unwrap();
            db.execute(
                "INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES(?1,?2,'specification','host-presented','Specification','now','now')",
                params![format!("host-conv-{index}"), f.project],
            )
            .unwrap();
            db.execute(
                "INSERT INTO specification_conversations(pulsation_id,conversation_id) VALUES(?1,?2)",
                params![pulse, format!("host-conv-{index}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES(?1,?2,?3,?4,1,?5,?6,'[]')",
                params![
                    format!("set-{index}"),
                    checkpoint,
                    pulse,
                    input_revision,
                    state_version,
                    format!("host-conv-{index}")
                ],
            )
            .unwrap();
        }
        let open = f
            .service
            .list_open_specification_checkpoints(&f.project)
            .unwrap();
        assert_eq!(open.len(), 2, "{open:?}");
        let mut pulsation_ids: Vec<_> = open
            .iter()
            .map(|set| set.fence.pulsation_id.clone().unwrap())
            .collect();
        pulsation_ids.sort();
        let mut expected = pulses.clone();
        expected.sort();
        assert_eq!(pulsation_ids, expected);
    }
}
