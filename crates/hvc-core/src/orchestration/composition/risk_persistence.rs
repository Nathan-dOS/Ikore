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
    Freshness, GapScope, ReviewBoundary, RiskReview, risk_level_name,
};
use crate::orchestration::composition::review_policy::{self, PolicyCapture, PolicyRead};
use crate::orchestration::composition::risk::{
    self, CheckFact, CoverageMatrix, RiskAssessment, RiskPolicyV1, VerdictFact,
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
        for row in rows {
            checks.insert(
                row.check_ref.clone(),
                CheckFact {
                    check_ref: row.check_ref.clone(),
                    status: row.status.clone(),
                    fresh: match row.freshness {
                        Freshness::Fresh => Some(true),
                        Freshness::Stale | Freshness::Unverifiable => Some(false),
                        Freshness::NotApplicable => None,
                    },
                },
            );
        }
        // The Integrator's own checks ran on the reviewed candidate just now.
        for summary in own_checks {
            let status = check_status_name(summary.status);
            checks.insert(
                summary.check_ref.clone(),
                CheckFact {
                    check_ref: summary.check_ref.clone(),
                    status: status.to_owned(),
                    fresh: (!matches!(status, "not_run" | "unavailable")).then_some(true),
                },
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
