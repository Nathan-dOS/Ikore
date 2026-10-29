//! Just-in-time scope requests in the host (ADR-035 M4).
//!
//! A Code or Test unit that reports `scope_insufficient` with refs parks its
//! stage on the usual failure checkpoint. When the derivation accepts at
//! least one delivered file and the stage may still ask, the request is kept
//! with that checkpoint. "Grant and retry" derives it again, checks that every
//! file is still the delivered one, issues a `scope_grant` mandate revision
//! and runs the existing Retry, in one immediate transaction. Any other action
//! declines it, and a revision of the stage supersedes a granted one. The plan
//! of every later generation gives the unit its granted files
//! (`recipes::prepare`).
//!
//! Included into `persistence_service` so it can use the service's private
//! helpers.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::composition_persistence::ExecutionRow;
use super::mandate_persistence::ReceiptInput;
use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, PulsationStage,
    ResolveStageFailureResponse, Uuid, params, sanitized_observation_text,
};
use crate::orchestration::composition::budget::Pool;
use crate::orchestration::composition::recipes::StageInputs;
use crate::orchestration::composition::review_boundary::{STAGE_DOCUMENT_KINDS, risk_level_name};
use crate::orchestration::composition::risk::{self, RiskPolicyV1};
use crate::orchestration::composition::{self as comp, Handle, RelPath, Risk};
use crate::orchestration::mandate::{
    self, IssuedBy, SCOPE_GRANT_COMMAND_VERSION, ScopeFacts, ScopeGrantCommandV1, ScopePathV1,
    ScopeRefusalV1, ScopeRequestDerivationV1, path_key, text_sha256,
};
use crate::orchestration::publication::observed_hash;

/// Characters of the worker's reason the card shows.
const MAX_AGENT_MESSAGE_CHARS: usize = 400;

fn runtime(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(error.to_string())
}

fn parse_risk(value: &str) -> Risk {
    match value {
        "low" => Risk::Low,
        "medium" => Risk::Medium,
        "high" => Risk::High,
        _ => Risk::Unknown,
    }
}

/// A derivation ready to be kept with the checkpoint the stage parks on.
pub(super) struct ScopeRequestDraft {
    pub derivation: ScopeRequestDerivationV1,
    pub related_refs: Vec<Handle>,
    pub agent_message: String,
    pub unit_id: String,
    pub budget_account_id: String,
}

/// A scope request as the failure checkpoint shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRequestView {
    pub request_id: String,
    pub checkpoint_id: String,
    pub stage: PulsationStage,
    pub unit_key: String,
    pub accepted: Vec<ScopePathV1>,
    pub refused: Vec<ScopeRefusalV1>,
    pub risk_before: Risk,
    pub risk_after: Risk,
    /// The worker's own reason, sanitized and cut to 400 characters.
    pub agent_message: Option<String>,
    pub derivation_sha256: String,
    /// `pending`, `granted`, `declined`, `stale` or `superseded`.
    pub state: String,
    /// Extra invocations the stage's shared budget still holds.
    pub remaining_extra_invocations: u32,
}

impl PersistenceService {
    /// Path identities of this pulsation's accepted stage documents, and of
    /// the files another stage of it produced.
    fn scope_artifact_paths(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<(BTreeSet<String>, BTreeSet<String>), PersistenceServiceError> {
        let mut documents = BTreeSet::new();
        let mut foreign = BTreeSet::new();
        for row in db
            .prepare(
                "SELECT relative_path,kind,COALESCE(producer_stage,'') FROM artifacts WHERE pulsation_id=?1 AND acceptance='accepted' AND relative_path IS NOT NULL",
            )?
            .query_map([pulsation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
        {
            let (path, kind, producer) = row?;
            if STAGE_DOCUMENT_KINDS.contains(&kind.as_str()) {
                documents.insert(path_key(&path));
            } else if producer != stage.as_db_value() {
                foreign.insert(path_key(&path));
            }
        }
        Ok((documents, foreign))
    }

    /// Derives the request of `unit_key` from the rows `db` holds now, with
    /// the given risk levels. `exclude` is a request not to count against the
    /// stage's limit: the one being granted.
    pub(super) fn derive_scope_request_on(
        db: &Connection,
        execution: &ExecutionRow,
        unit_key: &str,
        related_refs: &[Handle],
        risk_before: Risk,
        risk_after: impl FnOnce(&[ScopePathV1]) -> Risk,
        exclude: Option<&str>,
    ) -> Result<Option<ScopeRequestDerivationV1>, PersistenceServiceError> {
        let (documents, foreign) =
            Self::scope_artifact_paths(db, &execution.pulsation_id, execution.stage)?;
        let mandate = Self::current_mandate(db, &execution.pulsation_id)?;
        let earlier: u32 = db.query_row(
            "SELECT COUNT(*) FROM mandate_scope_requests WHERE budget_account_id=?1 AND id IS NOT ?2",
            params![execution.budget_account_id, exclude],
            |row| row.get(0),
        )?;
        // The retry's first attempt takes an initial permit while one is
        // left, the extra pool otherwise.
        let ledger = Self::load_ledger(db, &execution.budget_account_id)?;
        let used = Self::unit_extra_used(db, &execution.budget_account_id, unit_key)?;
        let output = execution
            .plan
            .units
            .iter()
            .find(|unit| unit.logical_unit_key.as_str() == unit_key)
            .map_or(0, |unit| unit.max_output_tokens);
        let budget_allows = ledger.can_reserve(Pool::Initial, output, 0, used).is_ok()
            || ledger.can_reserve(Pool::Extra, output, 0, used).is_ok();
        let project_id: String = db.query_row(
            "SELECT project_id FROM pulsations WHERE id=?1",
            [&execution.pulsation_id],
            |row| row.get(0),
        )?;
        let settings = Self::composition_settings_for(db, &project_id)?;
        let policy = RiskPolicyV1::effective(&settings.risk_policy);
        let sensitive = |path: &str| !policy.sensitive_paths(std::iter::once(path)).is_empty();
        Ok(mandate::derive_scope_request(
            &ScopeFacts {
                plan: &execution.plan,
                unit_key,
                related_refs,
                stage_documents: &documents,
                foreign_paths: &foreign,
                mandate: mandate
                    .as_ref()
                    .map(|(mandate, sha)| (mandate, sha.as_str())),
                earlier_requests: earlier as usize,
                budget_allows,
                risk_before,
                is_sensitive: &sensitive,
            },
            risk_after,
        ))
    }

    /// The request a `scope_insufficient` failure of `unit_key` would raise,
    /// with the risk levels before and after its files.
    pub(super) fn scope_request_draft(
        &self,
        execution: &ExecutionRow,
        unit_id: &str,
        unit_key: &str,
        related_refs: &[Handle],
        message: &str,
        inputs: &StageInputs,
    ) -> Result<Option<ScopeRequestDraft>, PersistenceServiceError> {
        let db = self.connection()?;
        let stage = comp::Stage::from_pulsation(execution.stage);
        let input_revision = u64::from(execution.plan.parent_fence.input_revision);
        let before =
            Self::stage_risk_assessment(&db, &execution.pulsation_id, stage, input_revision)?;
        let earlier = if stage == comp::Stage::Code {
            None
        } else {
            Self::stage_risk_assessment(
                &db,
                &execution.pulsation_id,
                comp::Stage::Code,
                input_revision,
            )?
        };
        let project_id: String = db.query_row(
            "SELECT project_id FROM pulsations WHERE id=?1",
            [&execution.pulsation_id],
            |row| row.get(0),
        )?;
        let policy =
            RiskPolicyV1::effective(&Self::composition_settings_for(&db, &project_id)?.risk_policy);
        let derivation = Self::derive_scope_request_on(
            &db,
            execution,
            unit_key,
            related_refs,
            before
                .as_ref()
                .map_or(Risk::Unknown, |assessment| assessment.level),
            |accepted| {
                let mut facts = risk::inputs_from_stage(inputs, &[]);
                facts
                    .paths
                    .extend(accepted.iter().map(|path| path.path.clone()));
                risk::raise(risk::assess(&facts, &policy), earlier.as_ref()).level
            },
            None,
        )?;
        Ok(derivation.map(|derivation| ScopeRequestDraft {
            derivation,
            related_refs: related_refs.to_vec(),
            agent_message: message.to_owned(),
            unit_id: unit_id.to_owned(),
            budget_account_id: execution.budget_account_id.clone(),
        }))
    }

    /// Keeps a grantable request with the checkpoint its stage parked on.
    pub(super) fn record_scope_request(
        tx: &Connection,
        checkpoint_id: &str,
        pulsation_id: &str,
        draft: &ScopeRequestDraft,
    ) -> Result<String, PersistenceServiceError> {
        let derivation = &draft.derivation;
        let id = Uuid::new_v4().to_string();
        let message = sanitized_observation_text(draft.agent_message.trim())
            .chars()
            .take(MAX_AGENT_MESSAGE_CHARS)
            .collect::<String>();
        tx.execute(
            "INSERT INTO mandate_scope_requests(id,pulsation_id,stage_execution_id,budget_account_id,unit_id,unit_key,checkpoint_id,stage,related_refs_json,accepted_json,refused_json,derivation_json,derivation_sha256,risk_before,risk_after,agent_message,agent_message_sha256,state,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,'pending',?18)",
            params![
                id,
                pulsation_id,
                derivation.stage_execution_id,
                draft.budget_account_id,
                draft.unit_id,
                derivation.unit_key,
                checkpoint_id,
                derivation.stage.to_pulsation().as_db_value(),
                serde_json::to_string(&draft.related_refs).map_err(runtime)?,
                serde_json::to_string(&derivation.accepted).map_err(runtime)?,
                serde_json::to_string(&derivation.refused).map_err(runtime)?,
                serde_json::to_string(derivation).map_err(runtime)?,
                derivation.sha256().map_err(runtime)?,
                risk_level_name(derivation.risk_before),
                risk_level_name(derivation.risk_after),
                (!message.is_empty()).then_some(message.as_str()),
                (!draft.agent_message.trim().is_empty())
                    .then(|| text_sha256(draft.agent_message.trim())),
                Self::now_iso()
            ],
        )?;
        Ok(id)
    }

    /// The request kept with a failure checkpoint, if any.
    pub(super) fn scope_request_view(
        db: &Connection,
        checkpoint_id: &str,
    ) -> Result<Option<ScopeRequestView>, PersistenceServiceError> {
        let row = db
            .query_row(
                "SELECT r.id,r.stage,r.unit_key,r.accepted_json,r.refused_json,r.risk_before,r.risk_after,r.agent_message,r.derivation_sha256,r.state,r.budget_account_id
                 FROM mandate_scope_requests r WHERE r.checkpoint_id=?1",
                [checkpoint_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            request_id,
            stage,
            unit_key,
            accepted,
            refused,
            before,
            after,
            agent_message,
            derivation_sha256,
            state,
            account,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(ScopeRequestView {
            request_id,
            checkpoint_id: checkpoint_id.to_owned(),
            stage: PulsationStage::from_db_value(&stage)
                .ok_or_else(|| runtime("Invalid stage in a scope request."))?,
            unit_key,
            accepted: serde_json::from_str(&accepted).map_err(runtime)?,
            refused: serde_json::from_str(&refused).map_err(runtime)?,
            risk_before: parse_risk(&before),
            risk_after: parse_risk(&after),
            agent_message,
            derivation_sha256,
            state,
            remaining_extra_invocations: Self::load_ledger(db, &account)?
                .remaining()
                .extra_invocations,
        }))
    }

    /// Marks a request stale and says why, in its own transaction's commit.
    fn stale_scope_request(
        tx: &Connection,
        request_id: &str,
        reason: &str,
    ) -> Result<PersistenceServiceError, PersistenceServiceError> {
        tx.execute(
            "UPDATE mandate_scope_requests SET state='stale',decided_at=?2 WHERE id=?1 AND state='pending'",
            params![request_id, Self::now_iso()],
        )?;
        Ok(PersistenceServiceError::Validation(format!(
            "scope_request_stale: {reason} Nothing was granted; the other decisions are still available."
        )))
    }

    /// "Grant and retry": the files of a pending request become the unit's
    /// own, under a new mandate revision, and the stage runs again on its
    /// shared budget. Nothing is granted unless the request still derives the
    /// same way and every file is still the delivered one.
    pub fn grant_scope_and_retry(
        &self,
        command: ScopeGrantCommandV1,
    ) -> Result<ResolveStageFailureResponse, PersistenceServiceError> {
        if command.schema_version != SCOPE_GRANT_COMMAND_VERSION {
            return Err(PersistenceServiceError::Validation(format!(
                "Unsupported scope grant command version {}.",
                command.schema_version
            )));
        }
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let failure = Self::waiting_stage_failure(
            &tx,
            &command.checkpoint_id,
            command.expected_state_version,
        )?;
        if failure.pulsation_id != command.pulsation_id {
            return Err(PersistenceServiceError::Validation(
                "The scope request belongs to another pulsation.".into(),
            ));
        }
        let request: Option<(String, String, String, String, String, String)> = tx
            .query_row(
                "SELECT stage_execution_id,unit_key,related_refs_json,derivation_json,derivation_sha256,state FROM mandate_scope_requests WHERE id=?1 AND checkpoint_id=?2",
                params![command.request_id, command.checkpoint_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((execution_id, unit_key, related_json, derivation_json, stored_sha, state)) =
            request
        else {
            return Err(PersistenceServiceError::Validation(
                "The failure checkpoint has no such scope request.".into(),
            ));
        };
        if state != "pending" {
            return Err(PersistenceServiceError::Validation(format!(
                "The scope request is {state}, not pending. Refresh before deciding."
            )));
        }
        let (project_id, root): (String, String) = tx.query_row(
            "SELECT p.project_id,r.workspace_path FROM pulsations p JOIN projects r ON r.id=p.project_id WHERE p.id=?1",
            [&failure.pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if project_id != command.project_id {
            return Err(PersistenceServiceError::Validation(
                "The scope request belongs to another project.".into(),
            ));
        }
        let stage = PulsationStage::from_db_value(&failure.stage_raw)
            .ok_or_else(|| runtime("Invalid stage in the failure checkpoint."))?;
        let current = Self::find_pulsation_by_id(&tx, &failure.pulsation_id)?
            .ok_or_else(|| PersistenceServiceError::NotFound("The pulsation is gone.".into()))?;
        if current.stage != stage {
            return Err(PersistenceServiceError::Validation(
                "The pulsation stage changed; refresh the checkpoint.".into(),
            ));
        }
        let stored: ScopeRequestDerivationV1 =
            serde_json::from_str(&derivation_json).map_err(runtime)?;
        let related: Vec<Handle> = serde_json::from_str(&related_json).map_err(runtime)?;
        let execution = Self::find_execution(&tx, &execution_id)?
            .ok_or_else(|| runtime("The parked stage execution is gone."))?;
        let derived = Self::derive_scope_request_on(
            &tx,
            &execution,
            &unit_key,
            &related,
            stored.risk_before,
            |_| stored.risk_after,
            Some(&command.request_id),
        )?;
        let same = derived
            .as_ref()
            .filter(|derived| derived.grantable())
            .map(ScopeRequestDerivationV1::sha256)
            .transpose()
            .map_err(runtime)?;
        if same.as_deref() != Some(stored_sha.as_str())
            || command.expected_derivation_sha256 != stored_sha
        {
            let error = Self::stale_scope_request(
                &tx,
                &command.request_id,
                "The plan, the mandate or the stage budget changed since the request.",
            )?;
            tx.commit()?;
            return Err(error);
        }
        let changed = stored
            .accepted
            .iter()
            .filter(|path| {
                observed_hash(Path::new(&root), &path.path)
                    .ok()
                    .flatten()
                    .is_none_or(|sha| sha.as_str() != path.sha256)
            })
            .map(|path| format!("`{}`", path.path))
            .collect::<Vec<_>>();
        if !changed.is_empty() {
            let error = Self::stale_scope_request(
                &tx,
                &command.request_id,
                &format!("{} changed since the request.", changed.join(", ")),
            )?;
            tx.commit()?;
            return Err(error);
        }
        let Some((mandate, sha)) = Self::current_mandate(&tx, &failure.pulsation_id)? else {
            return Err(PersistenceServiceError::Validation(
                "The pulsation has no mandate to amend.".into(),
            ));
        };
        let paths = stored
            .accepted
            .iter()
            .map(|path| path.path.clone())
            .collect::<Vec<_>>();
        let input_revision: u64 = tx.query_row(
            "SELECT input_revision FROM pulsations WHERE id=?1",
            [&failure.pulsation_id],
            |row| row.get(0),
        )?;
        let next = mandate::amend_scope(
            &mandate,
            &sha,
            &unit_key,
            &paths,
            IssuedBy {
                checkpoint_id: command.checkpoint_id.clone(),
                stage: stage.as_db_value().to_owned(),
                state_version: command.expected_state_version,
                input_revision,
            },
        );
        Self::insert_mandate(&tx, &next)?;
        let now = Self::now_iso();
        tx.execute(
            "UPDATE checkpoints SET status='approved',updated_at=?2 WHERE id=?1",
            params![command.checkpoint_id, now],
        )?;
        tx.execute(
            "UPDATE stage_failure_requests SET decision='retry_stage',resolved_at=?2,updated_at=?2 WHERE checkpoint_id=?1",
            params![command.checkpoint_id, now],
        )?;
        let updated =
            Self::resume_failed_stage(&tx, &current, stage, &failure.producer_job_id, &now)?;
        tx.execute(
            "UPDATE mandate_scope_requests SET state='granted',decision='grant_scope',mandate_revision=?2,decided_at=?3 WHERE id=?1 AND state='pending'",
            params![command.request_id, next.revision, now],
        )?;
        Self::record_approval_receipt(
            &tx,
            ReceiptInput {
                checkpoint_id: &command.checkpoint_id,
                pulsation_id: &failure.pulsation_id,
                purpose: "stage_failure",
                stage: stage.as_db_value(),
                decision: "grant_scope",
                state_version: command.expected_state_version,
                reviewed: Vec::new(),
                confirmed_paths: paths,
                feedback: None,
            },
        )?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(ResolveStageFailureResponse { pulsation: updated })
    }

    /// Any decision other than a grant declines the checkpoint's request.
    pub(super) fn decline_scope_request(
        tx: &Connection,
        checkpoint_id: &str,
        decision: &str,
        now: &str,
    ) -> Result<(), PersistenceServiceError> {
        tx.execute(
            "UPDATE mandate_scope_requests SET state='declined',decision=?2,decided_at=?3 WHERE checkpoint_id=?1 AND state='pending'",
            params![checkpoint_id, decision, now],
        )?;
        Ok(())
    }

    /// A revision from `from_stage` on supersedes the requests of the stages
    /// it reruns: their files stop being granted to the next plans.
    pub(super) fn supersede_scope_requests(
        tx: &Connection,
        pulsation_id: &str,
        from_stage: PulsationStage,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        for stage in [PulsationStage::Code, PulsationStage::Test] {
            if stage < from_stage {
                continue;
            }
            tx.execute(
                "UPDATE mandate_scope_requests SET state='superseded',decided_at=COALESCE(decided_at,?3) WHERE pulsation_id=?1 AND stage=?2 AND state IN ('pending','granted')",
                params![pulsation_id, stage.as_db_value(), now],
            )?;
        }
        Ok(())
    }

    /// The files granted to each unit of `stage`, for its next plan.
    pub(super) fn granted_scope_amendments(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<Vec<(String, RelPath)>, PersistenceServiceError> {
        let mut amendments = Vec::new();
        for row in db
            .prepare(
                "SELECT unit_key,accepted_json FROM mandate_scope_requests WHERE pulsation_id=?1 AND stage=?2 AND state='granted' ORDER BY created_at,id",
            )?
            .query_map(params![pulsation_id, stage.as_db_value()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
        {
            let (unit_key, accepted) = row?;
            for path in serde_json::from_str::<Vec<ScopePathV1>>(&accepted).map_err(runtime)? {
                amendments.push((
                    unit_key.clone(),
                    RelPath::new(path.path).map_err(|error| runtime(error.message))?,
                ));
            }
        }
        Ok(amendments)
    }
}
