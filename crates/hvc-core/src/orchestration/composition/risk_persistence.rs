//! Risk-driven scrutiny of composed pulsations: what the host records and
//! enforces (ADR-034 M3).
//!
//! The level is assessed when Code and Test are admitted, where it decides
//! which configured checks become required, and again when the Integrator is
//! prepared, where it can only rise. The `REVIEW.md` guides are captured once
//! at the pulsation's first Intake admission and the Integrator reviews under
//! that capture. After the review the host derives the coverage matrix from
//! the kept verdicts and its own receipts; approving a high-risk review with
//! unverified or unrun items needs an acknowledgement with a reason.
//!
//! Included into `persistence_service` so it can use the service's private
//! helpers. Everything is derived from durable rows, so a restart keeps it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, PulsationStage,
    Uuid, params,
};
use crate::orchestration::PipelineVersion;
use crate::orchestration::composition::recipes::StageInputs;
use crate::orchestration::composition::review_boundary::{
    Freshness, GapScope, HostCheckRow, ReviewBoundary, RiskReview, risk_level_name,
};
use crate::orchestration::composition::review_policy::{self, PolicyCapture, PolicyRead};
use crate::orchestration::composition::risk::{
    self, CheckFact, CoverageMatrix, RiskAssessment, RiskPolicyV1, VerdictFact,
};
use crate::orchestration::composition::validation::evidence::{CheckResult, InterpretationStatus};
use crate::orchestration::composition::validation::{
    RunEvidenceDetailsV1, ValidationProfileSnapshot,
};
use crate::orchestration::composition::{
    self as comp, CompletedPayloadV1, Handle, UnitModelResponseV1, ValidationCommandProfileV1,
};
use crate::repository_context::RepositoryContext;

/// The longest reason kept with an acknowledgement, in Unicode scalar values.
pub const MAX_ACKNOWLEDGEMENT_CHARS: usize = 400;

/// The developer's acknowledgement when approving a high-risk review that
/// still has unverified or unrun items (ADR-034 M3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskAcknowledgement {
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskAcknowledgementRecord {
    pub level: String,
    pub pending: u32,
    pub reason: String,
    pub created_at: String,
}

/// What the Integrator checkpoint shows about risk and coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRiskStatus {
    /// Pipeline v4 only.
    pub applies: bool,
    pub assessment: Option<RiskAssessment>,
    pub matrix: Option<CoverageMatrix>,
    /// Approving the waiting review needs an acknowledgement with a reason.
    pub acknowledgement_required: bool,
    pub acknowledgements: Vec<RiskAcknowledgementRecord>,
}

const fn check_status_name(status: comp::CheckStatus) -> &'static str {
    match status {
        comp::CheckStatus::Pending => "pending",
        comp::CheckStatus::Running => "running",
        comp::CheckStatus::Passed => "passed",
        comp::CheckStatus::Failed => "failed",
        comp::CheckStatus::TimedOut => "timed_out",
        comp::CheckStatus::Cancelled => "cancelled",
        comp::CheckStatus::Unavailable => "unavailable",
        comp::CheckStatus::NotRun => "not_run",
        comp::CheckStatus::Error => "error",
    }
}

fn runtime(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(error.to_string())
}

fn bounded_coverage_text(text: &str) -> String {
    crate::secret_redactor::redact_secrets(text)
        .map(|view| view.text.chars().take(400).collect())
        .unwrap_or_else(|_| "Evidence text unavailable (sanitization failed).".to_owned())
}

fn unverified_check(mut fact: CheckFact, reason: &str) -> CheckFact {
    fact.fresh = Some(false);
    if fact.interpretation.as_deref() != Some("partial") {
        fact.interpretation = Some("uninterpreted".into());
    }
    fact.result = Some("unknown".into());
    fact.limitations.truncate(7);
    fact.limitations.push(bounded_coverage_text(reason));
    fact
}

/// Execution coverage is weaker than a successful check, but stronger than
/// launching a process. A supported failure is useful executed evidence;
/// timeout, incomplete parsing or a changed candidate is not.
fn coverage_fact_for_receipt(
    row: HostCheckRow,
    receipt: Option<&comp::ValidationReceiptV1>,
    snapshot: Option<&ValidationProfileSnapshot>,
    details: Option<&RunEvidenceDetailsV1>,
    expected_execution: Option<&str>,
) -> CheckFact {
    let expected_receipt_id = row.receipt_id;
    let mut fact = CheckFact {
        check_ref: row.check_ref,
        status: bounded_coverage_text(&row.status),
        fresh: match row.freshness {
            Freshness::Fresh => Some(true),
            Freshness::Stale | Freshness::Unverifiable => Some(false),
            Freshness::NotApplicable => None,
        },
        interpretation: row.interpretation.map(|text| bounded_coverage_text(&text)),
        result: row.result.map(|text| bounded_coverage_text(&text)),
        limitations: row
            .limitations
            .iter()
            .take(8)
            .map(|text| bounded_coverage_text(text))
            .collect(),
    };
    let Some(receipt) = receipt else {
        return unverified_check(fact, "No complete host execution receipt is available.");
    };
    let completed = receipt.started_at.is_some()
        && receipt.finished_at.is_some()
        && receipt.exit_code.is_some()
        && receipt.check_execution_semantics().is_ok()
        && receipt.check_ref == fact.check_ref
        && expected_receipt_id.as_deref() == Some(receipt.receipt_id.as_str())
        && expected_execution.is_none_or(|id| receipt.stage_execution_id.as_str() == id)
        && matches!(
            receipt.status,
            comp::ReceiptStatus::Passed | comp::ReceiptStatus::Failed
        )
        && match receipt.status {
            comp::ReceiptStatus::Passed => fact.status == "passed",
            comp::ReceiptStatus::Failed => fact.status == "failed",
            _ => false,
        };
    if !completed {
        return unverified_check(
            fact,
            "The receipt does not establish a completed check execution.",
        );
    }
    let Some(snapshot) = snapshot else {
        return unverified_check(fact, "The command snapshot is unavailable.");
    };
    if snapshot.profile.check_ref != receipt.check_ref
        || snapshot.profile_sha256 != receipt.profile_sha256
        || snapshot.toolchain_sha256 != receipt.toolchain_sha256
    {
        return unverified_check(
            fact,
            "The receipt does not match its command/tool snapshot.",
        );
    }
    let Some(policy) = snapshot.cargo_policy.as_ref() else {
        // Legacy generic commands have no Cargo interpretation policy. Keep
        // their existing completed passed/failed contract without inventing
        // qualified details for historical rows.
        return fact;
    };
    let Some(details) = details else {
        return unverified_check(fact, "The approved Cargo profile has no execution details.");
    };
    let interpretation = match details.interpretation {
        InterpretationStatus::Supported => "supported",
        InterpretationStatus::Partial => "partial",
        InterpretationStatus::Uninterpreted => "uninterpreted",
        InterpretationStatus::NotApplicable => "not_applicable",
    };
    let result = match details.result {
        CheckResult::Passed => "passed",
        CheckResult::Failed => "failed",
        CheckResult::Unknown => "unknown",
    };
    fact.interpretation
        .get_or_insert_with(|| interpretation.into());
    fact.result.get_or_insert_with(|| result.into());
    for limitation in details.limitations.iter().take(8) {
        let text = bounded_coverage_text(limitation);
        if fact.limitations.len() < 8 && !fact.limitations.contains(&text) {
            fact.limitations.push(text);
        }
    }
    let policy_sha = comp::canonical_sha256(policy, &[]).ok();
    if !details.executed()
        || details.receipt_id != receipt.receipt_id
        || policy_sha.as_ref() != Some(&details.policy_sha256)
        || details.parser_version != policy.parser_version
        || result != fact.status
    {
        return unverified_check(
            fact,
            "Cargo execution evidence is inconclusive or inconsistent.",
        );
    }
    fact
}

fn persisted_coverage_fact(
    db: &Connection,
    row: HostCheckRow,
    expected_execution: Option<&str>,
) -> Result<CheckFact, PersistenceServiceError> {
    let receipt_json: Option<String> = match row.receipt_id.as_deref() {
        Some(id) => db
            .query_row(
                "SELECT receipt_json FROM host_validation_runs WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    let receipt: Option<comp::ValidationReceiptV1> =
        receipt_json.and_then(|json| serde_json::from_str(&json).ok());
    let (snapshot, details) = match receipt.as_ref() {
        Some(receipt) => {
            let snapshot_json: Option<String> = db
                .query_row(
                    "SELECT snapshot_json FROM composition_policy_snapshots WHERE id=?1",
                    [receipt.profile_snapshot_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            let details_json: Option<String> = db
                .query_row(
                    "SELECT details_json FROM host_validation_run_details WHERE receipt_id=?1",
                    [receipt.receipt_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            (
                snapshot_json.and_then(|json| serde_json::from_str(&json).ok()),
                details_json.and_then(|json| serde_json::from_str(&json).ok()),
            )
        }
        None => (None, None),
    };
    Ok(coverage_fact_for_receipt(
        row,
        receipt.as_ref(),
        snapshot.as_ref(),
        details.as_ref(),
        expected_execution,
    ))
}

impl PersistenceService {
    /// Assesses the risk of a composed stage, records it and returns what it
    /// requires. Test and the Integrator keep at least the Code level of the
    /// same input revision. `candidates` are the Integrator's reviewed paths.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assess_composed_risk(
        &self,
        project_id: &str,
        pulsation_id: &str,
        input_revision: u64,
        stage: comp::Stage,
        inputs: &StageInputs,
        profiles: &[ValidationCommandProfileV1],
        specification_required: &HashSet<Handle>,
        candidates: &[String],
    ) -> Result<RiskReview, PersistenceServiceError> {
        let db = self.connection()?;
        let settings = Self::composition_settings_for(&db, project_id)?;
        let policy = RiskPolicyV1::effective(&settings.risk_policy);
        let facts = risk::inputs_from_stage(inputs, candidates);
        let mut assessment = risk::assess(&facts, &policy);
        if stage != comp::Stage::Code {
            let earlier =
                Self::stage_risk_assessment(&db, pulsation_id, comp::Stage::Code, input_revision)?;
            assessment = risk::raise(assessment, earlier.as_ref());
        }
        let selection =
            risk::select_checks(assessment.level, profiles, specification_required, &policy);
        db.execute(
            "INSERT INTO pulsation_risk_assessments(pulsation_id,stage,input_revision,level,policy_version,policy_sha256,assessment_json,selection_json,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(pulsation_id,stage,input_revision) DO UPDATE SET level=excluded.level,policy_version=excluded.policy_version,policy_sha256=excluded.policy_sha256,assessment_json=excluded.assessment_json,selection_json=excluded.selection_json,created_at=excluded.created_at",
            params![
                pulsation_id,
                stage.to_pulsation().as_db_value(),
                input_revision,
                risk_level_name(assessment.level),
                assessment.policy_version,
                assessment.policy_sha256,
                serde_json::to_string(&assessment).map_err(runtime)?,
                serde_json::to_string(&selection).map_err(runtime)?,
                Self::now_iso(),
            ],
        )?;
        Ok(RiskReview {
            assessment,
            selection,
            policy,
        })
    }

    pub(super) fn stage_risk_assessment(
        db: &Connection,
        pulsation_id: &str,
        stage: comp::Stage,
        input_revision: u64,
    ) -> Result<Option<RiskAssessment>, PersistenceServiceError> {
        db.query_row(
            "SELECT assessment_json FROM pulsation_risk_assessments WHERE pulsation_id=?1 AND stage=?2 AND input_revision=?3",
            params![pulsation_id, stage.to_pulsation().as_db_value(), input_revision],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| serde_json::from_str(&json).map_err(runtime))
        .transpose()
    }

    /// ADR-034 M3 (D7): captures every `REVIEW.md` of the repository once,
    /// at the pulsation's first Intake admission. A repository the host
    /// cannot list keeps no capture: the Integrator then reads the guides
    /// live, as ADR-032 did.
    pub(super) fn capture_review_policy_once(
        &self,
        pulsation_id: &str,
        root: &Path,
    ) -> Result<(), PersistenceServiceError> {
        let exists: bool = self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM pulsation_review_policy_snapshots WHERE pulsation_id=?1)",
            [pulsation_id],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(());
        }
        let Ok(repository) = RepositoryContext::new(root) else {
            return Ok(());
        };
        let Ok(paths) = repository.document_paths(true) else {
            return Ok(());
        };
        let capture = review_policy::capture(&paths, |location| {
            if std::fs::symlink_metadata(root.join(location)).is_err() {
                return PolicyRead::Missing;
            }
            match repository.read_document(location) {
                Ok(text) => PolicyRead::Loaded(text),
                Err(_) => PolicyRead::Invalid,
            }
        });
        self.connection()?.execute(
            "INSERT OR IGNORE INTO pulsation_review_policy_snapshots(pulsation_id,state,files_json,omitted_json,content_json,snapshot_sha256,captured_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                pulsation_id,
                capture.state.as_str(),
                serde_json::to_string(&capture.files).map_err(runtime)?,
                serde_json::to_string(&capture.omitted).map_err(runtime)?,
                serde_json::to_string(&capture.contents).map_err(runtime)?,
                capture.sha256(),
                Self::now_iso(),
            ],
        )?;
        Ok(())
    }

    /// The pulsation's Intake capture, with the bytes while it is live.
    pub(super) fn review_policy_capture(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<(PolicyCapture, String)>, PersistenceServiceError> {
        let row = db
            .query_row(
                "SELECT state,files_json,omitted_json,content_json,snapshot_sha256 FROM pulsation_review_policy_snapshots WHERE pulsation_id=?1",
                [pulsation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((state, files, omitted, contents, sha256)) = row else {
            return Ok(None);
        };
        let state = match state.as_str() {
            "captured" => review_policy::CaptureState::Captured,
            "partial" => review_policy::CaptureState::Partial,
            _ => review_policy::CaptureState::Absent,
        };
        let capture = PolicyCapture {
            state,
            files: serde_json::from_str(&files).map_err(runtime)?,
            omitted: serde_json::from_str(&omitted).map_err(runtime)?,
            contents: contents
                .map(|json| serde_json::from_str::<BTreeMap<String, String>>(&json))
                .transpose()
                .map_err(runtime)?
                .unwrap_or_default(),
        };
        Ok(Some((capture, sha256)))
    }

    /// Derives and records the coverage matrix of an Integrator execution
    /// from its kept verdicts and the host's own facts. No model text enters.
    pub(super) fn record_coverage(
        &self,
        execution_id: &str,
        pulsation_id: &str,
        input_revision: u64,
        review: &ReviewBoundary,
        responses: &[&UnitModelResponseV1],
        own_checks: &[comp::CheckSummaryV1],
    ) -> Result<(), PersistenceServiceError> {
        let manifest = &review.manifest;
        let Some(selection) = manifest.risk_selection.as_ref() else {
            return Ok(());
        };
        let level = manifest
            .risk
            .as_ref()
            .map(|risk| risk.level)
            .unwrap_or(comp::Risk::Unknown);
        let limiting = manifest
            .gaps
            .iter()
            .filter(|gap| gap.limits_verdicts())
            .collect::<Vec<_>>();
        let global = limiting.iter().any(|gap| gap.scope == GapScope::Global);
        let mut verdicts = Vec::new();
        for response in responses {
            let UnitModelResponseV1::Completed {
                payload: CompletedPayloadV1::Integration(integration),
                ..
            } = response
            else {
                continue;
            };
            for assessment in &integration.assessments {
                let downgraded = assessment.verdict == comp::Verdict::Unverified
                    && (global
                        || limiting
                            .iter()
                            .any(|gap| gap.affected_criteria.contains(&assessment.criterion_ref)));
                verdicts.push(VerdictFact {
                    criterion: assessment.criterion_ref.clone(),
                    verdict: assessment.verdict,
                    downgraded,
                });
            }
        }
        // The latest stage's receipt of each check: Test supersedes Code.
        let mut checks = BTreeMap::<Handle, CheckFact>::new();
        let mut rows = manifest.checks.clone();
        rows.sort_by_key(|row| row.stage);
        let db = self.connection()?;
        for row in rows {
            checks.insert(
                row.check_ref.clone(),
                persisted_coverage_fact(&db, row, None)?,
            );
        }
        // The Integrator's own checks ran on the reviewed candidate just now.
        for summary in own_checks {
            let status = check_status_name(summary.status);
            checks.insert(
                summary.check_ref.clone(),
                persisted_coverage_fact(
                    &db,
                    HostCheckRow {
                        stage: comp::Stage::Integrator,
                        check_ref: summary.check_ref.clone(),
                        status: status.to_owned(),
                        freshness: if matches!(status, "passed" | "failed") {
                            Freshness::Fresh
                        } else {
                            Freshness::NotApplicable
                        },
                        reason: None,
                        receipt_id: summary.receipt_id.as_ref().map(|id| id.as_str().to_owned()),
                        interpretation: None,
                        result: None,
                        limitations: Vec::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        diagnostics: 0,
                    },
                    Some(execution_id),
                )?,
            );
        }
        let checks = checks.into_values().collect::<Vec<_>>();
        let matrix = risk::coverage_matrix(
            level,
            &verdicts,
            &review.criteria.checks,
            &checks,
            selection,
        );
        self.connection()?.execute(
            "INSERT INTO integrator_coverage(stage_execution_id,pulsation_id,input_revision,level,matrix_json,pending,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(stage_execution_id) DO UPDATE SET level=excluded.level,matrix_json=excluded.matrix_json,pending=excluded.pending,created_at=excluded.created_at",
            params![
                execution_id,
                pulsation_id,
                input_revision,
                risk_level_name(level),
                serde_json::to_string(&matrix).map_err(runtime)?,
                matrix.pending() as i64,
                Self::now_iso(),
            ],
        )?;
        Ok(())
    }

    /// The coverage of the accepted Integrator execution of the pulsation's
    /// current input revision.
    fn current_coverage(
        db: &Connection,
        pulsation_id: &str,
        input_revision: u64,
    ) -> Result<Option<CoverageMatrix>, PersistenceServiceError> {
        db.query_row(
            "SELECT c.matrix_json FROM integrator_coverage c JOIN stage_executions e ON e.id=c.stage_execution_id
             WHERE c.pulsation_id=?1 AND c.input_revision=?2 AND e.status='accepted'
             ORDER BY c.created_at DESC,c.rowid DESC LIMIT 1",
            params![pulsation_id, input_revision],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|json| serde_json::from_str(&json).map_err(runtime))
        .transpose()
    }

    /// Risk and coverage of a pulsation's current input revision, for the
    /// Integrator checkpoint and the report.
    pub fn checkpoint_risk_status(
        &self,
        pulsation_id: &str,
    ) -> Result<CheckpointRiskStatus, PersistenceServiceError> {
        let db = self.connection()?;
        let Some((input_revision, version)) = db
            .query_row(
                "SELECT input_revision,pipeline_version FROM pulsations WHERE id=?1",
                [pulsation_id],
                |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u8>(1)?)),
            )
            .optional()?
        else {
            return Err(PersistenceServiceError::NotFound(
                "Pulsation not found.".into(),
            ));
        };
        if !PipelineVersion::from_db(version).is_some_and(PipelineVersion::supports_composition) {
            return Ok(CheckpointRiskStatus::default());
        }
        let assessment = Self::stage_risk_assessment(
            &db,
            pulsation_id,
            comp::Stage::Integrator,
            input_revision,
        )?;
        let matrix = Self::current_coverage(&db, pulsation_id, input_revision)?;
        let acknowledgements = db
            .prepare(
                "SELECT level,pending,reason,created_at FROM pulsation_risk_acknowledgements WHERE pulsation_id=?1 AND input_revision=?2 ORDER BY created_at,rowid",
            )?
            .query_map(params![pulsation_id, input_revision], |row| {
                Ok(RiskAcknowledgementRecord {
                    level: row.get(0)?,
                    pending: row.get(1)?,
                    reason: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let waiting_integrator: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM checkpoints WHERE pulsation_id=?1 AND status='waiting' AND purpose='artifact_review' AND stage='Integrator' AND input_revision=?2)",
            params![pulsation_id, input_revision],
            |row| row.get(0),
        )?;
        Ok(CheckpointRiskStatus {
            applies: true,
            acknowledgement_required: waiting_integrator
                && matrix
                    .as_ref()
                    .is_some_and(CoverageMatrix::needs_acknowledgement),
            assessment,
            matrix,
            acknowledgements,
        })
    }

    /// Admits an Approve at the Integrator checkpoint: a high-risk review
    /// with unverified or unrun items needs an acknowledgement with a reason,
    /// recorded with the checkpoint. Any other checkpoint ignores it.
    pub(super) fn admit_risk_acknowledgement(
        tx: &Connection,
        pulsation_id: &str,
        checkpoint_id: &str,
        stage: PulsationStage,
        acknowledgement: Option<&RiskAcknowledgement>,
    ) -> Result<(), PersistenceServiceError> {
        if stage != PulsationStage::Integrator {
            return Ok(());
        }
        let input_revision: u64 = tx.query_row(
            "SELECT input_revision FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| row.get(0),
        )?;
        let Some(matrix) = Self::current_coverage(tx, pulsation_id, input_revision)? else {
            return Ok(());
        };
        if !matrix.needs_acknowledgement() {
            return Ok(());
        }
        let Some(acknowledgement) = acknowledgement else {
            return Err(PersistenceServiceError::Validation(format!(
                "risk_acknowledgement_required: this high-risk review has {} unverified or unrun item(s); approving it needs an acknowledgement with a reason.",
                matrix.pending()
            )));
        };
        let reason = acknowledgement.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_ACKNOWLEDGEMENT_CHARS {
            return Err(PersistenceServiceError::Validation(format!(
                "The acknowledgement needs a reason of 1 to {MAX_ACKNOWLEDGEMENT_CHARS} characters."
            )));
        }
        tx.execute(
            "INSERT INTO pulsation_risk_acknowledgements(id,pulsation_id,checkpoint_id,input_revision,level,pending,reason,created_at)
             VALUES(?1,?2,?3,?4,'high',?5,?6,?7)",
            params![
                Uuid::new_v4().to_string(),
                pulsation_id,
                checkpoint_id,
                input_revision,
                matrix.pending() as i64,
                reason,
                Self::now_iso(),
            ],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod cargo_coverage_tests {
    use super::*;
    use crate::orchestration::SchemaV1;
    use crate::orchestration::composition::validation::CargoValidationPolicyV1;
    use crate::orchestration::composition::validation::evidence::{
        PreparationEvidence, ToolVersion, ValidationPhase,
    };
    use crate::orchestration::composition::validation::profile::ProfileOrigin;

    fn fixture() -> (
        HostCheckRow,
        comp::ValidationReceiptV1,
        ValidationProfileSnapshot,
        RunEvidenceDetailsV1,
    ) {
        let policy = CargoValidationPolicyV1::default();
        let profile = ValidationCommandProfileV1 {
            schema_version: SchemaV1,
            profile_id: comp::Uuid::generate(),
            profile_version: 2,
            check_ref: Handle::new("cargo_test").unwrap(),
            kind: comp::CheckKind::Test,
            required: true,
            executable: "cargo".into(),
            argv: vec!["test".into()],
            cwd_relative_path: ".".into(),
            input_globs: vec!["**/*".into()],
            environment_names: Vec::new(),
            toolchain_identity: "cargo".into(),
            timeout_seconds: 900,
            max_log_bytes: 1024,
        };
        let snapshot = ValidationProfileSnapshot {
            profile_sha256: comp::canonical_sha256(&profile, &[]).unwrap(),
            profile,
            origin: ProfileOrigin::Preset,
            resolved_executable: Some("fixture/cargo".into()),
            resolved_executable_sha256: Some(comp::Sha256Hex::of(b"executable")),
            toolchain_sha256: comp::Sha256Hex::of(b"toolchain"),
            cargo_policy: Some(policy.clone()),
            cargo_toolchain: None,
            cargo_unavailable_reason: None,
            effective_environment: Vec::new(),
        };
        let empty_log = || comp::LogExcerptV1 {
            text: String::new(),
            truncated: false,
        };
        let receipt = comp::ValidationReceiptV1 {
            schema_version: SchemaV1,
            receipt_id: comp::Uuid::generate(),
            stage_execution_id: comp::Uuid::generate(),
            parent_generation: 1,
            check_ref: snapshot.profile.check_ref.clone(),
            profile_snapshot_id: comp::Uuid::generate(),
            profile_sha256: snapshot.profile_sha256.clone(),
            candidate_sha256: comp::Sha256Hex::of(b"candidate"),
            input_manifest_sha256: comp::Sha256Hex::of(b"inputs"),
            toolchain_sha256: snapshot.toolchain_sha256.clone(),
            environment_sha256: comp::Sha256Hex::of(b"environment"),
            status: comp::ReceiptStatus::Passed,
            exit_code: Some(0),
            started_at: Some(comp::Timestamp::now()),
            finished_at: Some(comp::Timestamp::now()),
            stdout: empty_log(),
            stderr: empty_log(),
            diagnostics: Vec::new(),
            baseline_receipt_id: None,
        };
        let row = HostCheckRow {
            stage: comp::Stage::Test,
            check_ref: receipt.check_ref.clone(),
            status: "passed".into(),
            freshness: Freshness::Fresh,
            reason: None,
            receipt_id: Some(receipt.receipt_id.as_str().into()),
            interpretation: None,
            result: None,
            limitations: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            diagnostics: 0,
        };
        let details = RunEvidenceDetailsV1 {
            schema_version: SchemaV1,
            receipt_id: receipt.receipt_id.clone(),
            policy_sha256: comp::canonical_sha256(&policy, &[]).unwrap(),
            parser_version: policy.parser_version,
            phase: ValidationPhase::Execution,
            interpretation: InterpretationStatus::Supported,
            result: CheckResult::Passed,
            candidate_integrity: true,
            preparation: Some(PreparationEvidence {
                status: comp::ReceiptStatus::Passed,
                exit_code: Some(0),
                started_at: receipt.started_at.clone(),
                finished_at: receipt.finished_at.clone(),
                stdout: empty_log(),
                stderr: empty_log(),
            }),
            tool_versions: ["cargo", "rustc", "rustdoc"]
                .into_iter()
                .map(|tool| ToolVersion {
                    tool: tool.into(),
                    version: Some(format!("{tool} fixture")),
                })
                .collect(),
            tests_passed: Some(1),
            tests_failed: Some(0),
            tests_ignored: Some(0),
            limitations: Vec::new(),
        };
        (row, receipt, snapshot, details)
    }

    #[test]
    fn a_completed_supported_cargo_failure_remains_executed_without_passing_its_gate() {
        let (mut row, mut receipt, snapshot, mut details) = fixture();
        row.status = "failed".into();
        receipt.status = comp::ReceiptStatus::Failed;
        receipt.exit_code = Some(101);
        details.result = CheckResult::Failed;
        assert!(details.executed());
        assert!(!details.qualifies());
        let fact = coverage_fact_for_receipt(
            row,
            Some(&receipt),
            Some(&snapshot),
            Some(&details),
            Some(receipt.stage_execution_id.as_str()),
        );
        assert_eq!(fact.status, "failed");
        assert_eq!(fact.fresh, Some(true));
        assert_eq!(fact.interpretation.as_deref(), Some("supported"));
        assert_eq!(fact.result.as_deref(), Some("failed"));
    }

    #[test]
    fn approved_cargo_coverage_refuses_missing_inconclusive_or_inconsistent_evidence() {
        for defect in [
            "missing_details",
            "missing_snapshot",
            "partial",
            "unknown",
            "integrity",
            "preparation",
            "tools",
            "receipt_binding",
            "row_receipt_binding",
            "policy_binding",
            "parser",
            "phase",
            "not_started",
            "not_finished",
            "not_exited",
            "wrong_execution",
            "profile_binding",
            "tool_binding",
            "error",
        ] {
            let (mut row, mut receipt, mut snapshot, mut details) = fixture();
            let mut expected = receipt.stage_execution_id.as_str().to_owned();
            match defect {
                "partial" => details.interpretation = InterpretationStatus::Partial,
                "unknown" => details.result = CheckResult::Unknown,
                "integrity" => details.candidate_integrity = false,
                "preparation" => details.preparation = None,
                "tools" => details.tool_versions[0].version = None,
                "receipt_binding" => details.receipt_id = comp::Uuid::generate(),
                "row_receipt_binding" => {
                    row.receipt_id = Some(comp::Uuid::generate().as_str().into())
                }
                "policy_binding" => details.policy_sha256 = comp::Sha256Hex::of(b"other policy"),
                "parser" => details.parser_version += 1,
                "phase" => details.phase = ValidationPhase::Preparation,
                "not_started" => receipt.started_at = None,
                "not_finished" => receipt.finished_at = None,
                "not_exited" => receipt.exit_code = None,
                "wrong_execution" => expected = comp::Uuid::generate().as_str().into(),
                "profile_binding" => {
                    snapshot.profile_sha256 = comp::Sha256Hex::of(b"changed command")
                }
                "tool_binding" => snapshot.toolchain_sha256 = comp::Sha256Hex::of(b"changed tools"),
                "error" => {
                    receipt.status = comp::ReceiptStatus::Error;
                    row.status = "error".into();
                }
                _ => {}
            }
            let fact = coverage_fact_for_receipt(
                row,
                Some(&receipt),
                (defect != "missing_snapshot").then_some(&snapshot),
                (defect != "missing_details").then_some(&details),
                Some(&expected),
            );
            assert_eq!(fact.fresh, Some(false), "{defect}");
            assert_eq!(fact.result.as_deref(), Some("unknown"), "{defect}");
            assert!(!fact.limitations.is_empty(), "{defect}");
        }
    }

    #[test]
    fn legacy_commands_keep_completed_failure_coverage_but_need_actual_process_evidence() {
        let (mut row, mut receipt, mut snapshot, _) = fixture();
        snapshot.cargo_policy = None;
        row.status = "failed".into();
        receipt.status = comp::ReceiptStatus::Failed;
        receipt.exit_code = Some(1);
        let legacy =
            coverage_fact_for_receipt(row.clone(), Some(&receipt), Some(&snapshot), None, None);
        assert_eq!(legacy.fresh, Some(true));
        assert!(legacy.interpretation.is_none() && legacy.result.is_none());
        receipt.started_at = None;
        let no_process =
            coverage_fact_for_receipt(row, Some(&receipt), Some(&snapshot), None, None);
        assert_eq!(no_process.fresh, Some(false));
        assert_eq!(no_process.result.as_deref(), Some("unknown"));
    }
}
