//! Resolution conversations for the clarification loop (ADR-022, P4/A4.1):
//! one real Companion conversation per `SpecificationQuestionSet`, distinct
//! from the host-rendered `specification_conversations` row (which only ever
//! holds the rendered questions and the developer's raw answer text, never
//! an LLM turn). Sending on this conversation reuses `ChatService`'s
//! existing turn machinery unchanged -- including its `chat_model_calls`
//! accounting with `pulsation_id: None` -- so this module owns no LLM
//! dispatch of its own, only the conversation's identity and lifecycle.
use super::{Connection, PersistenceService, PersistenceServiceError, Uuid, coordination, params};
use coordination::invalid;
use rusqlite::OptionalExtension;

/// A snapshot of whichever provider/model identity a Companion turn should
/// currently use, for either transport. Mirrors the columns `chat_conversations`
/// stores for each: `connection_id`/`provider_id`/`model_id` for API,
/// `cli_backend`/`model_profile`/`model_source` for CLI.
struct CompanionTarget {
    connection_id: Option<String>,
    provider_id: String,
    model_id: String,
    transport_kind: String,
    cli_backend: Option<String>,
    model_profile: Option<String>,
    model_source: String,
}

impl PersistenceService {
    /// Creates a real Companion chat only after a developer asks for help.
    /// Opening or listing a decision never calls this method.
    pub fn ensure_decision_conversation(
        &self,
        project_id: &str,
        decision_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let decision = crate::orchestration::decisions::read_pending_decision_on(
            &tx,
            project_id,
            decision_id,
        )?
        .ok_or_else(|| {
            invalid("This decision is no longer pending. Refresh before asking about it.")
        })?;
        if let Some(id) = tx.query_row(
            "SELECT d.conversation_id FROM decision_conversations d JOIN chat_conversations c ON c.id=d.conversation_id WHERE d.project_id=?1 AND d.decision_id=?2 AND c.project_id=?1",
            params![project_id,decision_id],|row|row.get::<_,String>(0),
        ).optional()? {
            return Ok(id);
        }
        // A clarification may have been opened through the older full-screen
        // flow since V65. Preserve that exact conversation and all its history.
        let legacy = if let Some(set) = &decision.question_set {
            tx.query_row("SELECT r.conversation_id FROM specification_resolution_conversations r JOIN chat_conversations c ON c.id=r.conversation_id WHERE r.question_set_id=?1 AND c.project_id=?2", params![set.id,project_id], |row|row.get::<_,String>(0)).optional()?
        } else {
            None
        };
        let conversation_id = if let Some(id) = legacy {
            id
        } else {
            let target = Self::companion_default_target(&tx, project_id)?;
            let id = Uuid::new_v4().to_string();
            let now = Self::now_iso();
            tx.execute("INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,transport_kind,cli_backend,model_profile,model_source,title,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)", params![id,project_id,target.connection_id,target.provider_id,target.model_id,target.transport_kind,target.cli_backend,target.model_profile,target.model_source,format!("Decision: {}",decision.title),now])?;
            if let Some(set) = &decision.question_set {
                tx.execute("INSERT INTO specification_resolution_conversations(question_set_id,conversation_id,created_at) VALUES(?1,?2,?3)",params![set.id,id,now])?;
            }
            id
        };
        let source_kind = serde_json::to_string(&decision.kind)
            .map_err(|error| invalid(error.to_string()))?
            .trim_matches('"')
            .to_owned();
        tx.execute("INSERT INTO decision_conversations(project_id,decision_id,source_kind,source_id,conversation_id,created_at) VALUES(?1,?2,?3,?4,?5,?6)", params![project_id,decision_id,source_kind,decision.source_id,conversation_id,Self::now_iso()])?;
        tx.commit()?;
        Ok(conversation_id)
    }

    /// Returns the `conversation_id` for `question_set_id`'s resolution
    /// conversation, creating one the first time it is opened. The
    /// conversation carries a real provider/model snapshot (never the
    /// `'specification'`/`'host-presented'` pair the rendered questions use),
    /// so it can run ordinary chat turns.
    ///
    /// Every call -- not just the first -- resyncs that snapshot to whatever
    /// the developer is currently using for the Companion. There is no "start
    /// a new one" escape hatch for a resolution conversation the way there is
    /// for an ordinary chat, so if it stayed pinned to whatever was current
    /// the moment it was first opened, a conversation created before a
    /// working connection existed would stay stuck on that stale/broken
    /// snapshot forever.
    pub fn ensure_resolution_conversation(
        &self,
        project_id: &str,
        question_set_id: &str,
    ) -> Result<String, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let checkpoint: String = tx.query_row(
            "SELECT q.checkpoint_id FROM specification_question_sets q JOIN pulsations p ON p.id=q.pulsation_id WHERE q.id=?1 AND p.project_id=?2",
            params![question_set_id,project_id],|row|row.get(0),
        ).optional()?.ok_or_else(||invalid("The question set does not belong to this project."))?;
        let target = Self::companion_default_target(&tx, project_id)?;
        let now = Self::now_iso();
        if let Some(conversation_id) = tx
            .query_row(
                "SELECT r.conversation_id FROM specification_resolution_conversations r JOIN chat_conversations c ON c.id=r.conversation_id WHERE r.question_set_id=?1 AND c.project_id=?2",
                params![question_set_id,project_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            tx.execute(
                "UPDATE chat_conversations SET connection_id=?2,provider_id_snapshot=?3,model_id_snapshot=?4,transport_kind=?5,cli_backend=?6,model_profile=?7,model_source=?8,updated_at=?9 WHERE id=?1",
                params![
                    conversation_id,
                    target.connection_id,
                    target.provider_id,
                    target.model_id,
                    target.transport_kind,
                    target.cli_backend,
                    target.model_profile,
                    target.model_source,
                    now
                ],
            )?;
            tx.execute("INSERT OR IGNORE INTO decision_conversations(project_id,decision_id,source_kind,source_id,conversation_id,created_at) VALUES(?1,?2,'clarification',?3,?4,?5)",params![project_id,format!("checkpoint:{checkpoint}"),question_set_id,conversation_id,now])?;
            tx.commit()?;
            return Ok(conversation_id);
        }
        let conversation_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,transport_kind,cli_backend,model_profile,model_source,title,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
            params![
                conversation_id,
                project_id,
                target.connection_id,
                target.provider_id,
                target.model_id,
                target.transport_kind,
                target.cli_backend,
                target.model_profile,
                target.model_source,
                "Specification clarification",
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO specification_resolution_conversations(question_set_id,conversation_id,created_at) VALUES(?1,?2,?3)",
            params![question_set_id, conversation_id, now],
        )?;
        tx.execute("INSERT INTO decision_conversations(project_id,decision_id,source_kind,source_id,conversation_id,created_at) VALUES(?1,?2,'clarification',?3,?4,?5)",params![project_id,format!("checkpoint:{checkpoint}"),question_set_id,conversation_id,now])?;
        tx.commit()?;
        Ok(conversation_id)
    }

    /// The Companion has no per-project routing the way agent roles do (see
    /// `provider_route_bindings`): a developer always picks a connection or
    /// CLI backend by hand when starting an ordinary chat. For a resolution
    /// conversation, this reuses the identity of the project's most recently
    /// touched Companion conversation of either transport -- whatever the
    /// developer already has working right now -- falling back to the first
    /// enabled, active API provider connection if the Companion has never
    /// been opened for this project yet. The `legacy-openai`/`legacy-anthropic`
    /// placeholders `ensure_legacy_configuration` seeds into every database are
    /// excluded from that fallback: they exist unconditionally regardless of
    /// whether real credentials are configured, so treating them as "active"
    /// would make this fallback pick them ahead of any connection the
    /// developer actually set up. A candidate whose API connection was since
    /// removed or disabled is skipped rather than copied forward broken, and
    /// another resolution conversation is never itself a candidate -- this
    /// always traces back to a conversation the developer actually picked.
    fn companion_default_target(
        db: &Connection,
        project_id: &str,
    ) -> Result<CompanionTarget, PersistenceServiceError> {
        if let Some(target) = Self::saved_companion_target(db, project_id)? {
            return Ok(target);
        }
        if let Some(target) = db
            .query_row(
                "SELECT c.connection_id,c.provider_id_snapshot,c.model_id_snapshot,c.transport_kind,c.cli_backend,c.model_profile,c.model_source
                 FROM chat_conversations c
                 WHERE c.project_id=?1
                   AND NOT EXISTS(SELECT 1 FROM specification_resolution_conversations r WHERE r.conversation_id=c.id)
                   AND NOT EXISTS(SELECT 1 FROM decision_conversations d WHERE d.conversation_id=c.id)
                   AND (
                     c.transport_kind='cli'
                     OR (c.connection_id IS NOT NULL AND EXISTS(SELECT 1 FROM provider_connections p WHERE p.id=c.connection_id AND p.enabled=1))
                   )
                 ORDER BY c.updated_at DESC LIMIT 1",
                [project_id],
                |r| {
                    Ok(CompanionTarget {
                        connection_id: r.get(0)?,
                        provider_id: r.get(1)?,
                        model_id: r.get(2)?,
                        transport_kind: r.get(3)?,
                        cli_backend: r.get(4)?,
                        model_profile: r.get(5)?,
                        model_source: r.get(6)?,
                    })
                },
            )
            .optional()?
        {
            return Ok(target);
        }
        let (connection_id, provider_id, model_id) = db
            .query_row(
                "SELECT id,provider_id,default_model FROM provider_connections WHERE enabled=1 AND state='active' AND id NOT IN ('legacy-openai','legacy-anthropic') ORDER BY created_at LIMIT 1",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            )
            .optional()?
            .ok_or_else(|| {
                invalid("No active provider connection is configured for the Companion.")
            })?;
        Ok(CompanionTarget {
            connection_id: Some(connection_id),
            provider_id,
            model_id,
            transport_kind: "api".into(),
            cli_backend: None,
            model_profile: None,
            model_source: "catalog".into(),
        })
    }

    /// The target the developer last chose in the Companion panel for this
    /// project (`ChatService::save_default_target`). Before it existed, the
    /// Teste 4 resolution conversation inherited `claude/haiku` from an older
    /// chat while the panel showed Codex. An API connection since disabled
    /// falls through to the older rules instead of being copied forward.
    fn saved_companion_target(
        db: &Connection,
        project_id: &str,
    ) -> Result<Option<CompanionTarget>, PersistenceServiceError> {
        use crate::chat_service::{ChatExecutionTargetRequest, companion_target_setting_key};
        let saved: Option<String> = db
            .query_row(
                "SELECT value FROM runtime_settings WHERE key=?1",
                [companion_target_setting_key(project_id)],
                |r| r.get(0),
            )
            .optional()?;
        let Some(target) =
            saved.and_then(|value| serde_json::from_str::<ChatExecutionTargetRequest>(&value).ok())
        else {
            return Ok(None);
        };
        Ok(match target {
            ChatExecutionTargetRequest::Api { connection_id } => db
                .query_row(
                    "SELECT provider_id,default_model FROM provider_connections WHERE id=?1 AND enabled=1",
                    [&connection_id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?
                .map(|(provider_id, model_id)| CompanionTarget {
                    connection_id: Some(connection_id),
                    provider_id,
                    model_id,
                    transport_kind: "api".into(),
                    cli_backend: None,
                    model_profile: None,
                    model_source: "catalog".into(),
                }),
            ChatExecutionTargetRequest::Cli {
                backend,
                model,
                model_profile,
            } => {
                let model_source = match model.as_deref() {
                    None => "default",
                    Some(model_id) => {
                        let in_catalog: bool = db.query_row(
                            "SELECT EXISTS(SELECT 1 FROM cli_model_catalog_entries e JOIN cli_model_catalog_snapshots s ON s.id=e.snapshot_id WHERE s.project_id=?1 AND s.backend=?2 AND e.model_id=?3)",
                            params![project_id, backend.as_str(), model_id],
                            |r| r.get(0),
                        )?;
                        if in_catalog { "catalog" } else { "manual" }
                    }
                };
                Some(CompanionTarget {
                    connection_id: None,
                    provider_id: backend.as_str().into(),
                    model_id: model.unwrap_or_else(|| "default".into()),
                    transport_kind: "cli".into(),
                    cli_backend: Some(backend.as_str().into()),
                    model_profile,
                    model_source: model_source.into(),
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature_forge::{
        AcceptanceCriterion, CreatePulsationFromTasksRequest, CreateTasksRequest, TaskDraft,
        TaskKind, TaskSelection, TaskService, id,
    };
    use crate::orchestration::composition::settings::CompositionSettings;
    use crate::persistence_service::EnsureProjectForWorkspaceRequest;

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
                    workspace_path: workspace.path().to_string_lossy().into(),
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
        assert!(
            f.service
                .ensure_resolution_conversation("other-project", "set-a")
                .is_err()
        );
        assert_eq!(db.query_row("SELECT conversation_id FROM decision_conversations WHERE decision_id='checkpoint:checkpoint-for-set-a'",[],|row|row.get::<_,String>(0)).unwrap(),first);
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
    fn prefers_a_cli_conversation_over_an_active_api_connection_when_more_recent() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-a");
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,transport_kind,cli_backend,model_profile,model_source,title,created_at,updated_at) VALUES('conv-cli',?1,'codex','gpt-5.6-terra','cli','codex',NULL,'catalog','CLI chat','2020-01-02T00:00:00Z','2020-01-02T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
        drop(db);
        bare_question_set(&f.service, &f.project, "set-a");
        let conversation_id = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        let (transport, cli_backend, connection): (String, Option<String>, Option<String>) = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT transport_kind,cli_backend,connection_id FROM chat_conversations WHERE id=?1",
                [&conversation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(transport, "cli");
        assert_eq!(cli_backend.as_deref(), Some("codex"));
        assert!(connection.is_none());
    }

    #[test]
    fn reopening_resyncs_a_stale_snapshot_to_the_now_current_conversation() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-old");
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-old',?1,'connection-old','openai','gpt-old','Old chat','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
        drop(db);
        bare_question_set(&f.service, &f.project, "set-a");
        let first = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        let model_after_first: String = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT model_id_snapshot FROM chat_conversations WHERE id=?1",
                [&first],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(model_after_first, "gpt-old");

        // The developer switches to CLI/Codex after this resolution
        // conversation already existed -- reopening it must pick that up,
        // not stay pinned to whatever was current the first time.
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,transport_kind,cli_backend,model_profile,model_source,title,created_at,updated_at) VALUES('conv-cli',?1,'codex','gpt-5.6-terra','cli','codex',NULL,'catalog','CLI chat','2020-01-02T00:00:00Z','2020-01-02T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
        drop(db);
        let second = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap();
        assert_eq!(first, second, "same conversation identity/history");
        let (transport, cli_backend): (String, Option<String>) = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT transport_kind,cli_backend FROM chat_conversations WHERE id=?1",
                [&second],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(transport, "cli");
        assert_eq!(cli_backend.as_deref(), Some("codex"));
    }

    #[test]
    fn a_disabled_connections_conversation_is_skipped_for_an_older_still_valid_one() {
        let f = Fixture::new();
        provider_connection(&f.service, "connection-old");
        provider_connection(&f.service, "connection-disabled");
        let db = f.service.connection().unwrap();
        db.execute(
            "UPDATE provider_connections SET enabled=0 WHERE id='connection-disabled'",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-old',?1,'connection-old','openai','gpt-old','Old chat','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z')",
            [&f.project],
        )
        .unwrap();
        // A more-recent conversation whose connection has since been disabled
        // (not deleted -- `ON DELETE SET NULL` would already exclude a
        // deleted one via the plain `connection_id IS NOT NULL` check).
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES('conv-disabled',?1,'connection-disabled','openai','gpt-disabled','Disabled chat','2020-01-02T00:00:00Z','2020-01-02T00:00:00Z')",
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
            model, "gpt-old",
            "must skip the more-recent conversation whose connection is now disabled"
        );
    }

    #[test]
    fn with_no_prior_conversation_and_no_active_connection_fails_clearly() {
        let f = Fixture::new();
        bare_question_set(&f.service, &f.project, "set-a");
        let error = f
            .service
            .ensure_resolution_conversation(&f.project, "set-a")
            .unwrap_err();
        assert!(error.to_string().contains("provider connection"));
        assert_eq!(
            f.service
                .connection()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM specification_resolution_conversations",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            f.service
                .connection()
                .unwrap()
                .query_row("SELECT count(*) FROM decision_conversations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn an_unknown_question_set_is_rejected_before_provider_resolution() {
        let f = Fixture::new();
        let error = f
            .service
            .ensure_resolution_conversation(&f.project, "missing-set")
            .unwrap_err();
        assert!(error.to_string().contains("question set"));
        assert_eq!(
            f.service
                .connection()
                .unwrap()
                .query_row("SELECT count(*) FROM chat_conversations", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
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

    /// Teste 4 (2026-09-22): the resolution conversation inherited
    /// `claude/haiku` from an older chat while the panel showed Codex. The
    /// target the developer last chose in the Companion panel now wins.
    #[test]
    fn the_saved_companion_target_wins_over_the_latest_conversation() {
        let f = Fixture::new();
        bare_question_set(&f.service, &f.project, "set-saved");
        let db = f.service.connection().unwrap();
        db.execute(
            "INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,transport_kind,cli_backend,model_source,title,created_at,updated_at) VALUES('older',?1,'claude','haiku','cli','claude','catalog','Older','2026-09-22T18:00:00Z','2026-09-22T18:00:00Z')",
            [&f.project],
        )
        .unwrap();
        db.execute(
            "INSERT INTO runtime_settings(key,value,updated_at) VALUES(?1,?2,'now')",
            params![
                crate::chat_service::companion_target_setting_key(&f.project),
                r#"{"transport":"cli","backend":"codex","model":null,"model_profile":null}"#
            ],
        )
        .unwrap();
        let conversation = f
            .service
            .ensure_resolution_conversation(&f.project, "set-saved")
            .unwrap();
        let (transport, backend, model): (String, Option<String>, String) = db
            .query_row(
                "SELECT transport_kind,cli_backend,model_id_snapshot FROM chat_conversations WHERE id=?1",
                [&conversation],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (transport.as_str(), backend.as_deref(), model.as_str()),
            ("cli", Some("codex"), "default")
        );
    }
}
