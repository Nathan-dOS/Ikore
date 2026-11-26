//! Feature 25 (M4): persistence store and CRUD for stage verifiers and their
//! runs, plus Review Brief finding aggregation (P18, P21). A module of
//! `persistence_service`.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use super::{PersistenceService, PersistenceServiceError, PulsationStage};
use crate::custom_pipelines::trigger_stage_label;
use crate::stage_verifiers::{
    FindingSeverity, MAX_VERIFIERS_PER_PROJECT, MAX_VERIFIERS_PER_STAGE, PipelineValidationError,
    SaveStageVerifierOutcome, StageVerdictV1, StageVerifierConfig, StageVerifierDraft,
    StageVerifierRunView, StageVerifierView, StageVerifiersSnapshot, StepAgent, StepTransport,
    parse_verifier_config, validate_verifier_draft,
};

fn invalid(message: impl Into<String>) -> PersistenceServiceError {
    PersistenceServiceError::Validation(message.into())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn stage_order(stage: &str) -> usize {
    match stage {
        "Intake" => 0,
        "SPECIFICATION" => 1,
        "Contract" => 2,
        "Code" => 3,
        "Test" => 4,
        "Integrator" => 5,
        _ => 6,
    }
}

fn ensure_active_project(tx: &Connection, project_id: &str) -> Result<(), PersistenceServiceError> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1 AND is_archived=0)",
        [project_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(PersistenceServiceError::NotFound("project".into()))
    }
}

fn validate_draft_for_project(
    tx: &Transaction<'_>,
    project_id: &str,
    verifier_id: Option<&str>,
    draft: &StageVerifierDraft,
) -> Result<Vec<PipelineValidationError>, PersistenceServiceError> {
    let mut errors = validate_verifier_draft(draft);
    if draft.label.contains('\0') {
        errors.push(PipelineValidationError::new(
            "label_invalid",
            None,
            "A verifier's label cannot contain a NUL character.",
        ));
    }
    if draft.instructions.contains('\0') {
        errors.push(PipelineValidationError::new(
            "instructions_invalid",
            None,
            "Verifier instructions cannot contain a NUL character.",
        ));
    }
    if draft.agent.transport == StepTransport::Api
        && let Some(route_id) = draft
            .agent
            .api_route_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    {
        let enabled: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM provider_routes WHERE id=?1 AND enabled=1)",
            [route_id],
            |row| row.get(0),
        )?;
        if !enabled {
            errors.push(PipelineValidationError::new(
                "api_route_unknown",
                None,
                "This API route does not exist or is disabled.",
            ));
        }
    }

    let trimmed_label = draft.label.trim();
    if !trimmed_label.is_empty() && !draft.stage.trim().is_empty() {
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM stage_verifiers
                WHERE project_id=?1 AND stage=?2 AND lower(trim(label))=lower(?3)
                  AND id<>COALESCE(?4,'')
             )",
            params![project_id, draft.stage, trimmed_label, verifier_id],
            |row| row.get(0),
        )?;
        if duplicate {
            errors.push(PipelineValidationError::new(
                "label_taken",
                None,
                "Another verifier on this stage already uses that name.",
            ));
        }
    }

    let stage_changed = match verifier_id {
        None => true,
        Some(id) => {
            let existing_stage: Option<String> = tx
                .query_row(
                    "SELECT stage FROM stage_verifiers WHERE id=?1 AND project_id=?2",
                    params![id, project_id],
                    |row| row.get(0),
                )
                .optional()?;
            existing_stage.as_deref() != Some(draft.stage.as_str())
        }
    };

    if stage_changed && !draft.stage.trim().is_empty() {
        let stage_count: i64 = tx.query_row(
            "SELECT count(*) FROM stage_verifiers WHERE project_id=?1 AND stage=?2",
            params![project_id, draft.stage],
            |row| row.get(0),
        )?;
        if stage_count >= MAX_VERIFIERS_PER_STAGE as i64 {
            errors.push(PipelineValidationError::new(
                "stage_limit_reached",
                None,
                format!(
                    "Each stage can have at most {MAX_VERIFIERS_PER_STAGE} verifiers. Remove one first."
                ),
            ));
        }
    }

    if verifier_id.is_none() {
        let project_count: i64 = tx.query_row(
            "SELECT count(*) FROM stage_verifiers WHERE project_id=?1",
            [project_id],
            |row| row.get(0),
        )?;
        if project_count >= MAX_VERIFIERS_PER_PROJECT as i64 {
            errors.push(PipelineValidationError::new(
                "project_limit_reached",
                None,
                format!(
                    "A project can have at most {MAX_VERIFIERS_PER_PROJECT} stage verifiers. Remove one first."
                ),
            ));
        }
    }

    Ok(errors)
}

fn map_verifier_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StageVerifierView> {
    let agent_json: String = row.get(7)?;
    let agent: StepAgent = serde_json::from_str(&agent_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let reject_limit_i64: i64 = row.get(6)?;
    Ok(StageVerifierView {
        id: row.get(0)?,
        project_id: row.get(1)?,
        stage: row.get(2)?,
        label: row.get(3)?,
        position: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        reject_limit: u8::try_from(reject_limit_i64).unwrap_or(2),
        agent,
        instructions: row.get(8)?,
        version: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        run_count: row.get::<_, i64>(12)?.max(0) as u64,
    })
}

fn load_verifier_by_id(
    db: &Connection,
    verifier_id: &str,
) -> Result<StageVerifierView, PersistenceServiceError> {
    db.query_row(
        "SELECT v.id,v.project_id,v.stage,v.label,v.position,v.enabled,v.reject_limit,
                v.agent_json,v.instructions,v.version,v.created_at,v.updated_at,
                (SELECT count(*) FROM stage_verifier_runs r WHERE r.verifier_id=v.id)
           FROM stage_verifiers v
          WHERE v.id=?1",
        [verifier_id],
        map_verifier_row,
    )
    .optional()?
    .ok_or_else(|| PersistenceServiceError::NotFound("Stage verifier not found.".into()))
}

fn map_verifier_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StageVerifierRunView> {
    let config_json: String = row.get(2)?;
    let verdict_json: Option<String> = row.get(11)?;
    let config: Option<StageVerifierConfig> = parse_verifier_config(&config_json).ok();
    let label_fallback: String = row.get(15)?;
    let reject_limit = config.as_ref().map(|c| c.reject_limit).unwrap_or(2);
    let verifier_label = config
        .as_ref()
        .map(|c| c.label.clone())
        .unwrap_or(label_fallback);
    let verdict: Option<StageVerdictV1> = verdict_json
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok());
    Ok(StageVerifierRunView {
        id: row.get(0)?,
        verifier_id: row.get(1)?,
        verifier_label,
        stage: row.get(3)?,
        pulsation_id: row.get(4)?,
        stage_job_id: row.get(5)?,
        stage_generation: row.get::<_, i64>(6)?.max(0) as u64,
        attempt: row.get::<_, i64>(7)?.max(0) as u64,
        recovery_count: row.get::<_, i64>(8)?.max(0) as u64,
        status: row.get(9)?,
        reject_limit,
        advisory: reject_limit == 0,
        terminal_run_id: row.get(10)?,
        llm_execution_id: row.get(16)?,
        verdict,
        created_at: row.get(12)?,
        started_at: row.get(13)?,
        finished_at: row.get(14)?,
        error: row.get(17)?,
    })
}

const VERIFIER_RUN_SELECT: &str = "SELECT r.id,r.verifier_id,r.config_json,r.stage,r.pulsation_id,
        r.stage_job_id,r.stage_generation,r.attempt,r.recovery_count,r.status,
        r.terminal_run_id,r.verdict_json,r.created_at,r.started_at,r.finished_at,
        COALESCE(v.label,'Verifier'),r.llm_execution_id,r.error
   FROM stage_verifier_runs r
   LEFT JOIN stage_verifiers v ON v.id=r.verifier_id";

pub(super) fn list_verifiers_on(
    db: &Connection,
    project_id: &str,
) -> Result<Vec<StageVerifierView>, PersistenceServiceError> {
    let mut rows = db
        .prepare(
            "SELECT v.id,v.project_id,v.stage,v.label,v.position,v.enabled,v.reject_limit,
                    v.agent_json,v.instructions,v.version,v.created_at,v.updated_at,
                    (SELECT count(*) FROM stage_verifier_runs r WHERE r.verifier_id=v.id)
               FROM stage_verifiers v
              WHERE v.project_id=?1
              ORDER BY v.position,v.created_at,v.id",
        )?
        .query_map([project_id], map_verifier_row)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.sort_by(|left, right| {
        stage_order(&left.stage)
            .cmp(&stage_order(&right.stage))
            .then_with(|| left.position.cmp(&right.position))
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(rows)
}

impl PersistenceService {
    /// All configured stage verifiers for a project, ordered by stage and position.
    pub fn list_stage_verifiers(
        &self,
        project_id: &str,
    ) -> Result<Vec<StageVerifierView>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let db = self.connection()?;
        ensure_active_project(&db, &project_id)?;
        list_verifiers_on(&db, &project_id)
    }

    /// Project verifiers, Labs flag, and recent runs for the Agents canvas & dock.
    pub fn stage_verifiers_snapshot(
        &self,
        project_id: &str,
    ) -> Result<StageVerifiersSnapshot, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let db = self.connection()?;
        ensure_active_project(&db, &project_id)?;
        let enabled_by_labs =
            Self::composition_settings_for(&db, &project_id)?.stage_verifiers_enabled;
        let verifiers = list_verifiers_on(&db, &project_id)?;
        let runs = db
            .prepare(&format!(
                "{VERIFIER_RUN_SELECT}
                  JOIN pulsations p ON p.id=r.pulsation_id
                 WHERE p.project_id=?1
                 ORDER BY r.created_at DESC,r.rowid DESC
                 LIMIT 100"
            ))?
            .query_map([&project_id], map_verifier_run_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(StageVerifiersSnapshot {
            project_id,
            enabled_by_labs,
            verifiers,
            runs,
        })
    }

    /// Creates a new stage verifier attached to a default stage.
    pub fn create_stage_verifier(
        &self,
        project_id: &str,
        draft: StageVerifierDraft,
    ) -> Result<SaveStageVerifierOutcome, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_project(&tx, &project_id)?;
        let errors = validate_draft_for_project(&tx, &project_id, None, &draft)?;
        if !errors.is_empty() {
            return Ok(SaveStageVerifierOutcome::Invalid { errors });
        }
        let id = Uuid::new_v4().to_string();
        let now = now();
        let trimmed_label = draft.label.trim().to_owned();
        let next_position: i64 = if draft.position == 0 {
            tx.query_row(
                "SELECT COALESCE(MAX(position)+1,0) FROM stage_verifiers WHERE project_id=?1 AND stage=?2",
                params![project_id, draft.stage],
                |row| row.get(0),
            )?
        } else {
            draft.position
        };
        let agent_json =
            serde_json::to_string(&draft.agent).map_err(|error| invalid(error.to_string()))?;
        tx.execute(
            "INSERT INTO stage_verifiers(
               id,project_id,stage,label,position,enabled,reject_limit,agent_json,instructions,version,created_at,updated_at
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10,?10)",
            params![
                id,
                project_id,
                draft.stage,
                trimmed_label,
                next_position,
                draft.enabled as i64,
                draft.reject_limit as i64,
                agent_json,
                draft.instructions,
                now,
            ],
        )?;
        let view = load_verifier_by_id(&tx, &id)?;
        tx.commit()?;
        Ok(SaveStageVerifierOutcome::Saved {
            verifier: Box::new(view),
        })
    }

    /// Updates a stage verifier, incrementing `version`. Past runs preserve
    /// their `config_json` snapshot (T4 / P15).
    pub fn update_stage_verifier(
        &self,
        project_id: &str,
        verifier_id: &str,
        draft: StageVerifierDraft,
    ) -> Result<SaveStageVerifierOutcome, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let verifier_id = verifier_id.trim();
        if verifier_id.is_empty() {
            return Err(invalid("Verifier id cannot be empty."));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_project(&tx, &project_id)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_verifiers WHERE id=?1 AND project_id=?2)",
            params![verifier_id, project_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(PersistenceServiceError::NotFound(
                "Stage verifier not found.".into(),
            ));
        }
        let errors = validate_draft_for_project(&tx, &project_id, Some(verifier_id), &draft)?;
        if !errors.is_empty() {
            return Ok(SaveStageVerifierOutcome::Invalid { errors });
        }
        let now = now();
        let trimmed_label = draft.label.trim().to_owned();
        let agent_json =
            serde_json::to_string(&draft.agent).map_err(|error| invalid(error.to_string()))?;
        tx.execute(
            "UPDATE stage_verifiers
                SET stage=?3,label=?4,position=?5,enabled=?6,reject_limit=?7,agent_json=?8,
                    instructions=?9,version=version+1,updated_at=?10
              WHERE id=?1 AND project_id=?2",
            params![
                verifier_id,
                project_id,
                draft.stage,
                trimmed_label,
                draft.position,
                draft.enabled as i64,
                draft.reject_limit as i64,
                agent_json,
                draft.instructions,
                now,
            ],
        )?;
        let view = load_verifier_by_id(&tx, verifier_id)?;
        tx.commit()?;
        Ok(SaveStageVerifierOutcome::Saved {
            verifier: Box::new(view),
        })
    }

    /// Enables or disables a stage verifier without removing its history.
    pub fn set_stage_verifier_enabled(
        &self,
        project_id: &str,
        verifier_id: &str,
        enabled: bool,
    ) -> Result<StageVerifierView, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let verifier_id = verifier_id.trim();
        if verifier_id.is_empty() {
            return Err(invalid("Verifier id cannot be empty."));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_project(&tx, &project_id)?;
        let changed = tx.execute(
            "UPDATE stage_verifiers SET enabled=?3,updated_at=?4 WHERE id=?1 AND project_id=?2",
            params![verifier_id, project_id, enabled as i64, now()],
        )?;
        if changed != 1 {
            return Err(PersistenceServiceError::NotFound(
                "Stage verifier not found.".into(),
            ));
        }
        let view = load_verifier_by_id(&tx, verifier_id)?;
        tx.commit()?;
        Ok(view)
    }

    /// Deletes a verifier only when it has zero recorded runs (`ON DELETE RESTRICT`, P16).
    pub fn delete_stage_verifier(
        &self,
        project_id: &str,
        verifier_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let verifier_id = verifier_id.trim();
        if verifier_id.is_empty() {
            return Err(invalid("Verifier id cannot be empty."));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_active_project(&tx, &project_id)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM stage_verifiers WHERE id=?1 AND project_id=?2)",
            params![verifier_id, project_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(PersistenceServiceError::NotFound(
                "Stage verifier not found.".into(),
            ));
        }
        let runs: i64 = tx.query_row(
            "SELECT count(*) FROM stage_verifier_runs WHERE verifier_id=?1",
            [verifier_id],
            |row| row.get(0),
        )?;
        if runs > 0 {
            return Err(invalid(
                "A verifier with recorded runs cannot be deleted; disable it instead.",
            ));
        }
        tx.execute(
            "DELETE FROM stage_verifiers WHERE id=?1 AND project_id=?2",
            params![verifier_id, project_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// All verifier runs recorded for a pulsation, oldest first.
    pub fn list_stage_verifier_runs_for_pulsation(
        &self,
        pulsation_id: &str,
    ) -> Result<Vec<StageVerifierRunView>, PersistenceServiceError> {
        let pulsation_id = Self::normalize_pulsation_id(pulsation_id)?;
        let db = self.connection()?;
        let rows = db
            .prepare(&format!(
                "{VERIFIER_RUN_SELECT}
                  WHERE r.pulsation_id=?1
                  ORDER BY r.stage_generation,r.created_at,r.attempt,r.rowid"
            ))?
            .query_map([&pulsation_id], map_verifier_run_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// All runs for a single verifier, newest first.
    pub fn list_stage_verifier_runs_for_verifier(
        &self,
        verifier_id: &str,
    ) -> Result<Vec<StageVerifierRunView>, PersistenceServiceError> {
        let verifier_id = verifier_id.trim();
        if verifier_id.is_empty() {
            return Err(invalid("Verifier id cannot be empty."));
        }
        let db = self.connection()?;
        let rows = db
            .prepare(&format!(
                "{VERIFIER_RUN_SELECT}
                  WHERE r.verifier_id=?1
                  ORDER BY r.created_at DESC,r.attempt DESC,r.rowid DESC"
            ))?
            .query_map([verifier_id], map_verifier_run_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Verifier findings shown in the Review Brief tab for `stage`, including
    /// non-blocking carryover from `Intake` into `Specification` and from `Code`
    /// into `Test` (P21, P23).
    pub fn list_checkpoint_verification_findings(
        &self,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<Vec<StageVerifierRunView>, PersistenceServiceError> {
        let pulsation_id = Self::normalize_pulsation_id(pulsation_id)?;
        let db = self.connection()?;
        Self::checkpoint_verification_findings_on(&db, &pulsation_id, stage)
    }

    pub(crate) fn checkpoint_verification_findings_on(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<Vec<StageVerifierRunView>, PersistenceServiceError> {
        let mut results = latest_runs_with_verdict_for_stage(db, pulsation_id, stage, false)?;
        let carryover_stage = match stage {
            PulsationStage::Specification => Some(PulsationStage::Intake),
            PulsationStage::Test => Some(PulsationStage::Code),
            _ => None,
        };
        if let Some(source_stage) = carryover_stage {
            let carried = latest_runs_with_verdict_for_stage(db, pulsation_id, source_stage, true)?;
            results.extend(carried);
        }
        Ok(results)
    }
}

fn latest_runs_with_verdict_for_stage(
    db: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
    non_blocking_only: bool,
) -> Result<Vec<StageVerifierRunView>, PersistenceServiceError> {
    let latest_generation: Option<i64> = db
        .query_row(
            "SELECT MAX(stage_generation) FROM stage_verifier_runs
              WHERE pulsation_id=?1 AND stage=?2",
            params![pulsation_id, stage.as_db_value()],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(generation) = latest_generation else {
        return Ok(Vec::new());
    };
    let rows = db
        .prepare(&format!(
            "{VERIFIER_RUN_SELECT}
              WHERE r.pulsation_id=?1 AND r.stage=?2 AND r.stage_generation=?3
                AND r.verdict_json IS NOT NULL
              ORDER BY COALESCE(json_extract(r.config_json,'$.position'),0),r.created_at,r.attempt DESC"
        ))?
        .query_map(
            params![pulsation_id, stage.as_db_value(), generation],
            map_verifier_run_row,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    // Keep only the latest attempt per verifier.
    let mut by_verifier: BTreeMap<(i64, String), StageVerifierRunView> = BTreeMap::new();
    for (order, mut run) in rows.into_iter().enumerate() {
        if non_blocking_only && let Some(verdict) = &mut run.verdict {
            verdict
                .findings
                .retain(|finding| finding.severity != FindingSeverity::Blocking);
        }
        let has_findings = run
            .verdict
            .as_ref()
            .is_some_and(|verdict| !verdict.findings.is_empty());
        if !has_findings {
            continue;
        }
        let key = (order as i64, run.verifier_id.clone());
        let dominated = by_verifier.values().any(|existing| {
            existing.verifier_id == run.verifier_id && existing.attempt >= run.attempt
        });
        if !dominated {
            by_verifier.retain(|_, existing| existing.verifier_id != run.verifier_id);
            by_verifier.insert(key, run);
        }
    }
    let mut out: Vec<StageVerifierRunView> = by_verifier.into_values().collect();
    for run in &mut out {
        if let Some(verdict) = &mut run.verdict {
            verdict
                .findings
                .sort_by_key(|finding| match finding.severity {
                    FindingSeverity::Blocking => 0,
                    FindingSeverity::Warning => 1,
                    FindingSeverity::Note => 2,
                });
        }
    }
    let _ = trigger_stage_label;
    Ok(out)
}
