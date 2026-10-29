//! Specification owns questions. Companion stores/presents them without a model call.
use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, Pulsation,
    PulsationStage, PulsationStatus, Uuid, coordination, params,
};
use crate::orchestration::{
    ClarificationAnswerV1, ClarificationExchange, ExecutionFence, SpecificationQuestion,
    SpecificationQuestionSet, SubmitSpecificationAnswersRequest, validate_specification_answers,
};
use coordination::invalid;
use rusqlite::TransactionBehavior;

impl PersistenceService {
    pub(super) fn pending_questions(
        db: &Connection,
        pulse: &str,
    ) -> Result<Option<SpecificationQuestionSet>, PersistenceServiceError> {
        let row=db.query_row("SELECT q.id,q.checkpoint_id,q.input_revision,q.generation,q.state_version,q.conversation_id,q.questions_json,p.project_id,c.producer_job_id FROM specification_question_sets q JOIN checkpoints c ON c.id=q.checkpoint_id JOIN pulsations p ON p.id=q.pulsation_id WHERE q.pulsation_id=?1 AND c.purpose='clarification' AND c.status='waiting' AND p.status='checkpoint' AND p.input_revision=q.input_revision AND p.state_version=q.state_version AND q.answers_json IS NULL",[pulse],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?,r.get::<_,u64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?))).optional()?;
        row.map(
            |(id, checkpoint, revision, generation, state, conversation, json, project, job)| {
                Ok(SpecificationQuestionSet {
                    id,
                    checkpoint_id: checkpoint,
                    conversation_id: conversation,
                    fence: ExecutionFence {
                        project_id: project,
                        pulsation_id: Some(pulse.into()),
                        input_revision: Some(revision),
                        job_id: job,
                        generation,
                        state_version: state,
                    },
                    questions: serde_json::from_str(&json).map_err(|e| invalid(e.to_string()))?,
                })
            },
        )
        .transpose()
    }

    /// Every open clarification checkpoint across the whole project (ADR-022
    /// clarification loop, P4/A4.1): unlike `pending_questions`, not scoped to
    /// one pulsation, since the resolution modal needs to show one chip per
    /// pulsation currently waiting on Specification, however many there are.
    pub fn list_open_specification_checkpoints(
        &self,
        project_id: &str,
    ) -> Result<Vec<SpecificationQuestionSet>, PersistenceServiceError> {
        let db = self.connection()?;
        let rows=db.prepare("SELECT q.id,q.checkpoint_id,q.input_revision,q.generation,q.state_version,q.conversation_id,q.questions_json,p.id,c.producer_job_id FROM specification_question_sets q JOIN checkpoints c ON c.id=q.checkpoint_id JOIN pulsations p ON p.id=q.pulsation_id WHERE p.project_id=?1 AND c.purpose='clarification' AND c.status='waiting' AND p.status='checkpoint' AND p.input_revision=q.input_revision AND p.state_version=q.state_version AND q.answers_json IS NULL ORDER BY q.id")?
            .query_map([project_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?,r.get::<_,u64>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,String>(8)?)))?
            .collect::<Result<Vec<_>,_>>()?;
        rows.into_iter()
            .map(
                |(id, checkpoint, revision, generation, state, conversation, json, pulse, job)| {
                    Ok(SpecificationQuestionSet {
                        id,
                        checkpoint_id: checkpoint,
                        conversation_id: conversation,
                        fence: ExecutionFence {
                            project_id: project_id.to_owned(),
                            pulsation_id: Some(pulse),
                            input_revision: Some(revision),
                            job_id: job,
                            generation,
                            state_version: state,
                        },
                        questions: serde_json::from_str(&json)
                            .map_err(|e| invalid(e.to_string()))?,
                    })
                },
            )
            .collect()
    }

    pub(super) fn persist_questions(
        db: &rusqlite::Transaction<'_>,
        current: &Pulsation,
        fence: &ExecutionFence,
        questions: &[SpecificationQuestion],
    ) -> Result<(), PersistenceServiceError> {
        let updated = Self::transition_pulsation(
            db,
            current,
            PulsationStatus::Checkpoint,
            PulsationStage::Specification,
            "specification_needs_clarification",
            Some(
                "Specification is waiting for your answers; the specification cannot be approved yet.",
            ),
        )?;
        Self::open_checkpoint(db, &updated.id, updated.stage)?;
        db.execute("UPDATE checkpoints SET kind='clarification',purpose='clarification',input_revision=?2,state_version=(SELECT state_version FROM pulsations WHERE id=?1),producer_job_id=?3,generation=?4 WHERE pulsation_id=?1 AND status='waiting' AND purpose='artifact_review'",params![current.id,fence.input_revision,fence.job_id,fence.generation])?;
        let (checkpoint, state): (String, u64) = db.query_row(
            "SELECT id,state_version FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' AND purpose='clarification'",
            [&current.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let conversation: Option<String> = db
            .query_row(
                "SELECT conversation_id FROM specification_conversations WHERE pulsation_id=?1",
                [&current.id],
                |r| r.get(0),
            )
            .optional()?;
        let conversation = if let Some(id) = conversation {
            id
        } else {
            let id = Uuid::new_v4().to_string();
            db.execute("INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at) VALUES(?1,?2,'specification','host-presented',?3,?4,?4)",params![id,current.project_id,format!("Specification · {}",current.feature_name.chars().take(80).collect::<String>()),Self::now_iso()])?;
            db.execute("INSERT INTO specification_conversations(pulsation_id,conversation_id) VALUES(?1,?2)",params![current.id,id])?;
            id
        };
        let id = Uuid::new_v4().to_string();
        db.execute("INSERT INTO specification_question_sets(id,checkpoint_id,pulsation_id,input_revision,generation,state_version,conversation_id,questions_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![id,checkpoint,current.id,fence.input_revision,fence.generation,state,conversation,serde_json::to_string(questions).map_err(|e|invalid(e.to_string()))?])?;
        let content = format!(
            "Specification — clarifications (revision {}, set {}). Answer in Pulsations; this message is not an approval.\n\n{}",
            fence.input_revision.unwrap_or(0),
            id,
            questions
                .iter()
                .map(|q| format!("[{}] {}\nMotivo: {}", q.id, q.question, q.rationale))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
        Self::append_specification_message(db, &conversation, "assistant", &content)?;
        Ok(())
    }

    fn append_specification_message(
        db: &Connection,
        conversation: &str,
        role: &str,
        content: &str,
    ) -> Result<(), PersistenceServiceError> {
        db.execute("INSERT INTO chat_messages(id,conversation_id,ordinal,role,content,status,created_at,updated_at) VALUES(?1,?2,(SELECT COALESCE(MAX(ordinal),-1)+1 FROM chat_messages WHERE conversation_id=?2),?3,?4,'completed',?5,?5)",params![Uuid::new_v4().to_string(),conversation,role,content,Self::now_iso()])?;
        db.execute(
            "UPDATE chat_conversations SET updated_at=?2 WHERE id=?1",
            params![conversation, Self::now_iso()],
        )?;
        Ok(())
    }

    pub fn submit_specification_answers(
        &self,
        request: SubmitSpecificationAnswersRequest,
    ) -> Result<Pulsation, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_project_admission(&tx, &request.pulsation_id)?;
        let pending = Self::pending_questions(&tx, &request.pulsation_id)?.ok_or_else(|| {
            invalid("Questions already answered or checkpoint stale. Refresh before sending.")
        })?;
        validate_specification_answers(&request, &pending).map_err(|e| invalid(e.to_string()))?;
        let current = Self::find_pulsation_by_id(&tx, &request.pulsation_id)?
            .ok_or_else(|| invalid("Pulsation missing."))?;
        if current.stage != PulsationStage::Specification {
            return Err(invalid("Esclarecimento fora de Specification."));
        }
        let workspace_root: String = tx.query_row(
            "SELECT workspace_path FROM projects WHERE id=?1",
            [&current.project_id],
            |r| r.get(0),
        )?;
        let mut answers = request.answers.clone();
        for answer in &mut answers {
            // The UI/Companion may already have parsed and populated these
            // (M5); a plain-text answer typed today (before M5's modal
            // ships) always arrives with both empty, so the host derives
            // them once here, before the answer is ever persisted or read
            // back by a later stage's `collect_stage_inputs`.
            if answer.planned_paths.is_empty() && answer.modified_paths.is_empty() {
                Self::anchor_clarification_answer(&workspace_root, answer);
            }
        }
        let answers_json = serde_json::to_string(&answers).map_err(|e| invalid(e.to_string()))?;
        // Check the entire accumulated normative history BEFORE closing the question UI.
        let config = self.load_runtime_config()?;
        let mut history =
            Self::clarification_history(&tx, &current.id, Some(request.expected_input_revision))?;
        history.push(ClarificationExchange {
            question_set_id: pending.id.clone(),
            questions: pending.questions.clone(),
            answers: answers.clone(),
        });
        let history_chars = serde_json::to_string(&history)
            .map_err(|e| invalid(e.to_string()))?
            .chars()
            .count();
        let answer_chars = history
            .iter()
            .flat_map(|c| &c.answers)
            .map(|a| a.text_answer.chars().count())
            .sum::<usize>();
        let feedback_chars: usize = tx.query_row(
            "SELECT COALESCE(length(revision_feedback),0) FROM pulsations WHERE id=?1",
            [&current.id],
            |r| r.get(0),
        )?;
        let artifacts:usize=tx.query_row("SELECT COALESCE(SUM(length(content)),0) FROM artifacts WHERE pulsation_id=?1 AND input_revision=?2 AND acceptance='accepted' AND kind!='code_context'",params![current.id,request.expected_input_revision],|r|r.get(0))?;
        if history_chars + answer_chars + feedback_chars + artifacts + 2048
            > config.context_chars_limit
        {
            return Err(invalid(
                "The answers and history exceed the context budget. Shorten the answers or raise the limit before sending.",
            ));
        }
        tx.execute("UPDATE specification_question_sets SET answers_json=?2 WHERE id=?1 AND answers_json IS NULL",params![pending.id,answers_json])?;
        // 'approved' resolves only the clarification checkpoint; kind remains distinct forever.
        tx.execute(
            "UPDATE checkpoints SET status='approved',updated_at=?2 WHERE id=?1",
            params![pending.checkpoint_id, Self::now_iso()],
        )?;
        let state_version: u64 = tx.query_row(
            "SELECT state_version FROM pulsations WHERE id=?1",
            [&current.id],
            |r| r.get(0),
        )?;
        Self::record_approval_receipt(
            &tx,
            super::mandate_persistence::ReceiptInput {
                checkpoint_id: &pending.checkpoint_id,
                pulsation_id: &current.id,
                purpose: "clarification",
                stage: current.stage.as_db_value(),
                decision: "answered",
                state_version,
                reviewed: Vec::new(),
                confirmed_paths: Vec::new(),
                feedback: Some(&answers_json),
            },
        )?;
        let text = format!(
            "Answers to set {}:\n\n{}",
            pending.id,
            answers
                .iter()
                .map(|a| format!("[{}] {}", a.question_id, a.text_answer))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
        Self::append_specification_message(&tx, &pending.conversation_id, "user", &text)?;
        let updated = Self::transition_pulsation(
            &tx,
            &current,
            PulsationStatus::Planning,
            PulsationStage::Specification,
            "specification_answers_submitted",
            Some("Answers recorded; Specification will run a new generation."),
        )?;
        Self::enqueue_stage_job(&tx, &current.id, PulsationStage::Specification)?;
        // An answer is new human input, like a revision: the re-run opens a
        // fresh v4 stage allowance (linked to the spent one) instead of
        // paying for the answer with the unit's single correction permit.
        // Two rounds used to end in `budget_exhausted` before any repair.
        // Rounds stay bounded by the context check above. No-op before v4.
        Self::close_budget_accounts(&tx, &current.id, PulsationStage::Specification)?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(updated)
    }

    /// Populates `planned_paths`/`modified_paths` from the `FILES_*:` tag
    /// lines in `answer.text_answer` (ADR-022 clarification loop, P2/A2.2).
    /// `FILES_NEW:`/`FILES_MOD:` are unambiguous; a bare `FILES:` is resolved
    /// against the workspace here (the parser itself has no disk access) --
    /// present on disk means the developer is pointing at an existing file,
    /// absent means a new one. The full prose in `text_answer`, including the
    /// raw tag lines, is left untouched: `batch_guidance` still injects it
    /// verbatim, unaware this parsing ever happened.
    fn anchor_clarification_answer(workspace_root: &str, answer: &mut ClarificationAnswerV1) {
        let (planned, modified, unresolved) =
            crate::orchestration::composition::recipes::parse_clarification_tags(
                &answer.text_answer,
            );
        let mut planned_paths: Vec<String> = planned
            .into_iter()
            .map(|path| path.as_str().to_owned())
            .collect();
        let mut modified_paths: Vec<String> = modified
            .into_iter()
            .map(|path| path.as_str().to_owned())
            .collect();
        let root = std::path::Path::new(workspace_root);
        for path in unresolved {
            match crate::orchestration::publication::observed_hash(root, path.as_str()) {
                Ok(Some(_)) => modified_paths.push(path.as_str().to_owned()),
                Ok(None) | Err(_) => planned_paths.push(path.as_str().to_owned()),
            }
        }
        answer.planned_paths = planned_paths;
        answer.modified_paths = modified_paths;
    }

    pub(super) fn clarification_history(
        db: &Connection,
        pulse: &str,
        revision: Option<u64>,
    ) -> Result<Vec<ClarificationExchange>, PersistenceServiceError> {
        let rows=db.prepare("SELECT id,questions_json,answers_json FROM specification_question_sets WHERE pulsation_id=?1 AND input_revision=?2 AND answers_json IS NOT NULL ORDER BY state_version,id")?.query_map(params![pulse,revision],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        rows.into_iter()
            .map(|(id, q, a)| {
                Ok(ClarificationExchange {
                    question_set_id: id,
                    questions: serde_json::from_str(&q).map_err(|e| invalid(e.to_string()))?,
                    answers: serde_json::from_str(&a).map_err(|e| invalid(e.to_string()))?,
                })
            })
            .collect()
    }
}
