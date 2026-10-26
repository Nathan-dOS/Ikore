//! Durable state of composed stage executions (ADR-022): settings, policy
//! snapshots, budget accounts and reservations, executions, units, attempts,
//! child acceptance and the observation snapshot. Included into
//! `persistence_service` so the coordinator shares the service's private
//! transaction helpers. Every write that changes a state follows the
//! transitions in `Stage-Composition-Persistence-and-Recovery.md` and uses a
//! compare-and-swap on the row's version.

use super::{
    Connection, Deserialize, OptionalExtension, Path, PersistenceService, PersistenceServiceError,
    Pulsation, PulsationStage, PulsationStatus, ResolveStageFailureRequest, Serialize,
    StageFailureDecision, Type, Uuid, params,
};
use crate::orchestration::SchemaV1;
use crate::orchestration::composition::budget::{AccountLedger, Pool};
use crate::orchestration::composition::review_boundary::{
    BaseFact, BaseState, ReviewBoundary, ReviewInputManifestV1,
};
use crate::orchestration::composition::schema::SchemaFlavor;
use crate::orchestration::composition::settings::CompositionSettings;
use crate::orchestration::composition::validation::profile::detect_presets;
use crate::orchestration::composition::{
    self as comp, AttemptState, AttemptSummaryV1, CheckSummaryV1, CompositionError,
    ExecutionStatus, ExecutorKind, RemainingBudgetV1, StageBudgetV1, StageExecutionPlanV1,
    StageExecutionSnapshot, UnitSnapshotV1, UnitState, UsageCompleteness, UsageSummaryV1, Validate,
    WorkUnitSpecV1,
};
use crate::orchestration::composition::{
    ReceiptStatus, StageRecoveryCommandV1, ValidationCommandProfileV1, ValidationReceiptV1,
};
use crate::terminals::{AgentExecutionMode, CliBackend, SpecialtyOverride, ThinkingLevel};
use crate::usage::UsageTotals;
use std::collections::HashMap;

fn protected_diagnostic(text: &str) -> String {
    crate::llm::adapter::sanitize_model_text(text)
        .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned())
}

pub(crate) fn composition_invalid(error: &CompositionError) -> PersistenceServiceError {
    PersistenceServiceError::Validation(format!(
        "{} {}: {}",
        error.sc.as_str(),
        error.code.as_str(),
        error.message
    ))
}

/// A persisted stage execution row.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionRow {
    pub id: String,
    pub pulsation_id: String,
    pub parent_generation: u64,
    pub parent_state_version: u64,
    pub stage: PulsationStage,
    pub budget_account_id: String,
    pub plan: StageExecutionPlanV1,
    pub status: ExecutionStatus,
    pub execution_version: u64,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct UnitRow {
    pub id: String,
    pub logical_unit_key: String,
    pub executor_kind: ExecutorKind,
    pub spec: WorkUnitSpecV1,
    pub generation: u64,
    pub state_version: u64,
    pub state: UnitState,
    pub validated_result_json: Option<String>,
    pub validated_result_sha256: Option<String>,
    pub reused_from_attempt_id: Option<String>,
    pub reuse_key_sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReservationRow {
    pub id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct AttemptRow {
    pub id: String,
    pub reservation_id: String,
    pub agent_run_id: String,
}

/// The immutable transport/model profile frozen for one attempt (SC-014).
/// Never carries credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveProfile {
    pub executor_kind: String,
    pub transport: String,
    pub backend: Option<String>,
    pub requested_model: String,
    pub thinking_level: Option<String>,
    pub schema_flavor: SchemaFlavor,
    pub attempt_limit: u8,
    /// Source of the resolution: recovery override, specialty override or
    /// the stage's own role configuration.
    pub source: String,
}

fn execution_status_str(status: ExecutionStatus) -> &'static str {
    match status {
        ExecutionStatus::Planned => "planned",
        ExecutionStatus::Running => "running",
        ExecutionStatus::Validating => "validating",
        ExecutionStatus::Ready => "ready",
        ExecutionStatus::Accepted => "accepted",
        ExecutionStatus::Parked => "parked",
        ExecutionStatus::Cancelled => "cancelled",
        ExecutionStatus::Superseded => "superseded",
        ExecutionStatus::Failed => "failed",
    }
}

fn parse_execution_status(value: &str) -> Option<ExecutionStatus> {
    Some(match value {
        "planned" => ExecutionStatus::Planned,
        "running" => ExecutionStatus::Running,
        "validating" => ExecutionStatus::Validating,
        "ready" => ExecutionStatus::Ready,
        "accepted" => ExecutionStatus::Accepted,
        "parked" => ExecutionStatus::Parked,
        "cancelled" => ExecutionStatus::Cancelled,
        "superseded" => ExecutionStatus::Superseded,
        "failed" => ExecutionStatus::Failed,
        _ => return None,
    })
}

fn unit_state_str(state: UnitState) -> &'static str {
    match state {
        UnitState::Pending => "pending",
        UnitState::Running => "running",
        UnitState::Validated => "validated",
        UnitState::Failed => "failed",
        UnitState::Cancelled => "cancelled",
        UnitState::Invalidated => "invalidated",
    }
}

fn parse_unit_state(value: &str) -> Option<UnitState> {
    Some(match value {
        "pending" => UnitState::Pending,
        "running" => UnitState::Running,
        "validated" => UnitState::Validated,
        "failed" => UnitState::Failed,
        "cancelled" => UnitState::Cancelled,
        "invalidated" => UnitState::Invalidated,
        _ => return None,
    })
}

fn attempt_state_str(state: AttemptState) -> &'static str {
    match state {
        AttemptState::Reserved => "reserved",
        AttemptState::Running => "running",
        AttemptState::Succeeded => "succeeded",
        AttemptState::Rejected => "rejected",
        AttemptState::Interrupted => "interrupted",
        AttemptState::Cancelled => "cancelled",
    }
}

fn parse_attempt_state(value: &str) -> Option<AttemptState> {
    Some(match value {
        "reserved" => AttemptState::Reserved,
        "running" => AttemptState::Running,
        "succeeded" => AttemptState::Succeeded,
        "rejected" => AttemptState::Rejected,
        "interrupted" => AttemptState::Interrupted,
        "cancelled" => AttemptState::Cancelled,
        _ => return None,
    })
}

fn stale(what: &str) -> PersistenceServiceError {
    PersistenceServiceError::Validation(format!(
        "SC-015 stale_result: {what} changed under the coordinator; the result was discarded."
    ))
}

fn usage_summary(totals: &UsageTotals) -> UsageSummaryV1 {
    let clamp = |value: Option<i64>| value.map(|v| v.clamp(0, i64::from(i32::MAX)) as u32);
    let completeness = if totals.records == 0 || totals.unavailable_records == totals.records {
        UsageCompleteness::Unavailable
    } else if totals.partial_records > 0 || totals.unavailable_records > 0 {
        UsageCompleteness::Partial
    } else {
        UsageCompleteness::Complete
    };
    UsageSummaryV1 {
        input_tokens: clamp(totals.input_tokens),
        output_tokens: clamp(totals.output_tokens),
        reasoning_tokens: clamp(totals.reasoning_tokens),
        cached_input_tokens: clamp(totals.cache_read_tokens),
        completeness,
    }
}

impl PersistenceService {
    // ----------------------------------------------------------------- settings

    pub(crate) fn composition_settings_for(
        db: &Connection,
        project_id: &str,
    ) -> Result<CompositionSettings, PersistenceServiceError> {
        let row: Option<(bool, u32, bool, String)> = db
            .query_row(
                "SELECT enabled,max_in_flight_units,validation_enabled,settings_json FROM project_composition_settings WHERE project_id=?1",
                [project_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        Ok(match row {
            Some((enabled, max_in_flight_units, validation_enabled, extra)) => {
                // The typed columns are authoritative; `settings_json` carries
                // the fields added after V32 (qualified recipes, Visualizer).
                let extra: serde_json::Value =
                    serde_json::from_str(&extra).unwrap_or(serde_json::Value::Null);
                let qualified_recipes = extra["qualifiedRecipes"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                CompositionSettings {
                    enabled,
                    max_in_flight_units,
                    validation_enabled,
                    qualified_recipes,
                    visualizer_enabled: extra["visualizerEnabled"].as_bool().unwrap_or(false),
                    revision_round_threshold: extra["revisionRoundThreshold"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| {
                            comp::settings::REVISION_ROUND_THRESHOLD_RANGE.contains(value)
                        })
                        .unwrap_or(comp::settings::DEFAULT_REVISION_ROUND_THRESHOLD),
                    // Stored additions that no longer parse are dropped; ones
                    // that parse but fail validation stay visible, and the
                    // risk assessment then applies the built-in policy alone.
                    risk_policy: serde_json::from_value(extra["riskPolicy"].clone())
                        .unwrap_or_default(),
                }
            }
            None => CompositionSettings::default(),
        })
    }

    pub fn get_composition_settings(
        &self,
        project_id: &str,
    ) -> Result<CompositionSettings, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        Self::composition_settings_for(&self.connection()?, &project_id)
    }

    /// Persists project settings. Affects future admissions only.
    pub fn update_composition_settings(
        &self,
        project_id: &str,
        settings: CompositionSettings,
    ) -> Result<CompositionSettings, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        settings.validate().map_err(|e| composition_invalid(&e))?;
        let db = self.connection()?;
        if Self::find_project_by_id(&db, &project_id, false)?.is_none() {
            return Err(PersistenceServiceError::NotFound(
                "Project not found to configure composition.".into(),
            ));
        }
        let extra = serde_json::json!({
            "qualifiedRecipes": settings.qualified_recipes,
            "visualizerEnabled": settings.visualizer_enabled,
            "revisionRoundThreshold": settings.revision_round_threshold,
            "riskPolicy": settings.risk_policy,
        })
        .to_string();
        db.execute(
            "INSERT INTO project_composition_settings(project_id,enabled,max_in_flight_units,validation_enabled,settings_json,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(project_id) DO UPDATE SET enabled=excluded.enabled,max_in_flight_units=excluded.max_in_flight_units,validation_enabled=excluded.validation_enabled,settings_json=excluded.settings_json,updated_at=excluded.updated_at",
            params![
                project_id,
                settings.enabled,
                settings.max_in_flight_units,
                settings.validation_enabled,
                extra,
                Self::now_iso()
            ],
        )?;
        Ok(settings)
    }

    // ---------------------------------------------------------------- snapshots

    /// Stores an immutable policy snapshot, deduplicated by content digest.
    pub(crate) fn snapshot_policy(
        db: &Connection,
        kind: &str,
        project_id: Option<&str>,
        snapshot: &serde_json::Value,
    ) -> Result<(String, comp::Sha256Hex), PersistenceServiceError> {
        let sha = comp::canonical_sha256(snapshot, &[]).map_err(|e| composition_invalid(&e))?;
        let existing: Option<String> = db
            .query_row(
                "SELECT id FROM composition_policy_snapshots WHERE kind=?1 AND sha256=?2",
                params![kind, sha.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok((id, sha));
        }
        let id = Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO composition_policy_snapshots(id,kind,project_id,sha256,snapshot_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, kind, project_id, sha.as_str(), snapshot.to_string(), Self::now_iso()],
        )?;
        Ok((id, sha))
    }

    pub(crate) fn policy_snapshot_json(
        db: &Connection,
        id: &str,
    ) -> Result<serde_json::Value, PersistenceServiceError> {
        let text: String = db.query_row(
            "SELECT snapshot_json FROM composition_policy_snapshots WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        serde_json::from_str(&text).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
    }

    // ------------------------------------------------------------------ budgets

    /// Opens (or reuses) the durable budget account of one logical stage.
    /// Retry generations share the account; a closed account (human scope or
    /// capacity revision) yields a new one linked to its predecessor.
    pub(crate) fn open_budget_account(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
        input_revision: u64,
        budget: &StageBudgetV1,
        policy_sha256: &str,
    ) -> Result<String, PersistenceServiceError> {
        let base = format!("{pulsation_id}:{}:{input_revision}", stage.as_db_value());
        let latest: Option<(String, String, i64)> = db
            .query_row(
                "SELECT id,state,(SELECT COUNT(*) FROM stage_budget_accounts b WHERE b.pulsation_id=?1 AND b.stage=?2 AND b.logical_stage_key LIKE ?3) FROM stage_budget_accounts WHERE pulsation_id=?1 AND stage=?2 AND logical_stage_key LIKE ?3 ORDER BY created_at DESC LIMIT 1",
                params![pulsation_id, stage.as_db_value(), format!("{base}:%")],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (predecessor, ordinal) = match latest {
            Some((id, state, count)) if state != "closed" => {
                let _ = count;
                return Ok(id);
            }
            Some((id, _, count)) => (Some(id), count + 1),
            None => (None, 1),
        };
        let id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        db.execute(
            "INSERT INTO stage_budget_accounts(id,pulsation_id,stage,logical_stage_key,policy_sha256,initial_invocation_limit,extra_invocation_limit,initial_output_token_limit,extra_output_token_limit,initial_prompt_char_limit,extra_prompt_char_limit,state,predecessor_account_id,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'open',?12,?13,?13)",
            params![
                id,
                pulsation_id,
                stage.as_db_value(),
                format!("{base}:{ordinal}"),
                policy_sha256,
                budget.initial_invocation_limit,
                budget.extra_invocation_limit,
                budget.initial_output_token_limit,
                budget.extra_output_token_limit,
                budget.initial_prompt_character_limit,
                budget.extra_prompt_character_limit,
                predecessor,
                now
            ],
        )?;
        Ok(id)
    }

    /// Marks the current account of a logical stage closed so the next
    /// admission opens a fresh one (human scope/capacity revision).
    pub(crate) fn close_budget_accounts(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<(), PersistenceServiceError> {
        db.execute(
            "UPDATE stage_budget_accounts SET state='closed',updated_at=?3 WHERE pulsation_id=?1 AND stage=?2 AND state<>'closed'",
            params![pulsation_id, stage.as_db_value(), Self::now_iso()],
        )?;
        Ok(())
    }

    /// The account's limits and currently held reservations.
    pub(crate) fn load_ledger(
        db: &Connection,
        account_id: &str,
    ) -> Result<AccountLedger, PersistenceServiceError> {
        let limits = db.query_row(
            "SELECT initial_invocation_limit,extra_invocation_limit,initial_output_token_limit,extra_output_token_limit,initial_prompt_char_limit,extra_prompt_char_limit FROM stage_budget_accounts WHERE id=?1",
            [account_id],
            |r| {
                Ok(StageBudgetV1 {
                    initial_invocation_limit: r.get(0)?,
                    extra_invocation_limit: r.get(1)?,
                    per_unit_extra_invocation_limit: 1,
                    initial_output_token_limit: r.get(2)?,
                    extra_output_token_limit: r.get(3)?,
                    initial_prompt_character_limit: r.get(4)?,
                    extra_prompt_character_limit: r.get(5)?,
                    max_in_flight_units: 1,
                })
            },
        )?;
        let held = |pool: &str| -> Result<(u32, u32, u32), PersistenceServiceError> {
            Ok(db.query_row(
                "SELECT COUNT(*),COALESCE(SUM(reserved_output_tokens),0),COALESCE(SUM(reserved_prompt_chars),0) FROM stage_invocation_reservations WHERE budget_account_id=?1 AND pool=?2 AND state<>'released'",
                params![account_id, pool],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32)),
            )?)
        };
        let (initial_count, initial_output, initial_chars) = held("initial")?;
        let (extra_count, extra_output, extra_chars) = held("extra")?;
        Ok(AccountLedger {
            limits: Some(limits),
            held_initial_invocations: initial_count,
            held_extra_invocations: extra_count,
            held_initial_output_tokens: initial_output,
            held_extra_output_tokens: extra_output,
            held_initial_prompt_chars: initial_chars,
            held_extra_prompt_chars: extra_chars,
        })
    }

    pub(crate) fn unit_extra_used(
        db: &Connection,
        account_id: &str,
        logical_unit_key: &str,
    ) -> Result<u32, PersistenceServiceError> {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM stage_invocation_reservations WHERE budget_account_id=?1 AND logical_unit_key=?2 AND pool='extra' AND state<>'released'",
            params![account_id, logical_unit_key],
            |r| r.get(0),
        )?)
    }

    /// Reserves one invocation atomically. The idempotency key ties the
    /// reservation to the unit generation and ordinal so a replay cannot
    /// double-reserve.
    pub(crate) fn reserve_invocation(
        db: &Connection,
        account_id: &str,
        execution_id: &str,
        unit: &UnitRow,
        pool: Pool,
        ordinal: u64,
        output_tokens: u32,
        prompt_chars: u32,
    ) -> Result<ReservationRow, PersistenceServiceError> {
        let ledger = Self::load_ledger(db, account_id)?;
        let extra_used = Self::unit_extra_used(db, account_id, &unit.logical_unit_key)?;
        ledger
            .can_reserve(pool, output_tokens, prompt_chars, extra_used)
            .map_err(|e| composition_invalid(&e))?;
        let id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        db.execute(
            "INSERT INTO stage_invocation_reservations(id,budget_account_id,execution_id,unit_id,logical_unit_key,idempotency_key,pool,reserved_output_tokens,reserved_prompt_chars,state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'reserved',?10,?10)",
            params![
                id,
                account_id,
                execution_id,
                unit.id,
                unit.logical_unit_key,
                format!("{execution_id}:{}:{}:{ordinal}", unit.id, unit.generation),
                pool.as_str(),
                output_tokens,
                prompt_chars,
                now
            ],
        )?;
        Ok(ReservationRow { id })
    }

    /// Releases a reservation proven to have made no external request.
    pub(crate) fn release_reservation(
        db: &Connection,
        reservation_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        db.execute(
            "UPDATE stage_invocation_reservations SET state='released',updated_at=?2 WHERE id=?1 AND state='reserved'",
            params![reservation_id, Self::now_iso()],
        )?;
        Ok(())
    }

    // --------------------------------------------------------------- executions

    /// Freezes the plan durably: execution, units, dependencies, ownership.
    pub(crate) fn admit_execution(
        db: &Connection,
        pulsation_id: &str,
        job_id: &str,
        plan: &StageExecutionPlanV1,
        budget_account_id: &str,
        policy_snapshot_id: &str,
        predecessor_execution_id: Option<&str>,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        let plan_json = serde_json::to_string(plan)
            .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
        db.execute(
            "INSERT INTO stage_executions(id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,recipe_version,policy_snapshot_id,plan_json,plan_sha256,status,execution_version,predecessor_execution_id,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'planned',1,?13,?14,?14)",
            params![
                plan.stage_execution_id.as_str(),
                pulsation_id,
                job_id,
                plan.parent_fence.generation,
                plan.parent_fence.input_revision,
                plan.parent_fence.state_version,
                plan.stage.to_pulsation().as_db_value(),
                budget_account_id,
                plan.recipe_version,
                policy_snapshot_id,
                plan_json,
                plan.plan_sha256.as_str(),
                predecessor_execution_id,
                now
            ],
        )?;
        for (ordinal, unit) in plan.units.iter().enumerate() {
            let spec_json = serde_json::to_string(unit)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let spec_sha =
                comp::canonical_sha256(unit, &[]).map_err(|e| composition_invalid(&e))?;
            db.execute(
                "INSERT INTO stage_work_units(id,execution_id,logical_unit_key,ordinal,executor_kind,spec_json,spec_sha256,generation,state_version,state,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,1,1,'pending',?8,?8)",
                params![
                    unit.unit_id.as_str(),
                    plan.stage_execution_id.as_str(),
                    unit.logical_unit_key.as_str(),
                    ordinal as i64,
                    unit.executor_kind.as_str(),
                    spec_json,
                    spec_sha.as_str(),
                    now
                ],
            )?;
        }
        for unit in &plan.units {
            for dependency in &unit.dependencies {
                db.execute(
                    "INSERT INTO stage_unit_dependencies(execution_id,predecessor_unit_id,successor_unit_id) VALUES(?1,?2,?3)",
                    params![plan.stage_execution_id.as_str(), dependency.as_str(), unit.unit_id.as_str()],
                )?;
            }
            for owned in &unit.owned_destination_refs {
                let Some(path) = plan
                    .references
                    .iter()
                    .find(|entry| entry.handle() == owned)
                    .and_then(comp::ReferenceEntryV1::relative_path)
                else {
                    continue;
                };
                db.execute(
                    "INSERT INTO stage_unit_ownership(execution_id,canonical_destination,relative_path,owner_unit_id) VALUES(?1,?2,?3,?4)",
                    params![plan.stage_execution_id.as_str(), path.canonical_key(), path.as_str(), unit.unit_id.as_str()],
                )?;
            }
        }
        Ok(())
    }

    fn map_execution(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionRow> {
        let stage: String = row.get(6)?;
        let plan_json: String = row.get(9)?;
        let status: String = row.get(10)?;
        Ok(ExecutionRow {
            id: row.get(0)?,
            pulsation_id: row.get(1)?,
            parent_generation: row.get(3)?,
            parent_state_version: row.get(5)?,
            stage: PulsationStage::from_db_value(&stage)
                .ok_or_else(|| Self::invalid_enum_error("stage", &stage))?,
            budget_account_id: row.get(7)?,
            plan: serde_json::from_str(&plan_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(e))
            })?,
            status: parse_execution_status(&status)
                .ok_or_else(|| Self::invalid_enum_error("status", &status))?,
            execution_version: row.get(11)?,
            blocked_reason: row.get(13)?,
        })
    }

    const EXECUTION_COLUMNS: &'static str = "id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,policy_snapshot_id,plan_json,plan_sha256,status,execution_version,blocked_code,blocked_reason,predecessor_execution_id";

    pub(crate) fn find_execution_for_job(
        db: &Connection,
        job_id: &str,
        generation: u64,
    ) -> Result<Option<ExecutionRow>, PersistenceServiceError> {
        Ok(db
            .query_row(
                "SELECT id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,policy_snapshot_id,plan_json,status,execution_version,blocked_code,blocked_reason,predecessor_execution_id FROM stage_executions WHERE job_id=?1 AND parent_generation=?2",
                params![job_id, generation],
                Self::map_execution,
            )
            .optional()?)
    }

    /// The most recent non-terminal, non-accepted execution of this stage for
    /// the same input revision (a parked or interrupted predecessor).
    pub(crate) fn find_reusable_predecessor(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
        input_revision: u64,
    ) -> Result<Option<ExecutionRow>, PersistenceServiceError> {
        Ok(db
            .query_row(
                "SELECT id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,policy_snapshot_id,plan_json,status,execution_version,blocked_code,blocked_reason,predecessor_execution_id FROM stage_executions WHERE pulsation_id=?1 AND stage=?2 AND parent_input_revision=?3 AND status IN ('planned','running','validating','ready','parked') ORDER BY created_at DESC LIMIT 1",
                params![pulsation_id, stage.as_db_value(), input_revision],
                Self::map_execution,
            )
            .optional()?)
    }

    pub(crate) fn find_execution(
        db: &Connection,
        execution_id: &str,
    ) -> Result<Option<ExecutionRow>, PersistenceServiceError> {
        let _ = Self::EXECUTION_COLUMNS;
        Ok(db
            .query_row(
                "SELECT id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,policy_snapshot_id,plan_json,status,execution_version,blocked_code,blocked_reason,predecessor_execution_id FROM stage_executions WHERE id=?1",
                [execution_id],
                Self::map_execution,
            )
            .optional()?)
    }

    pub(crate) fn latest_execution_for_pulsation(
        db: &Connection,
        pulsation_id: &str,
        stage: Option<PulsationStage>,
    ) -> Result<Option<ExecutionRow>, PersistenceServiceError> {
        Ok(db
            .query_row(
                "SELECT id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,policy_snapshot_id,plan_json,status,execution_version,blocked_code,blocked_reason,predecessor_execution_id FROM stage_executions WHERE pulsation_id=?1 AND (?2 IS NULL OR stage=?2) ORDER BY created_at DESC LIMIT 1",
                params![pulsation_id, stage.map(PulsationStage::as_db_value)],
                Self::map_execution,
            )
            .optional()?)
    }

    /// The accepted execution of `stage` for one input revision. Request
    /// revision keeps the input revision, so several executions of a stage
    /// may share it; superseded ones never count (ADR-032).
    pub(crate) fn accepted_execution_for(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
        input_revision: u64,
    ) -> Result<Option<ExecutionRow>, PersistenceServiceError> {
        Ok(db
            .query_row(
                "SELECT id,pulsation_id,job_id,parent_generation,parent_input_revision,parent_state_version,stage,budget_account_id,policy_snapshot_id,plan_json,status,execution_version,blocked_code,blocked_reason,predecessor_execution_id FROM stage_executions WHERE pulsation_id=?1 AND stage=?2 AND parent_input_revision=?3 AND status='accepted' ORDER BY created_at DESC,rowid DESC LIMIT 1",
                params![pulsation_id, stage.as_db_value(), input_revision],
                Self::map_execution,
            )
            .optional()?)
    }

    /// The pre-pulsation base of every path this pulsation published, keyed
    /// by the ASCII-lowercased path (ADR-032).
    pub(crate) fn pulsation_file_bases(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<HashMap<String, BaseFact>, PersistenceServiceError> {
        let mut statement = db.prepare(
            "SELECT path_key,state,content_sha256,reason,content FROM pulsation_file_bases WHERE pulsation_id=?1",
        )?;
        let rows = statement
            .query_map([pulsation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(key, state, sha256, reason, content)| {
                let state = match state.as_str() {
                    "absent" => BaseState::Absent,
                    "present" if content.is_some() => BaseState::Present,
                    "present" => {
                        return (
                            key,
                            BaseFact {
                                state: BaseState::PresentUnstored,
                                sha256,
                                reason: Some("purged".into()),
                                content: None,
                            },
                        );
                    }
                    _ => BaseState::PresentUnstored,
                };
                (
                    key,
                    BaseFact {
                        state,
                        sha256,
                        reason,
                        content,
                    },
                )
            })
            .collect())
    }

    /// Paths this pulsation published in an earlier round whose artifact is
    /// no longer accepted or candidate in `input_revision`.
    pub(crate) fn orphaned_publications(
        db: &Connection,
        pulsation_id: &str,
        input_revision: u64,
    ) -> Result<Vec<String>, PersistenceServiceError> {
        let mut statement = db.prepare(
            "SELECT b.relative_path FROM pulsation_file_bases b WHERE b.pulsation_id=?1 AND NOT EXISTS(SELECT 1 FROM artifacts a WHERE a.pulsation_id=b.pulsation_id AND lower(a.relative_path)=b.path_key AND a.input_revision=?2 AND a.acceptance IN ('accepted','candidate')) ORDER BY b.path_key",
        )?;
        let rows = statement
            .query_map(params![pulsation_id, input_revision], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records the Integrator's classified inputs with its execution.
    pub(crate) fn record_review_inputs(
        db: &Connection,
        execution_id: &str,
        pulsation_id: &str,
        input_revision: u64,
        review: &ReviewBoundary,
    ) -> Result<(), PersistenceServiceError> {
        let json = serde_json::to_string(&review.manifest)
            .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
        db.execute(
            "INSERT INTO integrator_review_inputs(stage_execution_id,pulsation_id,input_revision,manifest_json,manifest_sha256,review_policy_state,review_policy_sha256,gap_count,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                execution_id,
                pulsation_id,
                input_revision,
                json,
                review.manifest_sha256,
                review.manifest.policy.state.as_str(),
                review.manifest.policy.sha256(),
                review.manifest.gap_count() as i64,
                Self::now_iso(),
            ],
        )?;
        Ok(())
    }

    /// The classified inputs recorded for an Integrator execution, if any.
    pub(crate) fn review_inputs_of(
        db: &Connection,
        execution_id: &str,
    ) -> Result<Option<ReviewInputManifestV1>, PersistenceServiceError> {
        db.query_row(
            "SELECT manifest_json FROM integrator_review_inputs WHERE stage_execution_id=?1",
            [execution_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| {
            serde_json::from_str(&json).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
        })
        .transpose()
    }

    /// Compare-and-swap status transition; bumps `execution_version`.
    pub(crate) fn transition_execution(
        db: &Connection,
        execution_id: &str,
        expected: &[ExecutionStatus],
        next: ExecutionStatus,
        blocked: Option<(&str, &str)>,
    ) -> Result<(), PersistenceServiceError> {
        let blocked_reason = blocked.map(|(_, reason)| protected_diagnostic(reason));
        let expected = expected
            .iter()
            .map(|status| format!("'{}'", execution_status_str(*status)))
            .collect::<Vec<_>>()
            .join(",");
        let changed = db.execute(
            &format!(
                "UPDATE stage_executions SET status=?2,execution_version=execution_version+1,blocked_code=?3,blocked_reason=?4,updated_at=?5 WHERE id=?1 AND status IN ({expected})"
            ),
            params![
                execution_id,
                execution_status_str(next),
                blocked.map(|b| b.0),
                blocked_reason,
                Self::now_iso()
            ],
        )?;
        if changed != 1 {
            return Err(stale("stage execution status"));
        }
        Ok(())
    }

    pub(crate) fn record_acceptance_on_execution(
        db: &Connection,
        execution_id: &str,
        aggregate_sha256: &str,
        acceptance_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let changed = db.execute(
            "UPDATE stage_executions SET status='accepted',execution_version=execution_version+1,accepted_aggregate_sha256=?2,acceptance_id=?3,updated_at=?4 WHERE id=?1 AND status='ready' AND acceptance_id IS NULL",
            params![execution_id, aggregate_sha256, acceptance_id, Self::now_iso()],
        )?;
        if changed != 1 {
            return Err(stale("stage execution acceptance"));
        }
        Ok(())
    }

    // -------------------------------------------------------------------- units

    fn map_unit(row: &rusqlite::Row<'_>) -> rusqlite::Result<UnitRow> {
        let kind: String = row.get(4)?;
        let spec_json: String = row.get(5)?;
        let state: String = row.get(8)?;
        Ok(UnitRow {
            id: row.get(0)?,
            logical_unit_key: row.get(2)?,
            executor_kind: ExecutorKind::parse(&kind)
                .ok_or_else(|| Self::invalid_enum_error("executor_kind", &kind))?,
            spec: serde_json::from_str(&spec_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(e))
            })?,
            generation: row.get(6)?,
            state_version: row.get(7)?,
            state: parse_unit_state(&state)
                .ok_or_else(|| Self::invalid_enum_error("state", &state))?,
            validated_result_json: row.get(9)?,
            validated_result_sha256: row.get(10)?,
            reused_from_attempt_id: row.get(11)?,
            reuse_key_sha256: row.get(14)?,
        })
    }

    pub(crate) fn load_units(
        db: &Connection,
        execution_id: &str,
    ) -> Result<Vec<UnitRow>, PersistenceServiceError> {
        Ok(db
            .prepare(
                "SELECT id,execution_id,logical_unit_key,ordinal,executor_kind,spec_json,generation,state_version,state,validated_result_json,validated_result_sha256,reused_from_attempt_id,failure_code,failure_message,reuse_key_sha256 FROM stage_work_units WHERE execution_id=?1 ORDER BY ordinal",
            )?
            .query_map([execution_id], Self::map_unit)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Compare-and-swap unit state transition; bumps `state_version`.
    pub(crate) fn transition_unit(
        db: &Connection,
        unit: &UnitRow,
        next: UnitState,
        failure: Option<(&str, &str)>,
    ) -> Result<(), PersistenceServiceError> {
        let failure_message = failure.map(|(_, message)| protected_diagnostic(message));
        let changed = db.execute(
            "UPDATE stage_work_units SET state=?2,state_version=state_version+1,failure_code=?3,failure_message=?4,updated_at=?5 WHERE id=?1 AND generation=?6 AND state_version=?7",
            params![
                unit.id,
                unit_state_str(next),
                failure.map(|f| f.0),
                failure_message,
                Self::now_iso(),
                unit.generation,
                unit.state_version
            ],
        )?;
        if changed != 1 {
            return Err(stale("work unit state"));
        }
        Ok(())
    }

    /// Accepts a validated child result exactly once (SC-015).
    pub(crate) fn accept_child(
        db: &Connection,
        unit: &UnitRow,
        attempt_id: Option<&str>,
        result_json: &str,
        result_sha256: &str,
        reuse_key_sha256: &str,
        reused_from_attempt_id: Option<&str>,
    ) -> Result<(), PersistenceServiceError> {
        let changed = db.execute(
            "UPDATE stage_work_units SET state='validated',state_version=state_version+1,validated_result_json=?2,validated_result_sha256=?3,reuse_key_sha256=?4,reused_from_attempt_id=?5,failure_code=NULL,failure_message=NULL,updated_at=?6 WHERE id=?1 AND generation=?7 AND state_version=?8 AND state IN ('running','pending')",
            params![
                unit.id,
                result_json,
                result_sha256,
                reuse_key_sha256,
                reused_from_attempt_id,
                Self::now_iso(),
                unit.generation,
                unit.state_version
            ],
        )?;
        if changed != 1 {
            return Err(stale("work unit acceptance"));
        }
        let _ = attempt_id;
        Ok(())
    }

    /// Opens a new unit generation for an admitted correction.
    pub(crate) fn next_unit_generation(
        db: &Connection,
        unit: &UnitRow,
    ) -> Result<UnitRow, PersistenceServiceError> {
        let changed = db.execute(
            "UPDATE stage_work_units SET generation=generation+1,state='pending',state_version=state_version+1,updated_at=?2 WHERE id=?1 AND generation=?3 AND state_version=?4 AND state='failed'",
            params![unit.id, Self::now_iso(), unit.generation, unit.state_version],
        )?;
        if changed != 1 {
            return Err(stale("work unit generation"));
        }
        let mut next = unit.clone();
        next.generation += 1;
        next.state_version += 1;
        next.state = UnitState::Pending;
        Ok(next)
    }

    /// Reopens a validated unit for a host-check repair: a new generation in
    /// `pending`, its previous result kept in history through the attempts.
    pub(crate) fn reopen_unit_for_repair(
        db: &Connection,
        unit: &UnitRow,
    ) -> Result<UnitRow, PersistenceServiceError> {
        let changed = db.execute(
            "UPDATE stage_work_units SET generation=generation+1,state='pending',state_version=state_version+1,validated_result_json=NULL,validated_result_sha256=NULL,reuse_key_sha256=NULL,reused_from_attempt_id=NULL,updated_at=?2 WHERE id=?1 AND generation=?3 AND state_version=?4 AND state='validated'",
            params![unit.id, Self::now_iso(), unit.generation, unit.state_version],
        )?;
        if changed != 1 {
            return Err(stale("work unit repair"));
        }
        let mut next = unit.clone();
        next.generation += 1;
        next.state_version += 1;
        next.state = UnitState::Pending;
        next.validated_result_json = None;
        next.validated_result_sha256 = None;
        next.reuse_key_sha256 = None;
        next.reused_from_attempt_id = None;
        Ok(next)
    }

    // ----------------------------------------------------------------- attempts

    /// Creates the attempt and the real `agent_runs` row it reports into.
    pub(crate) fn create_attempt(
        db: &Connection,
        job_id: &str,
        execution_id: &str,
        unit: &UnitRow,
        ordinal: u64,
        reservation_id: &str,
        executor_kind: ExecutorKind,
        profile_snapshot_id: &str,
        profile: &EffectiveProfile,
    ) -> Result<AttemptRow, PersistenceServiceError> {
        let now = Self::now_iso();
        let agent_run_id = Uuid::new_v4().to_string();
        let provider = match (profile.transport.as_str(), &profile.backend) {
            ("cli", Some(backend)) => format!("cli:{backend}"),
            ("cli", None) => "cli".to_owned(),
            _ => "api".to_owned(),
        };
        db.execute(
            "INSERT INTO agent_runs(id,job_id,agent_name,status,input_tokens,output_tokens,provider,model,estimated_cost_usd,created_at,updated_at) VALUES(?1,?2,?3,'running',NULL,NULL,?4,?5,NULL,?6,?6)",
            params![agent_run_id, job_id, executor_kind.as_str(), provider, profile.requested_model, now],
        )?;
        let id = Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO stage_work_unit_attempts(id,unit_id,execution_id,unit_generation,ordinal,reservation_id,agent_run_id,executor_kind,profile_snapshot_id,effective_model,state,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'reserved',?11,?11)",
            params![
                id,
                unit.id,
                execution_id,
                unit.generation,
                ordinal as i64,
                reservation_id,
                agent_run_id,
                executor_kind.as_str(),
                profile_snapshot_id,
                profile.requested_model,
                now
            ],
        )?;
        Ok(AttemptRow {
            id,
            reservation_id: reservation_id.to_owned(),
            agent_run_id,
        })
    }

    /// Durable "started" marker written immediately before external dispatch.
    /// From here on the reservation is consumed even if the process crashes.
    pub(crate) fn mark_attempt_started(
        db: &Connection,
        attempt: &AttemptRow,
        llm_execution_id: &str,
        dispatch_sha256: &str,
        input_sha256: &str,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        db.execute(
            "UPDATE stage_work_unit_attempts SET state='running',llm_execution_id=?2,dispatch_sha256=?3,input_sha256=?4,started_at=?5,updated_at=?5 WHERE id=?1 AND state='reserved'",
            params![attempt.id, llm_execution_id, dispatch_sha256, input_sha256, now],
        )?;
        db.execute(
            "UPDATE stage_invocation_reservations SET state='started',llm_execution_id=?2,updated_at=?3 WHERE id=?1 AND state='reserved'",
            params![attempt.reservation_id, llm_execution_id, now],
        )?;
        Ok(())
    }

    pub(crate) fn finish_attempt(
        db: &Connection,
        attempt: &AttemptRow,
        state: AttemptState,
        error_code: Option<&str>,
        diagnostic: Option<&str>,
        observed_model: Option<&str>,
        result_sha256: Option<&str>,
    ) -> Result<(), PersistenceServiceError> {
        let diagnostic = diagnostic.map(protected_diagnostic);
        let observed_model = observed_model.map(protected_diagnostic);
        let now = Self::now_iso();
        db.execute(
            "UPDATE stage_work_unit_attempts SET state=?2,error_code=?3,diagnostic=?4,observed_model=COALESCE(?5,observed_model),result_sha256=?6,finished_at=?7,updated_at=?7 WHERE id=?1 AND state IN ('reserved','running')",
            params![attempt.id, attempt_state_str(state), error_code, diagnostic, observed_model, result_sha256, now],
        )?;
        db.execute(
            "UPDATE stage_invocation_reservations SET state='settled',updated_at=?2 WHERE id=?1 AND state IN ('reserved','started')",
            params![attempt.reservation_id, now],
        )?;
        let run_status = match state {
            AttemptState::Succeeded => "done",
            AttemptState::Rejected => "failed",
            AttemptState::Interrupted | AttemptState::Cancelled => "interrupted",
            AttemptState::Reserved | AttemptState::Running => "running",
        };
        db.execute(
            "UPDATE agent_runs SET status=?2,model=COALESCE(?3,model),updated_at=?4 WHERE id=?1",
            params![attempt.agent_run_id, run_status, observed_model, now],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- snapshot

    /// Read-only observation of the latest execution of a pulsation.
    pub fn get_stage_execution_snapshot(
        &self,
        pulsation_id: &str,
        stage: Option<PulsationStage>,
    ) -> Result<Option<StageExecutionSnapshot>, PersistenceServiceError> {
        let pulsation_id = Self::normalize_pulsation_id(pulsation_id)?;
        let db = self.connection()?;
        let Some(execution) = Self::latest_execution_for_pulsation(&db, &pulsation_id, stage)?
        else {
            return Ok(None);
        };
        Self::execution_snapshot(&db, &execution).map(Some)
    }

    pub(crate) fn execution_snapshot(
        db: &Connection,
        execution: &ExecutionRow,
    ) -> Result<StageExecutionSnapshot, PersistenceServiceError> {
        let project_id: String = db.query_row(
            "SELECT project_id FROM pulsations WHERE id=?1",
            [&execution.pulsation_id],
            |r| r.get(0),
        )?;
        let mut units = Vec::new();
        for unit in Self::load_units(db, &execution.id)? {
            let attempts = db
                .prepare(
                    "SELECT id,state,executor_kind,COALESCE(observed_model,effective_model,'unknown'),profile_snapshot_id,error_code FROM stage_work_unit_attempts WHERE unit_id=?1 ORDER BY created_at DESC LIMIT 2",
                )?
                .query_map([&unit.id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut summaries = Vec::new();
            for (id, state, kind, model, profile, error) in attempts {
                let totals = crate::usage::attempt_totals(db, &id)?;
                summaries.push(AttemptSummaryV1 {
                    attempt_id: comp::Uuid::new(id.clone()).map_err(|e| composition_invalid(&e))?,
                    state: parse_attempt_state(&state).unwrap_or(AttemptState::Interrupted),
                    executor_kind: ExecutorKind::parse(&kind).unwrap_or(unit.executor_kind),
                    effective_model: if model.trim().is_empty() {
                        "unknown".into()
                    } else {
                        protected_diagnostic(&model).chars().take(256).collect()
                    },
                    profile_snapshot_id: comp::Uuid::new(profile)
                        .map_err(|e| composition_invalid(&e))?,
                    error_code: error
                        .and_then(|code| comp::Handle::new(code.replace('-', "_")).ok()),
                    usage: usage_summary(&totals),
                });
            }
            units.push(UnitSnapshotV1 {
                unit_id: comp::Uuid::new(unit.id.clone()).map_err(|e| composition_invalid(&e))?,
                logical_unit_key: comp::Handle::new(unit.logical_unit_key.clone())
                    .map_err(|e| composition_invalid(&e))?,
                executor_kind: unit.executor_kind,
                generation: unit.generation as u32,
                state_version: unit.state_version as u32,
                state: unit.state,
                reused_from_attempt_id: unit
                    .reused_from_attempt_id
                    .as_deref()
                    .map(|id| comp::Uuid::new(id.to_owned()))
                    .transpose()
                    .map_err(|e| composition_invalid(&e))?,
                attempts: summaries,
            });
        }
        let ledger = Self::load_ledger(db, &execution.budget_account_id)?;
        let mut remaining = ledger.remaining();
        remaining.initial_invocations = remaining.initial_invocations.min(16);
        remaining.extra_invocations = remaining.extra_invocations.min(2);
        let checks = Self::execution_checks(db, execution)?;
        let validation_evidence = checks
            .iter()
            .filter_map(|check| check.receipt_id.as_ref())
            .map(|id| Self::validation_evidence_on(db, id.as_str()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let totals = crate::usage::stage_execution_totals(db, &execution.id)?;
        let snapshot = StageExecutionSnapshot {
            schema_version: SchemaV1,
            project_id: comp::Uuid::new(project_id).map_err(|e| composition_invalid(&e))?,
            pulsation_id: comp::Uuid::new(execution.pulsation_id.clone())
                .map_err(|e| composition_invalid(&e))?,
            stage_execution_id: comp::Uuid::new(execution.id.clone())
                .map_err(|e| composition_invalid(&e))?,
            stage: comp::Stage::from_pulsation(execution.stage),
            parent_generation: execution.parent_generation as u32,
            parent_state_version: execution.parent_state_version as u32,
            execution_version: execution.execution_version as u32,
            status: execution.status,
            units,
            blocked_reason: execution
                .blocked_reason
                .clone()
                .map(|r| protected_diagnostic(&r).chars().take(4_000).collect()),
            checks,
            validation_evidence,
            validation_receipts: Self::latest_validation_receipts(db, &execution.id)?,
            remaining_budget: RemainingBudgetV1 { ..remaining },
            usage: usage_summary(&totals),
        };
        snapshot.validate().map_err(|e| composition_invalid(&e))?;
        Ok(snapshot)
    }

    // ------------------------------------------------ specialty overrides

    /// The project's per-specialty overrides (ADR-022 §4 resolution order:
    /// recovery override > specialty override > stage role).
    pub fn list_specialty_overrides(
        &self,
        project_id: &str,
    ) -> Result<Vec<(ExecutorKind, SpecialtyOverride)>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let db = self.connection()?;
        let mut statement = db.prepare(
            "SELECT executor_kind,execution_mode,backend,model,thinking_level FROM composition_specialty_overrides WHERE project_id=?1 ORDER BY executor_kind",
        )?;
        let rows = statement
            .query_map([&project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(kind, mode, backend, model, thinking)| {
                Some((
                    ExecutorKind::parse(&kind)?,
                    SpecialtyOverride {
                        execution_mode: match mode.as_str() {
                            "cli_process" => AgentExecutionMode::CliProcess,
                            _ => AgentExecutionMode::ApiDirect,
                        },
                        backend: backend.as_deref().and_then(CliBackend::parse),
                        model: model.filter(|value| !value.trim().is_empty()),
                        thinking_level: ThinkingLevel::parse(&thinking)
                            .unwrap_or(ThinkingLevel::Default),
                    },
                ))
            })
            .collect())
    }

    /// Stores one override. A CLI override needs a backend; an API override
    /// carries none. Affects future admissions only (SC-014).
    pub fn upsert_specialty_override(
        &self,
        project_id: &str,
        executor_kind: ExecutorKind,
        setting: SpecialtyOverride,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let backend = match (setting.execution_mode, setting.backend) {
            (AgentExecutionMode::CliProcess, Some(backend)) => Some(backend.as_str().to_owned()),
            (AgentExecutionMode::CliProcess, None) => {
                return Err(PersistenceServiceError::Validation(
                    "Um override de especialidade em modo CLI precisa de um backend.".into(),
                ));
            }
            (AgentExecutionMode::ApiDirect, _) => None,
        };
        let db = self.connection()?;
        if Self::find_project_by_id(&db, &project_id, false)?.is_none() {
            return Err(PersistenceServiceError::NotFound(
                "Project not found to configure specialties.".into(),
            ));
        }
        if let (AgentExecutionMode::CliProcess, Some(cli)) =
            (setting.execution_mode, setting.backend)
        {
            // The specialty inherits its role's model and catalog profile.
            let role = crate::llm::AgentRole::for_stage(
                executor_kind.stage().to_pulsation().as_db_value(),
            );
            let (role_model, role_profile): (Option<String>, Option<String>) = db
                .query_row(
                    "SELECT model,model_profile FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2 AND backend=?3",
                    params![project_id, role.as_str(), cli.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or_default();
            let model = setting
                .model
                .as_deref()
                .filter(|m| !m.trim().is_empty())
                .or(role_model.as_deref());
            crate::terminals::validate_catalog_effort(
                &db,
                &project_id,
                cli,
                model,
                role_profile.as_deref(),
                setting.thinking_level,
            )
            .map_err(|error| PersistenceServiceError::Validation(error.to_string()))?;
        }
        db.execute(
            "INSERT INTO composition_specialty_overrides(project_id,executor_kind,execution_mode,backend,model,thinking_level,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(project_id,executor_kind) DO UPDATE SET execution_mode=excluded.execution_mode,backend=excluded.backend,model=excluded.model,thinking_level=excluded.thinking_level,updated_at=excluded.updated_at",
            params![
                project_id,
                executor_kind.as_str(),
                setting.execution_mode.as_str(),
                backend,
                setting.model.filter(|m| !m.trim().is_empty()),
                setting.thinking_level.as_str(),
                Self::now_iso()
            ],
        )?;
        Ok(())
    }

    pub fn delete_specialty_override(
        &self,
        project_id: &str,
        executor_kind: ExecutorKind,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        self.connection()?.execute(
            "DELETE FROM composition_specialty_overrides WHERE project_id=?1 AND executor_kind=?2",
            params![project_id, executor_kind.as_str()],
        )?;
        Ok(())
    }

    // --------------------------------------------------- recovery commands

    /// Applies a typed recovery command (validation contract §7): the
    /// expected versions must match the execution and its parent exactly,
    /// the request is idempotent by `requestId`, and the decision goes through
    /// the same failure resolution the legacy checkpoint uses. Returns the
    /// execution snapshot after the decision.
    pub fn apply_stage_recovery(
        &self,
        command: StageRecoveryCommandV1,
    ) -> Result<StageExecutionSnapshot, PersistenceServiceError> {
        command.validate().map_err(|e| composition_invalid(&e))?;
        let (request_id, execution_id, pulsation_id, project_id, action) = match &command {
            StageRecoveryCommandV1::Retry {
                request_id,
                stage_execution_id,
                pulsation_id,
                project_id,
                ..
            } => (
                request_id,
                stage_execution_id,
                pulsation_id,
                project_id,
                "retry",
            ),
            StageRecoveryCommandV1::Revise {
                request_id,
                stage_execution_id,
                pulsation_id,
                project_id,
                ..
            } => (
                request_id,
                stage_execution_id,
                pulsation_id,
                project_id,
                "revise",
            ),
            StageRecoveryCommandV1::Abort {
                request_id,
                stage_execution_id,
                pulsation_id,
                project_id,
                ..
            } => (
                request_id,
                stage_execution_id,
                pulsation_id,
                project_id,
                "abort",
            ),
        };
        let db = self.connection()?;
        if let Some(previous) = db
            .query_row(
                "SELECT result_json FROM stage_recovery_requests WHERE request_id=?1",
                [request_id.as_str()],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            return serde_json::from_str(&previous)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()));
        }
        let execution = Self::find_execution(&db, execution_id.as_str())?
            .ok_or_else(|| PersistenceServiceError::NotFound("Composed run not found.".into()))?;
        if execution.pulsation_id != pulsation_id.as_str() {
            return Err(PersistenceServiceError::Validation(
                "SC-021: the run does not belong to this pulsation.".into(),
            ));
        }
        let (current_project, state_version): (String, u64) = db.query_row(
            "SELECT project_id,state_version FROM pulsations WHERE id=?1",
            [&execution.pulsation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if current_project != project_id.as_str() {
            return Err(PersistenceServiceError::Validation(
                "SC-021: the pulsation does not belong to this project.".into(),
            ));
        }
        let (generation, parent_state, version) = command.versions();
        if u64::from(generation) != execution.parent_generation
            || u64::from(version) != execution.execution_version
            || u64::from(parent_state) != state_version
        {
            return Err(PersistenceServiceError::Validation(
                "SC-021: the decision was made on a stale snapshot; reload the run.".into(),
            ));
        }
        let checkpoint_id: String = db
            .query_row(
                "SELECT f.checkpoint_id FROM stage_failure_requests f JOIN checkpoints c ON c.id=f.checkpoint_id WHERE f.stage_execution_id=?1 AND f.decision IS NULL AND c.status='waiting'",
                [&execution.id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                PersistenceServiceError::Validation(
                    "SC-021: this run is not parked on a failure checkpoint.".into(),
                )
            })?;
        drop(db);
        let (decision, feedback, target) = match &command {
            StageRecoveryCommandV1::Retry { .. } => (StageFailureDecision::RetryStage, None, None),
            StageRecoveryCommandV1::Revise {
                target_stage,
                reason,
                ..
            } => (
                StageFailureDecision::RequestRevision,
                Some(reason.clone()),
                Some(target_stage.to_pulsation()),
            ),
            StageRecoveryCommandV1::Abort { .. } => (StageFailureDecision::Abort, None, None),
        };
        self.resolve_stage_failure_with_target(
            ResolveStageFailureRequest {
                checkpoint_id,
                expected_state_version: state_version,
                decision,
                feedback,
            },
            target,
        )?;
        let snapshot = self
            .get_stage_execution_snapshot(&execution.pulsation_id, Some(execution.stage))?
            .ok_or_else(|| PersistenceServiceError::Runtime("Run snapshot unavailable.".into()))?;
        self.connection()?.execute(
            "INSERT INTO stage_recovery_requests(request_id,stage_execution_id,action,result_json,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![
                request_id.as_str(),
                execution.id,
                action,
                serde_json::to_string(&snapshot)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
                Self::now_iso()
            ],
        )?;
        Ok(snapshot)
    }

    // ------------------------------------------------- validation profiles

    /// The project's configured validation profiles (developer-owned) with
    /// their enabled flag, by check handle.
    pub(crate) fn validation_profiles_for(
        db: &Connection,
        project_id: &str,
    ) -> Result<Vec<(ValidationCommandProfileV1, bool)>, PersistenceServiceError> {
        let mut statement = db.prepare(
            "SELECT profile_json,enabled FROM project_validation_profiles WHERE project_id=?1 ORDER BY check_ref",
        )?;
        let rows = statement
            .query_map([project_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut profiles = Vec::with_capacity(rows.len());
        for (json, enabled) in rows {
            let profile: ValidationCommandProfileV1 = serde_json::from_str(&json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            profiles.push((profile, enabled));
        }
        Ok(profiles)
    }

    pub fn list_validation_profiles(
        &self,
        project_id: &str,
    ) -> Result<Vec<(ValidationCommandProfileV1, bool)>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        Self::validation_profiles_for(&self.connection()?, &project_id)
    }

    /// Presets detected from the project's manifests; never persisted or
    /// enabled implicitly - the developer chooses which to keep.
    pub fn detect_validation_presets(
        &self,
        project_id: &str,
    ) -> Result<Vec<ValidationCommandProfileV1>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        let db = self.connection()?;
        let project = Self::find_project_by_id(&db, &project_id, false)?
            .ok_or_else(|| PersistenceServiceError::NotFound("Project not found.".into()))?;
        Ok(detect_presets(Path::new(&project.workspace_path)))
    }

    /// Stores or replaces one profile. A changed profile bumps its version so
    /// receipts bound to the old digest are never reused for the new one.
    pub fn upsert_validation_profile(
        &self,
        project_id: &str,
        mut profile: ValidationCommandProfileV1,
        enabled: bool,
    ) -> Result<ValidationCommandProfileV1, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        profile.validate().map_err(|e| composition_invalid(&e))?;
        if let Some(path) = profile.resolved_wrapper_hint() {
            return Err(PersistenceServiceError::Validation(format!(
                "`{path}` is a shell wrapper; a validation profile needs a direct executable."
            )));
        }
        let db = self.connection()?;
        if Self::find_project_by_id(&db, &project_id, false)?.is_none() {
            return Err(PersistenceServiceError::NotFound(
                "Project not found to configure validation.".into(),
            ));
        }
        let previous: Option<(String, u32)> = db
            .query_row(
                "SELECT profile_json,profile_version FROM project_validation_profiles WHERE project_id=?1 AND check_ref=?2",
                params![project_id, profile.check_ref.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((json, version)) = previous {
            let old: ValidationCommandProfileV1 = serde_json::from_str(&json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let mut comparable = profile.clone();
            comparable.profile_version = old.profile_version;
            profile.profile_version = if comparable == old {
                version
            } else {
                version + 1
            };
        }
        let now = Self::now_iso();
        db.execute(
            "INSERT INTO project_validation_profiles(project_id,check_ref,profile_version,enabled,profile_json,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?6)
             ON CONFLICT(project_id,check_ref) DO UPDATE SET profile_version=excluded.profile_version,enabled=excluded.enabled,profile_json=excluded.profile_json,updated_at=excluded.updated_at",
            params![
                project_id,
                profile.check_ref.as_str(),
                profile.profile_version,
                enabled,
                serde_json::to_string(&profile).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
                now
            ],
        )?;
        Ok(profile)
    }

    pub fn delete_validation_profile(
        &self,
        project_id: &str,
        check_ref: &str,
    ) -> Result<(), PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        self.connection()?.execute(
            "DELETE FROM project_validation_profiles WHERE project_id=?1 AND check_ref=?2",
            params![project_id, check_ref],
        )?;
        Ok(())
    }

    // ------------------------------------------------------- validation runs

    /// Persists a receipt as an immutable run row and returns its id.
    pub(crate) fn insert_validation_run(
        db: &Connection,
        receipt: &ValidationReceiptV1,
    ) -> Result<(), PersistenceServiceError> {
        let ordinal: u64 = db.query_row(
            "SELECT COALESCE(MAX(invocation_ordinal),0)+1 FROM host_validation_runs WHERE execution_id=?1 AND check_ref=?2 AND candidate_sha256=?3 AND profile_sha256=?4",
            params![
                receipt.stage_execution_id.as_str(),
                receipt.check_ref.as_str(),
                receipt.candidate_sha256.as_str(),
                receipt.profile_sha256.as_str()
            ],
            |r| r.get(0),
        )?;
        db.execute(
            "INSERT INTO host_validation_runs(id,execution_id,parent_generation,check_ref,profile_snapshot_id,profile_sha256,candidate_sha256,input_manifest_sha256,toolchain_sha256,environment_sha256,invocation_ordinal,status,exit_code,started_at,finished_at,receipt_json,baseline_receipt_id,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                receipt.receipt_id.as_str(),
                receipt.stage_execution_id.as_str(),
                receipt.parent_generation,
                receipt.check_ref.as_str(),
                receipt.profile_snapshot_id.as_str(),
                receipt.profile_sha256.as_str(),
                receipt.candidate_sha256.as_str(),
                receipt.input_manifest_sha256.as_str(),
                receipt.toolchain_sha256.as_str(),
                receipt.environment_sha256.as_str(),
                ordinal,
                receipt_status_str(receipt.status),
                receipt.exit_code,
                receipt.started_at.as_ref().map(|t| t.as_str().to_owned()),
                receipt.finished_at.as_ref().map(|t| t.as_str().to_owned()),
                serde_json::to_string(receipt).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
                receipt.baseline_receipt_id.as_ref().map(|u| u.as_str().to_owned()),
                Self::now_iso(),
            ],
        )?;
        Ok(())
    }

    /// A receipt already bound to exactly this check, exact input set and
    /// profile (§2 reuse rule), newest first. Baseline runs never match.
    pub(crate) fn find_validation_receipt(
        db: &Connection,
        execution_id: &str,
        check_ref: &str,
        input_manifest_sha256: &str,
        profile_sha256: &str,
    ) -> Result<Option<ValidationReceiptV1>, PersistenceServiceError> {
        let json: Option<String> = db
            .query_row(
                "SELECT receipt_json FROM host_validation_runs WHERE execution_id=?1 AND check_ref=?2 AND input_manifest_sha256=?3 AND profile_sha256=?4 AND baseline_receipt_id IS NULL ORDER BY created_at DESC,invocation_ordinal DESC LIMIT 1",
                params![execution_id, check_ref, input_manifest_sha256, profile_sha256],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|json| {
            serde_json::from_str(&json).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
        })
        .transpose()
    }

    /// Every check the execution's plan requires, with the status of its
    /// latest host receipt (`not_run` without one). Snapshots and the review
    /// brief read the same summary.
    pub(crate) fn execution_checks(
        db: &Connection,
        execution: &ExecutionRow,
    ) -> Result<Vec<CheckSummaryV1>, PersistenceServiceError> {
        let receipts = Self::latest_validation_receipts(db, &execution.id)?;
        Ok(execution
            .plan
            .check_refs
            .iter()
            .map(|check| {
                let receipt = receipts.iter().find(|r| &r.check_ref == check);
                CheckSummaryV1 {
                    check_ref: check.clone(),
                    required: true,
                    receipt_id: receipt.map(|r| r.receipt_id.clone()),
                    status: receipt
                        .map(|r| check_status_of(r.status))
                        .unwrap_or(comp::CheckStatus::NotRun),
                }
            })
            .collect())
    }

    /// The latest receipt per check of an execution, for reports and snapshots.
    pub(crate) fn latest_validation_receipts(
        db: &Connection,
        execution_id: &str,
    ) -> Result<Vec<ValidationReceiptV1>, PersistenceServiceError> {
        let mut statement = db.prepare(
            "SELECT receipt_json FROM host_validation_runs r WHERE execution_id=?1 AND created_at=(SELECT MAX(created_at) FROM host_validation_runs x WHERE x.execution_id=r.execution_id AND x.check_ref=r.check_ref AND x.baseline_receipt_id IS r.baseline_receipt_id) AND NOT EXISTS(SELECT 1 FROM host_validation_runs b WHERE b.baseline_receipt_id=r.id) ORDER BY check_ref",
        )?;
        let rows = statement
            .query_map([execution_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
            })
            .collect()
    }

    // ------------------------------------------------------ revision/abort

    /// Human Revision from `from_stage` onward: every execution of those
    /// stages that is not already terminal by failure - including the
    /// accepted ones whose artifacts the revision supersedes - becomes
    /// `superseded`, and their budget accounts are closed so the re-admitted
    /// stage opens a fresh account with visible prior spend.
    pub(crate) fn supersede_stage_executions(
        db: &Connection,
        pulsation_id: &str,
        from_stage: PulsationStage,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        for (stage, _, _) in crate::orchestration::catalog::STAGES {
            if stage < from_stage {
                continue;
            }
            db.execute(
                "UPDATE stage_executions SET status='superseded',execution_version=execution_version+1,updated_at=?3 WHERE pulsation_id=?1 AND stage=?2 AND status NOT IN ('failed','cancelled','superseded')",
                params![pulsation_id, stage.as_db_value(), now],
            )?;
            Self::close_budget_accounts(db, pulsation_id, stage)?;
        }
        Ok(())
    }

    /// Abort/cancel: every non-terminal execution of the pulsation is cancelled.
    pub(crate) fn cancel_stage_executions(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        db.execute(
            "UPDATE stage_executions SET status='cancelled',execution_version=execution_version+1,updated_at=?2 WHERE pulsation_id=?1 AND status NOT IN ('accepted','failed','cancelled','superseded')",
            params![pulsation_id, now],
        )?;
        db.execute(
            "UPDATE stage_work_units SET state='cancelled',state_version=state_version+1,updated_at=?2 WHERE state IN ('pending','running') AND execution_id IN (SELECT id FROM stage_executions WHERE pulsation_id=?1)",
            params![pulsation_id, now],
        )?;
        Ok(())
    }

    /// Records a `stage_failure` checkpoint for a composed execution, carrying
    /// the execution, the permitted revision targets and the remaining shared
    /// allowance. Same preconditions and no-checkpoint fallback as the legacy
    /// `record_stage_failure_checkpoint`.
    pub(crate) fn record_stage_failure_checkpoint_v4(
        transaction: &rusqlite::Transaction<'_>,
        current: &Pulsation,
        stage: PulsationStage,
        job_id: &str,
        agent_run_id: Option<&str>,
        offered: &crate::orchestration::OfferedDecisionsV4,
        error_code: &str,
        diagnostic: &str,
        stage_execution_id: &str,
        remaining: &RemainingBudgetV1,
    ) -> Result<bool, PersistenceServiceError> {
        let diagnostic = protected_diagnostic(diagnostic);
        if !matches!(
            current.status,
            PulsationStatus::Running | PulsationStatus::Planning
        ) || current.stage != stage
        {
            return Ok(false);
        }
        let waiting: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE pulsation_id=?1 AND status='waiting'",
            [&current.id],
            |row| row.get(0),
        )?;
        if waiting != 0 {
            return Ok(false);
        }
        Self::transition_pulsation(
            transaction,
            current,
            PulsationStatus::Checkpoint,
            stage,
            "stage_failure_opened",
            Some("The stage failed and waits for an explicit decision before closing."),
        )?;
        let (state_version, input_revision): (u64, u64) = transaction.query_row(
            "SELECT state_version,input_revision FROM pulsations WHERE id=?1",
            [&current.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let generation: u64 =
            transaction.query_row("SELECT generation FROM jobs WHERE id=?1", [job_id], |row| {
                row.get(0)
            })?;
        let checkpoint_id = Uuid::new_v4().to_string();
        let now = Self::now_iso();
        transaction.execute(
            "INSERT INTO checkpoints(id,pulsation_id,stage,status,created_at,updated_at,kind,input_revision,state_version,producer_job_id,generation,purpose) VALUES(?1,?2,?3,'waiting',?4,?4,'stage_failure',?5,?6,?7,?8,'stage_failure')",
            params![checkpoint_id, current.id, stage.as_db_value(), now, input_revision, state_version, job_id, generation],
        )?;
        let targets = offered
            .revision_targets
            .iter()
            .map(|stage| stage.as_db_value())
            .collect::<Vec<_>>();
        transaction.execute(
            "INSERT INTO stage_failure_requests(checkpoint_id,pulsation_id,job_id,stage,failure_class,allow_retry,allow_revision,revision_stage,error_code,diagnostic,agent_run_id,created_at,updated_at,stage_execution_id,allowed_revision_targets_json,remaining_budget_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12,?13,?14,?15)",
            params![
                checkpoint_id,
                current.id,
                job_id,
                stage.as_db_value(),
                offered.class.as_db_value(),
                offered.allow_retry as i64,
                offered.allow_revision() as i64,
                offered.primary_revision_stage().map(PulsationStage::as_db_value),
                error_code,
                diagnostic,
                agent_run_id,
                now,
                stage_execution_id,
                serde_json::to_string(&targets).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
                serde_json::to_string(remaining).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
            ],
        )?;
        transaction.execute(
            "UPDATE jobs SET status='interrupted',last_error=?2,cancellation_reason=NULL,cancel_requested_at=NULL,available_at=NULL,finished_at=?3,updated_at=?3 WHERE id=?1 AND status='running'",
            params![job_id, diagnostic, now],
        )?;
        if let Some(run_id) = agent_run_id {
            transaction.execute(
                "UPDATE agent_runs SET status='awaiting_decision',updated_at=?2 WHERE id=?1",
                params![run_id, now],
            )?;
        }
        Ok(true)
    }

    // ----------------------------------------------------------------- recovery

    /// Startup recovery for composed executions: in-flight attempts become
    /// interrupted (their reservations stay consumed), running units fail with
    /// an interruption code so the re-admitted generation retries them within
    /// the shared allowance, and validated results stay untouched for reuse.
    pub(crate) fn recover_stage_executions(db: &Connection) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        db.execute(
            "UPDATE stage_work_unit_attempts SET state='interrupted',error_code='interrupted',diagnostic='The host restarted while the attempt was in flight.',finished_at=?1,updated_at=?1 WHERE state IN ('reserved','running')",
            [&now],
        )?;
        db.execute(
            "UPDATE stage_invocation_reservations SET state=CASE WHEN state='reserved' THEN 'released' ELSE 'settled' END,updated_at=?1 WHERE state IN ('reserved','started')",
            [&now],
        )?;
        db.execute(
            "UPDATE stage_work_units SET state='failed',state_version=state_version+1,failure_code='interrupted',failure_message='The host restarted while the unit was running.',updated_at=?1 WHERE state='running'",
            [&now],
        )?;
        Ok(())
    }
}

fn receipt_status_str(status: ReceiptStatus) -> &'static str {
    match status {
        ReceiptStatus::Passed => "passed",
        ReceiptStatus::Failed => "failed",
        ReceiptStatus::TimedOut => "timed_out",
        ReceiptStatus::Cancelled => "cancelled",
        ReceiptStatus::Unavailable => "unavailable",
        ReceiptStatus::NotRun => "not_run",
        ReceiptStatus::Error => "error",
    }
}

pub(crate) fn check_status_of(status: ReceiptStatus) -> comp::CheckStatus {
    match status {
        ReceiptStatus::Passed => comp::CheckStatus::Passed,
        ReceiptStatus::Failed => comp::CheckStatus::Failed,
        ReceiptStatus::TimedOut => comp::CheckStatus::TimedOut,
        ReceiptStatus::Cancelled => comp::CheckStatus::Cancelled,
        ReceiptStatus::Unavailable => comp::CheckStatus::Unavailable,
        ReceiptStatus::NotRun => comp::CheckStatus::NotRun,
        ReceiptStatus::Error => comp::CheckStatus::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::composition::budget::Pool;
    use crate::orchestration::composition::plan::{PlanInputs, UnitDraft, freeze_plan};
    use crate::orchestration::composition::references::ReferenceTable;
    use crate::orchestration::composition::{
        Handle, ParentFenceV1, RelPath, Sha256Hex, SourceRole, Stage, Uuid as CompUuid,
    };
    use std::collections::BTreeMap;

    struct Fixture {
        _root: tempfile::TempDir,
        service: PersistenceService,
        account: String,
        execution: String,
    }

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    /// A project, a v4 pulsation, its Code job, an open account with one
    /// initial permit and one extra permit, and an admitted single-unit plan.
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let service = PersistenceService::initialize(root.path()).unwrap();
        let db = service.connection().unwrap();
        let now = PersistenceService::now_iso();
        db.execute("INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','Composition',?1,?2,?2)", params![root.path().to_string_lossy(), now]).unwrap();
        db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at,queue_sequence,pipeline_version) VALUES('pulse','p','Feature','Code','running',?1,?1,1,4)", [&now]).unwrap();
        db.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at,project_id,input_revision,generation,state_version,idempotency_key) VALUES('job','pulse','run_stage::Code','running',?1,?1,'p',1,1,1,'job')", [&now]).unwrap();
        let mut builder = ReferenceTable::builder();
        builder
            .add_task("t-1", 1, "One", &[("AC-1".into(), "a".into())])
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/a.rs").unwrap(),
                Sha256Hex::of(b"a"),
                SourceRole::Source,
            )
            .unwrap();
        let table = builder.build();
        let (snapshot_id, _) = PersistenceService::snapshot_policy(
            &db,
            "executor_profile",
            Some("p"),
            &serde_json::json!({"executorKind":"code_implementer","transport":"api"}),
        )
        .unwrap();
        let mut snapshots = BTreeMap::new();
        snapshots.insert(
            ExecutorKind::CodeImplementer,
            CompUuid::new(snapshot_id.clone()).unwrap(),
        );
        snapshots.insert(
            ExecutorKind::CodeRepairer,
            CompUuid::new(snapshot_id.clone()).unwrap(),
        );
        let account = PersistenceService::open_budget_account(
            &db,
            "pulse",
            PulsationStage::Code,
            1,
            &StageBudgetV1 {
                initial_invocation_limit: 1,
                extra_invocation_limit: 1,
                per_unit_extra_invocation_limit: 1,
                initial_output_token_limit: 4_000,
                extra_output_token_limit: 4_000,
                initial_prompt_character_limit: 64_000,
                extra_prompt_character_limit: 64_000,
                max_in_flight_units: 1,
            },
            "policy",
        )
        .unwrap();
        let plan = freeze_plan(PlanInputs {
            stage_execution_id: CompUuid::generate(),
            budget_account_id: CompUuid::new(account.clone()).unwrap(),
            parent_fence: ParentFenceV1 {
                project_id: CompUuid::generate(),
                pulsation_id: CompUuid::generate(),
                input_revision: 1,
                job_id: CompUuid::generate(),
                generation: 1,
                state_version: 1,
            },
            stage: Stage::Code,
            policy_sha256: Sha256Hex::of(b"policy"),
            batch_sha256: Sha256Hex::of(b"batch"),
            approved_contract_sha256: None,
            table: &table,
            units: vec![UnitDraft {
                logical_unit_key: h("implement"),
                executor_kind: ExecutorKind::CodeImplementer,
                task_refs: vec![h("task_1")],
                criterion_refs: vec![h("crit_1_1")],
                depends_on: vec![],
                input_refs: vec![h("src_1")],
                owned_destination_refs: vec![h("src_1")],
                required_check_refs: vec![],
                objective: "Implement.".into(),
                estimated_output_tokens: 500,
                unit_id: None,
            }],
            stage_output_limit: 4_000,
            context_chars_limit: 16_000,
            max_in_flight_units: 1,
            profile_snapshots: snapshots,
        })
        .unwrap();
        PersistenceService::admit_execution(
            &db,
            "pulse",
            "job",
            &plan,
            &account,
            &snapshot_id,
            None,
        )
        .unwrap();
        let execution = plan.stage_execution_id.as_str().to_owned();
        Fixture {
            _root: root,
            service,
            account,
            execution,
        }
    }

    fn unit(db: &Connection, execution: &str) -> UnitRow {
        PersistenceService::load_units(db, execution)
            .unwrap()
            .remove(0)
    }

    fn profile() -> EffectiveProfile {
        EffectiveProfile {
            executor_kind: "code_implementer".into(),
            transport: "api".into(),
            backend: None,
            requested_model: "fixture".into(),
            thinking_level: None,
            schema_flavor: SchemaFlavor::Canonical,
            attempt_limit: 1,
            source: "test".into(),
        }
    }

    #[test]
    fn failure_diagnostics_are_sanitized_before_sqlite() {
        let fixture = fixture();
        let db = fixture.service.connection().unwrap();
        let unit = unit(&db, &fixture.execution);
        let diagnostic = r#"{"message":"Authorization: Bearer synthetic-credential-value"}"#;
        PersistenceService::transition_unit(
            &db,
            &unit,
            UnitState::Failed,
            Some(("failure", diagnostic)),
        )
        .unwrap();
        PersistenceService::transition_execution(
            &db,
            &fixture.execution,
            &[ExecutionStatus::Planned],
            ExecutionStatus::Parked,
            Some(("failure", diagnostic)),
        )
        .unwrap();
        for (table, column, id) in [
            ("stage_work_units", "failure_message", unit.id.as_str()),
            (
                "stage_executions",
                "blocked_reason",
                fixture.execution.as_str(),
            ),
        ] {
            let stored: String = db
                .query_row(
                    &format!("SELECT {column} FROM {table} WHERE id=?1"),
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!stored.contains("synthetic-credential-value"));
            assert!(stored.contains("<REDACTED_SECRET:"));
        }
    }

    #[test]
    fn concurrent_reservations_admit_exactly_one_initial_permit() {
        let fixture = fixture();
        let unit = unit(&fixture.service.connection().unwrap(), &fixture.execution);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let outcomes = (0..2)
            .map(|ordinal| {
                let service = fixture.service.clone();
                let account = fixture.account.clone();
                let execution = fixture.execution.clone();
                let unit = unit.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut db = service.connection().unwrap();
                    barrier.wait();
                    let tx = db
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                        .unwrap();
                    let result = PersistenceService::reserve_invocation(
                        &tx,
                        &account,
                        &execution,
                        &unit,
                        Pool::Initial,
                        ordinal + 1,
                        500,
                        2_000,
                    );
                    tx.commit().unwrap();
                    result.is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(outcomes.iter().filter(|ok| **ok).count(), 1, "{outcomes:?}");
        let ledger = PersistenceService::load_ledger(
            &fixture.service.connection().unwrap(),
            &fixture.account,
        )
        .unwrap();
        assert_eq!(ledger.remaining().initial_invocations, 0);
        assert_eq!(ledger.remaining().extra_invocations, 1);
    }

    #[test]
    fn the_extra_pool_is_capped_per_unit_and_stage_wide() {
        let fixture = fixture();
        let db = fixture.service.connection().unwrap();
        let unit = unit(&db, &fixture.execution);
        PersistenceService::reserve_invocation(
            &db,
            &fixture.account,
            &fixture.execution,
            &unit,
            Pool::Extra,
            1,
            500,
            2_000,
        )
        .unwrap();
        let second = PersistenceService::reserve_invocation(
            &db,
            &fixture.account,
            &fixture.execution,
            &unit,
            Pool::Extra,
            2,
            500,
            2_000,
        );
        assert!(
            matches!(second, Err(PersistenceServiceError::Validation(_))),
            "{second:?}"
        );
        // Releasing a never-started permit returns it to the pool.
        let id: String = db
            .query_row(
                "SELECT id FROM stage_invocation_reservations WHERE pool='extra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        PersistenceService::release_reservation(&db, &id).unwrap();
        assert_eq!(
            PersistenceService::load_ledger(&db, &fixture.account)
                .unwrap()
                .remaining()
                .extra_invocations,
            1
        );
    }

    #[test]
    fn retry_generations_share_the_account_and_a_closed_account_gets_a_successor() {
        let fixture = fixture();
        let db = fixture.service.connection().unwrap();
        let budget = StageBudgetV1 {
            initial_invocation_limit: 1,
            extra_invocation_limit: 1,
            per_unit_extra_invocation_limit: 1,
            initial_output_token_limit: 4_000,
            extra_output_token_limit: 4_000,
            initial_prompt_character_limit: 64_000,
            extra_prompt_character_limit: 64_000,
            max_in_flight_units: 1,
        };
        let again = PersistenceService::open_budget_account(
            &db,
            "pulse",
            PulsationStage::Code,
            1,
            &budget,
            "policy",
        )
        .unwrap();
        assert_eq!(
            again, fixture.account,
            "a Retry generation reuses the logical stage's account"
        );
        PersistenceService::close_budget_accounts(&db, "pulse", PulsationStage::Code).unwrap();
        let successor = PersistenceService::open_budget_account(
            &db,
            "pulse",
            PulsationStage::Code,
            1,
            &budget,
            "policy",
        )
        .unwrap();
        assert_ne!(successor, fixture.account);
        let predecessor: Option<String> = db
            .query_row(
                "SELECT predecessor_account_id FROM stage_budget_accounts WHERE id=?1",
                [&successor],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(predecessor.as_deref(), Some(fixture.account.as_str()));
        let keys: Vec<String> = db
            .prepare("SELECT logical_stage_key FROM stage_budget_accounts ORDER BY created_at")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(keys, vec!["pulse:Code:1:1", "pulse:Code:1:2"]);
    }

    #[test]
    fn restart_recovery_settles_started_permits_and_releases_unstarted_ones() {
        let fixture = fixture();
        let db = fixture.service.connection().unwrap();
        let unit = unit(&db, &fixture.execution);
        let started = PersistenceService::reserve_invocation(
            &db,
            &fixture.account,
            &fixture.execution,
            &unit,
            Pool::Initial,
            1,
            500,
            2_000,
        )
        .unwrap();
        let attempt = PersistenceService::create_attempt(
            &db,
            "job",
            &fixture.execution,
            &unit,
            1,
            &started.id,
            ExecutorKind::CodeImplementer,
            &snapshot_id(&db),
            &profile(),
        )
        .unwrap();
        PersistenceService::transition_unit(&db, &unit, UnitState::Running, None).unwrap();
        PersistenceService::mark_attempt_started(&db, &attempt, "llm-exec", "dispatch", "input")
            .unwrap();
        let unstarted = PersistenceService::reserve_invocation(
            &db,
            &fixture.account,
            &fixture.execution,
            &unit,
            Pool::Extra,
            2,
            500,
            2_000,
        )
        .unwrap();

        PersistenceService::recover_stage_executions(&db).unwrap();

        let state = |id: &str| -> String {
            db.query_row(
                "SELECT state FROM stage_invocation_reservations WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            state(&started.id),
            "settled",
            "a request that may have left the host is consumed"
        );
        assert_eq!(
            state(&unstarted.id),
            "released",
            "a permit that never dispatched returns to the pool"
        );
        let attempt_state: String = db
            .query_row(
                "SELECT state FROM stage_work_unit_attempts WHERE id=?1",
                [&attempt.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attempt_state, "interrupted");
        let remaining = PersistenceService::load_ledger(&db, &fixture.account)
            .unwrap()
            .remaining();
        assert_eq!(
            (remaining.initial_invocations, remaining.extra_invocations),
            (0, 1)
        );
        let unit_state: String = db
            .query_row(
                "SELECT state FROM stage_work_units WHERE id=?1",
                [&unit.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unit_state, "failed");
    }

    #[test]
    fn a_stale_unit_row_cannot_accept_a_child() {
        let fixture = fixture();
        let db = fixture.service.connection().unwrap();
        let stale = unit(&db, &fixture.execution);
        PersistenceService::transition_unit(&db, &stale, UnitState::Running, None).unwrap();
        let late = PersistenceService::accept_child(&db, &stale, None, "{}", "sha", "reuse", None);
        assert!(
            matches!(late, Err(PersistenceServiceError::Validation(_))),
            "{late:?}"
        );
        let fresh = unit(&db, &fixture.execution);
        PersistenceService::accept_child(&db, &fresh, None, "{}", "sha", "reuse", None).unwrap();
        let fresh = unit(&db, &fixture.execution);
        assert_eq!(fresh.state, UnitState::Validated);
        assert_eq!(fresh.state_version, stale.state_version + 2);
        // A validated unit is final for its generation.
        let twice = PersistenceService::accept_child(&db, &fresh, None, "{}", "sha", "reuse", None);
        assert!(twice.is_err());
    }

    fn snapshot_id(db: &Connection) -> String {
        db.query_row(
            "SELECT id FROM composition_policy_snapshots LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod validation_profile_tests {
    use super::*;
    use crate::orchestration::composition::validation::profile::detect_presets;

    fn service_with_project() -> (tempfile::TempDir, PersistenceService, String) {
        let root = tempfile::tempdir().unwrap();
        let service = PersistenceService::initialize(root.path()).unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let project = service
            .ensure_project_for_workspace(
                crate::persistence_service::EnsureProjectForWorkspaceRequest {
                    workspace_path: workspace.to_string_lossy().into_owned(),
                    preferred_name: None,
                },
            )
            .unwrap()
            .project
            .id;
        (root, service, project)
    }

    #[test]
    fn profiles_are_versioned_on_change_and_wrappers_are_refused() {
        let (_root, service, project) = service_with_project();
        let presets = service.detect_validation_presets(&project).unwrap();
        assert_eq!(presets.len(), 2);
        let mut check = presets[0].clone();
        let initial_version = check.profile_version;
        let stored = service
            .upsert_validation_profile(&project, check.clone(), true)
            .unwrap();
        assert_eq!(stored.profile_version, initial_version);
        // Same content: the version stays.
        let again = service
            .upsert_validation_profile(&project, check.clone(), false)
            .unwrap();
        assert_eq!(again.profile_version, initial_version);
        let listed = service.list_validation_profiles(&project).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].1, "the enabled flag is updated in place");
        // Changed argv: a new version, so old receipts never match.
        check.argv.push("--all-targets".into());
        let bumped = service
            .upsert_validation_profile(&project, check, true)
            .unwrap();
        assert_eq!(bumped.profile_version, initial_version + 1);
        let mut wrapper = presets[1].clone();
        wrapper.executable = r"C:\tools\cargo.cmd".into();
        let refused = service.upsert_validation_profile(&project, wrapper, true);
        assert!(
            matches!(refused, Err(PersistenceServiceError::Validation(_))),
            "{refused:?}"
        );
        service
            .delete_validation_profile(&project, "cargo_check")
            .unwrap();
        assert!(
            service
                .list_validation_profiles(&project)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            detect_presets(std::path::Path::new("/definitely/missing")).len(),
            0
        );
    }
}
