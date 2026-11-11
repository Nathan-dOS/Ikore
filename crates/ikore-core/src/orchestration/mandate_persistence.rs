//! Execution mandates and approval receipts in the host's transactions
//! (ADR-035 M1).
//!
//! The mandate is issued in the transaction that approves the Specification,
//! and the brief's preview derives it from the same rows the same way. Every
//! checkpoint resolution records a receipt in its own transaction. Stage
//! writes, dispatches and budgets of a v4 pulsation are checked against the
//! revision in force; legacy pipelines record the approval without checks.
//!
//! Included into `persistence_service` so it can use the service's private
//! helpers. Everything is derived from durable rows, so a restart keeps it.

use serde::{Deserialize, Serialize};

use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, PulsationStage,
    Uuid, params,
};
use crate::llm::AgentRole;
use crate::orchestration::PipelineVersion;
use crate::orchestration::composition::budget::{
    EXTRA_INVOCATION_LIMIT, PER_UNIT_EXTRA_INVOCATION_LIMIT,
};
use crate::orchestration::composition::recipes::{STAGE_PAYLOAD_KIND, StagePayloadRecord};
use crate::orchestration::composition::risk::RiskPolicyV1;
use crate::orchestration::composition::{
    self as comp, ExecutorKind, ValidationCommandProfileV1, canonical_sha256,
};
use crate::orchestration::mandate::{
    self, AcknowledgementKind, ApprovalReceiptV1, BudgetParameters, CheckCapability, Enforcement,
    ExecutionMandateV1, IssueInputs, IssuedBy, MandateCause, RECEIPT_SCHEMA_VERSION,
    ReceiptAcknowledgement, ReviewedArtifact, StageOutputLimit, TransportCapability, WriteSource,
    WriteVerdict, text_sha256,
};

/// Enforcement rows kept per pulsation; past it only the newest row counts.
const MAX_ENFORCEMENT_EVENTS: i64 = 200;

/// The stages after the Specification whose transport and budget the mandate
/// freezes, each with the executor that resolves the role's transport.
const GOVERNED_STAGES: [(PulsationStage, ExecutorKind); 4] = [
    (PulsationStage::Contract, ExecutorKind::ContractAuthor),
    (PulsationStage::Code, ExecutorKind::CodeImplementer),
    (PulsationStage::Test, ExecutorKind::TestAuthor),
    (
        PulsationStage::Integrator,
        ExecutorKind::IntegrationReviewer,
    ),
];

fn runtime(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(error.to_string())
}

/// One stored revision of a pulsation's mandate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandateRevisionRecord {
    pub revision: u32,
    pub cause: MandateCause,
    pub mandate_sha256: String,
    pub checkpoint_id: Option<String>,
    pub created_at: String,
    pub mandate: ExecutionMandateV1,
}

/// What approving the waiting Specification would grant (ADR-035 M1). The
/// same derivation as the issued revision, read under the checkpoint's
/// snapshot; nothing is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandatePreview {
    pub mandate: ExecutionMandateV1,
    pub mandate_sha256: String,
    /// Granted paths that match a sensitive glob (ADR-034), as the risk
    /// policy normalizes them.
    pub sensitive_paths: Vec<String>,
}

/// One refused write or dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MandateEnforcementEvent {
    pub stage: String,
    pub rule: String,
    pub relative_path: Option<String>,
    pub detail: String,
    pub mandate_revision: u32,
    pub occurrences: u32,
    pub created_at: String,
}

/// The decision a receipt records, gathered by the resolving transaction.
#[derive(Debug, Clone)]
pub(super) struct ReceiptInput<'a> {
    pub checkpoint_id: &'a str,
    pub pulsation_id: &'a str,
    pub purpose: &'a str,
    pub stage: &'a str,
    pub decision: &'a str,
    pub state_version: u64,
    pub reviewed: Vec<ReviewedArtifact>,
    pub confirmed_paths: Vec<String>,
    pub feedback: Option<&'a str>,
}

/// What the host checks before a composed stage's first request.
pub(super) struct AdmissionFacts<'a> {
    pub stage: comp::Stage,
    pub role: AgentRole,
    /// The transport each executor of the stage resolved to.
    pub transports: Vec<(ExecutorKind, &'a str, Option<&'a str>)>,
    pub applicable_checks: &'a [ValidationCommandProfileV1],
}

impl PersistenceService {
    /// Whether the mandate governs `stage`. Only the stages after the
    /// Specification run under it; a revision that sends the pulsation back
    /// to the Specification runs it on the live settings, and its new
    /// approval re-issues the mandate.
    pub(super) fn mandate_governs(stage: PulsationStage) -> bool {
        GOVERNED_STAGES
            .iter()
            .any(|(governed, _)| *governed == stage)
    }

    /// The revision in force, verified against its stored digest.
    pub(crate) fn current_mandate(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<(ExecutionMandateV1, String)>, PersistenceServiceError> {
        let row: Option<(String, String)> = db
            .query_row(
                "SELECT mandate_json,mandate_sha256 FROM execution_mandates WHERE pulsation_id=?1 ORDER BY revision DESC LIMIT 1",
                [pulsation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((json, stored)) = row else {
            return Ok(None);
        };
        let mandate: ExecutionMandateV1 = serde_json::from_str(&json).map_err(runtime)?;
        let actual = mandate.sha256().map_err(runtime)?;
        if actual.as_str() != stored {
            return Err(PersistenceServiceError::Runtime(
                "mandate_integrity_failed: the stored mandate does not match its digest.".into(),
            ));
        }
        Ok(Some((mandate, stored)))
    }

    /// Every revision of a pulsation's mandate, oldest first.
    pub fn pulsation_mandates(
        &self,
        pulsation_id: &str,
    ) -> Result<Vec<MandateRevisionRecord>, PersistenceServiceError> {
        let db = self.connection()?;
        db.prepare(
            "SELECT revision,cause,mandate_sha256,checkpoint_id,created_at,mandate_json FROM execution_mandates WHERE pulsation_id=?1 ORDER BY revision",
        )?
        .query_map([pulsation_id], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?
        .map(|row| {
            let (revision, cause, sha, checkpoint_id, created_at, json) = row?;
            Ok(MandateRevisionRecord {
                revision,
                cause: MandateCause::from_db_value(&cause)
                    .ok_or_else(|| runtime(format!("Unknown mandate cause `{cause}`.")))?,
                mandate_sha256: sha,
                checkpoint_id,
                created_at,
                mandate: serde_json::from_str(&json).map_err(runtime)?,
            })
        })
        .collect()
    }

    /// The receipt of one checkpoint decision.
    pub fn checkpoint_receipt(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<ApprovalReceiptV1>, PersistenceServiceError> {
        self.connection()?
            .query_row(
                "SELECT receipt_json FROM checkpoint_approval_receipts WHERE checkpoint_id=?1",
                [checkpoint_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| serde_json::from_str(&json).map_err(runtime))
            .transpose()
    }

    /// The refused writes and dispatches of a pulsation, oldest first.
    pub fn mandate_enforcement_events(
        &self,
        pulsation_id: &str,
    ) -> Result<Vec<MandateEnforcementEvent>, PersistenceServiceError> {
        Ok(self
            .connection()?
            .prepare(
                "SELECT stage,rule,relative_path,detail,mandate_revision,occurrences,created_at FROM mandate_enforcement_events WHERE pulsation_id=?1 ORDER BY created_at,rowid",
            )?
            .query_map([pulsation_id], |row| {
                Ok(MandateEnforcementEvent {
                    stage: row.get(0)?,
                    rule: row.get(1)?,
                    relative_path: row.get(2)?,
                    detail: row.get(3)?,
                    mandate_revision: row.get(4)?,
                    occurrences: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// The candidate artifacts a decision on the waiting review covers.
    pub(super) fn checkpoint_candidates(
        db: &Connection,
        pulsation_id: &str,
        stage: PulsationStage,
    ) -> Result<Vec<ReviewedArtifact>, PersistenceServiceError> {
        Ok(db
            .prepare(
                "SELECT id,content_sha256 FROM artifacts WHERE pulsation_id=?1 AND producer_stage=?2 AND acceptance='candidate' ORDER BY id",
            )?
            .query_map(params![pulsation_id, stage.as_db_value()], |row| {
                Ok(ReviewedArtifact {
                    artifact_id: row.get(0)?,
                    sha256: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Derives the revision an Approve of the Specification issues. Reads
    /// only, and only on `db`: the preview and the issuing transaction both
    /// call it inside their transaction.
    fn derive_specification_mandate(
        db: &Connection,
        pulsation_id: &str,
        checkpoint_id: &str,
        state_version: u64,
        candidates: &[ReviewedArtifact],
    ) -> Result<ExecutionMandateV1, PersistenceServiceError> {
        let (project_id, title, version, input_revision, stage): (String, String, u8, u64, String) =
            db.query_row(
                "SELECT project_id,feature_name,pipeline_version,input_revision,stage FROM pulsations WHERE id=?1",
                [pulsation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )?;
        let enforced =
            PipelineVersion::from_db(version).is_some_and(PipelineVersion::supports_composition);
        let enforcement = if enforced {
            Enforcement::EnforcedV4
        } else {
            Enforcement::NotAppliedLegacy
        };
        let manifest = crate::feature_forge::pipeline::manifest(db, pulsation_id)?;
        let clarification_paths =
            Self::clarification_history(db, pulsation_id, Some(input_revision))?
                .into_iter()
                .flat_map(|exchange| exchange.answers)
                .flat_map(|answer| {
                    answer
                        .planned_paths
                        .into_iter()
                        .chain(answer.modified_paths)
                })
                .collect::<Vec<_>>();
        let specification: Option<StagePayloadRecord> = if enforced {
            let content: Option<String> = db
                .query_row(
                    "SELECT content FROM artifacts WHERE pulsation_id=?1 AND producer_stage=?2 AND acceptance='candidate' AND kind=?3 ORDER BY output_position,id LIMIT 1",
                    params![
                        pulsation_id,
                        PulsationStage::Specification.as_db_value(),
                        STAGE_PAYLOAD_KIND
                    ],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            let content = content.ok_or_else(|| {
                PersistenceServiceError::Validation(
                    "mandate_derivation_failed: the Specification payload behind this review is missing."
                        .into(),
                )
            })?;
            Some(
                crate::orchestration::review_brief::decode_record(&content).map_err(|error| {
                    PersistenceServiceError::Validation(format!(
                        "mandate_derivation_failed: the Specification payload is unreadable: {error}"
                    ))
                })?,
            )
        } else {
            None
        };
        let review_policy: Option<String> = db
            .query_row(
                "SELECT snapshot_sha256 FROM pulsation_review_policy_snapshots WHERE pulsation_id=?1",
                [pulsation_id],
                |row| row.get(0),
            )
            .optional()?;
        let settings = Self::composition_settings_for(db, &project_id)?;
        let project_sensitive = (!settings.risk_policy.is_empty())
            .then(|| RiskPolicyV1::effective(&settings.risk_policy).sha256());
        let config = Self::runtime_config_from(db)?;
        let mut transports = Vec::new();
        for (governed, kind) in GOVERNED_STAGES {
            let role = AgentRole::for_stage(governed.as_db_value());
            if transports
                .iter()
                .any(|existing: &TransportCapability| existing.role == role.as_str())
            {
                continue;
            }
            let resolved =
                Self::resolve_effective_profile(db, &project_id, pulsation_id, role, kind, &config);
            match resolved {
                Ok(profile) => transports.push(TransportCapability {
                    role: role.as_str().to_owned(),
                    transport: profile.transport,
                    backend: profile.backend,
                }),
                // A legacy pulsation records what it can; a v4 approval needs
                // every governed role's transport.
                Err(error) if enforced => {
                    return Err(PersistenceServiceError::Validation(format!(
                        "mandate_derivation_failed: the transport of role `{}` cannot be resolved: {error}",
                        role.as_str()
                    )));
                }
                Err(_) => {}
            }
        }
        let mut checks = Vec::new();
        for (profile, enabled) in Self::validation_profiles_for(db, &project_id)? {
            if enabled {
                checks.push(check_capability(&profile)?);
            }
        }
        let budget = BudgetParameters {
            stage_output_limits: GOVERNED_STAGES
                .iter()
                .map(|(governed, _)| StageOutputLimit {
                    stage: governed.as_db_value().to_owned(),
                    output_tokens: config
                        .output_budget_for(AgentRole::for_stage(governed.as_db_value())),
                })
                .collect(),
            context_chars_limit: config.context_chars_limit as u64,
            extra_invocation_limit: EXTRA_INVOCATION_LIMIT,
            per_unit_extra_invocation_limit: PER_UNIT_EXTRA_INVOCATION_LIMIT,
            max_in_flight_units: settings.max_in_flight_units,
        };
        let previous = Self::current_mandate(db, pulsation_id)?;
        let accepted = candidates
            .iter()
            .map(|artifact| artifact.sha256.clone())
            .collect::<Vec<_>>();
        mandate::derive_issued(&IssueInputs {
            pulsation_id,
            project_id: &project_id,
            enforcement,
            title: &title,
            issued_by: IssuedBy {
                checkpoint_id: checkpoint_id.to_owned(),
                stage,
                state_version,
                input_revision,
            },
            previous: previous
                .as_ref()
                .map(|(mandate, sha)| (mandate, sha.as_str())),
            manifest: manifest.as_ref(),
            clarification_paths: &clarification_paths,
            specification: specification.as_ref(),
            accepted_artifact_sha256s: &accepted,
            review_policy_snapshot_sha256: review_policy,
            project_sensitive_sha256: project_sensitive,
            transports,
            checks,
            budget,
        })
        .map_err(|error| {
            PersistenceServiceError::Validation(format!("mandate_derivation_failed: {error}"))
        })
    }

    pub(super) fn insert_mandate(
        db: &Connection,
        mandate: &ExecutionMandateV1,
    ) -> Result<(), PersistenceServiceError> {
        let bytes = mandate.canonical_bytes().map_err(runtime)?;
        let sha = mandate.sha256().map_err(runtime)?;
        db.execute(
            "INSERT INTO execution_mandates(id,pulsation_id,revision,cause,enforcement,mandate_json,mandate_sha256,previous_sha256,checkpoint_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                Uuid::new_v4().to_string(),
                mandate.pulsation_id,
                mandate.revision,
                mandate.cause.as_db_value(),
                mandate.enforcement.as_db_value(),
                bytes,
                sha.as_str(),
                mandate.previous_sha256,
                mandate.issued_by.checkpoint_id,
                Self::now_iso()
            ],
        )?;
        Ok(())
    }

    /// Issues the next revision in the transaction that approves the
    /// Specification. A derivation failure refuses the approval.
    pub(super) fn issue_specification_mandate(
        tx: &Connection,
        pulsation_id: &str,
        checkpoint_id: &str,
        state_version: u64,
        candidates: &[ReviewedArtifact],
    ) -> Result<u32, PersistenceServiceError> {
        let mandate = Self::derive_specification_mandate(
            tx,
            pulsation_id,
            checkpoint_id,
            state_version,
            candidates,
        )?;
        Self::insert_mandate(tx, &mandate)?;
        Ok(mandate.revision)
    }

    /// What approving the waiting Specification review would grant, or
    /// `None` when the checkpoint is not one.
    pub fn mandate_preview(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<MandatePreview>, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let row: Option<(String, u64)> = tx
            .query_row(
                "SELECT c.pulsation_id,p.state_version FROM checkpoints c JOIN pulsations p ON p.id=c.pulsation_id
                 WHERE c.id=?1 AND c.status='waiting' AND c.purpose='artifact_review' AND p.status='checkpoint'
                   AND upper(c.stage)='SPECIFICATION' AND c.input_revision=p.input_revision",
                [checkpoint_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((pulsation_id, state_version)) = row else {
            return Ok(None);
        };
        let candidates =
            Self::checkpoint_candidates(&tx, &pulsation_id, PulsationStage::Specification)?;
        let mandate = Self::derive_specification_mandate(
            &tx,
            &pulsation_id,
            checkpoint_id,
            state_version,
            &candidates,
        )?;
        let project_id = mandate.project_id.clone();
        let settings = Self::composition_settings_for(&tx, &project_id)?;
        let sensitive_paths = RiskPolicyV1::effective(&settings.risk_policy)
            .sensitive_paths(mandate.scope.grants.iter().map(|grant| grant.path.as_str()));
        let mandate_sha256 = mandate.sha256().map_err(runtime)?.as_str().to_owned();
        tx.commit()?;
        Ok(Some(MandatePreview {
            mandate,
            mandate_sha256,
            sensitive_paths,
        }))
    }

    /// An approved CLI fallback moved `role` to the API: the mandate in
    /// force, if any, gains the matching revision.
    pub(super) fn amend_mandate_transport(
        tx: &Connection,
        pulsation_id: &str,
        role: AgentRole,
        checkpoint_id: &str,
        stage: PulsationStage,
        state_version: u64,
    ) -> Result<Option<u32>, PersistenceServiceError> {
        let Some((current, sha)) = Self::current_mandate(tx, pulsation_id)? else {
            return Ok(None);
        };
        let input_revision: u64 = tx.query_row(
            "SELECT input_revision FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| row.get(0),
        )?;
        let next = mandate::amend_transport(
            &current,
            &sha,
            TransportCapability {
                role: role.as_str().to_owned(),
                transport: "api".into(),
                backend: None,
            },
            IssuedBy {
                checkpoint_id: checkpoint_id.to_owned(),
                stage: stage.as_db_value().to_owned(),
                state_version,
                input_revision,
            },
        );
        Self::insert_mandate(tx, &next)?;
        Ok(Some(next.revision))
    }

    /// Records what one checkpoint decision covered, in its transaction,
    /// after the decision's own acknowledgements were recorded.
    pub(super) fn record_approval_receipt(
        tx: &Connection,
        input: ReceiptInput<'_>,
    ) -> Result<(), PersistenceServiceError> {
        let input_revision: u64 = tx.query_row(
            "SELECT input_revision FROM checkpoints WHERE id=?1",
            [input.checkpoint_id],
            |row| row.get(0),
        )?;
        let mandate_revision =
            Self::current_mandate(tx, input.pulsation_id)?.map(|(mandate, _)| mandate.revision);
        let mut reviewed = input.reviewed;
        reviewed.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        let mut confirmed_paths = input.confirmed_paths;
        confirmed_paths.sort();
        confirmed_paths.dedup();
        let mut acknowledgements = Vec::new();
        for (kind, table) in [
            (AcknowledgementKind::Risk, "pulsation_risk_acknowledgements"),
            (
                AcknowledgementKind::Convergence,
                "pulsation_convergence_decisions",
            ),
        ] {
            let reason: Option<String> = tx
                .query_row(
                    &format!("SELECT reason FROM {table} WHERE checkpoint_id=?1"),
                    [input.checkpoint_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(reason) = reason {
                acknowledgements.push(ReceiptAcknowledgement {
                    kind,
                    reason_sha256: text_sha256(&reason),
                });
            }
        }
        let receipt = ApprovalReceiptV1 {
            schema_version: RECEIPT_SCHEMA_VERSION,
            checkpoint_id: input.checkpoint_id.to_owned(),
            pulsation_id: input.pulsation_id.to_owned(),
            purpose: input.purpose.to_owned(),
            stage: input.stage.to_owned(),
            decision: input.decision.to_owned(),
            state_version: input.state_version,
            input_revision,
            decided_at: Self::now_iso(),
            reviewed_artifacts: reviewed,
            confirmed_paths,
            mandate_revision,
            acknowledgements,
            feedback_sha256: input
                .feedback
                .filter(|text| !text.trim().is_empty())
                .map(text_sha256),
        };
        let bytes = receipt.canonical_bytes().map_err(runtime)?;
        tx.execute(
            "INSERT INTO checkpoint_approval_receipts(checkpoint_id,pulsation_id,purpose,decision,receipt_json,receipt_sha256,mandate_revision,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                receipt.checkpoint_id,
                receipt.pulsation_id,
                receipt.purpose,
                receipt.decision,
                bytes,
                text_sha256(&bytes),
                receipt.mandate_revision,
                receipt.decided_at
            ],
        )?;
        Ok(())
    }

    /// The refusal of a stage write outside the mandate in force, with the
    /// revision that refused it. `None` when the write is allowed or no
    /// mandate governs it.
    pub(super) fn mandate_write_refusal(
        db: &Connection,
        pulsation_id: &str,
        source: WriteSource,
        relative_path: &str,
    ) -> Result<Option<(String, u32)>, PersistenceServiceError> {
        if !source.checked_by_mandate() {
            return Ok(None);
        }
        let current = Self::current_mandate(db, pulsation_id)?;
        let mandate = current.as_ref().map(|(mandate, _)| mandate);
        Ok(match mandate::check_write(mandate, source, relative_path) {
            WriteVerdict::Refused(reason) => {
                Some((reason, mandate.map_or(1, |mandate| mandate.revision)))
            }
            WriteVerdict::Allowed | WriteVerdict::NotChecked => None,
        })
    }

    /// Keeps a refused write or dispatch for the Scope layer, bounded per
    /// pulsation.
    pub(super) fn record_enforcement_event(
        db: &Connection,
        pulsation_id: &str,
        stage: &str,
        rule: &str,
        relative_path: Option<&str>,
        detail: &str,
        mandate_revision: u32,
    ) -> Result<(), PersistenceServiceError> {
        let now = Self::now_iso();
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM mandate_enforcement_events WHERE pulsation_id=?1",
            [pulsation_id],
            |row| row.get(0),
        )?;
        if count >= MAX_ENFORCEMENT_EVENTS {
            db.execute(
                "UPDATE mandate_enforcement_events SET occurrences=occurrences+1,updated_at=?2 WHERE rowid=(SELECT rowid FROM mandate_enforcement_events WHERE pulsation_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1)",
                params![pulsation_id, now],
            )?;
            return Ok(());
        }
        db.execute(
            "INSERT INTO mandate_enforcement_events(id,pulsation_id,stage,rule,relative_path,detail,mandate_revision,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![
                Uuid::new_v4().to_string(),
                pulsation_id,
                stage,
                rule,
                relative_path,
                detail,
                mandate_revision,
                now
            ],
        )?;
        Ok(())
    }

    /// The budget parameters a v4 pulsation's stages must use from the
    /// Specification approval on; `None` before it and in legacy pipelines.
    pub(super) fn frozen_budget(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Option<BudgetParameters>, PersistenceServiceError> {
        Ok(Self::current_mandate(db, pulsation_id)?
            .filter(|(mandate, _)| mandate.enforcement.is_enforced())
            .map(|(mandate, _)| mandate.budget))
    }

    /// The reason a composed stage may not dispatch under the mandate in
    /// force: a changed executor policy, transport or validation profile.
    pub(super) fn mandate_admission_refusal(
        db: &Connection,
        pulsation_id: &str,
        facts: &AdmissionFacts<'_>,
    ) -> Result<Option<String>, PersistenceServiceError> {
        let Some((mandate, _)) = Self::current_mandate(db, pulsation_id)? else {
            return Ok(None);
        };
        if !mandate.enforcement.is_enforced() {
            return Ok(None);
        }
        for (kind, transport, backend) in &facts.transports {
            if let Err(reason) = mandate::check_executor(&mandate, *kind) {
                return Ok(Some(reason));
            }
            if let Err(reason) =
                mandate::check_transport(&mandate, facts.role.as_str(), transport, *backend)
            {
                return Ok(Some(reason));
            }
        }
        let current = facts
            .applicable_checks
            .iter()
            .map(check_capability)
            .collect::<Result<Vec<_>, _>>()?;
        let stage = facts.stage;
        Ok(mandate::check_checks(
            &mandate,
            |kind| comp::validation::profile::applies_to_stage(kind, stage),
            &current,
        )
        .err())
    }
}

fn check_capability(
    profile: &ValidationCommandProfileV1,
) -> Result<CheckCapability, PersistenceServiceError> {
    Ok(CheckCapability {
        check_ref: profile.check_ref.as_str().to_owned(),
        kind: profile.kind,
        profile_sha256: canonical_sha256(profile, &[])
            .map_err(|error| runtime(error.message))?
            .as_str()
            .to_owned(),
        required: profile.required,
    })
}

#[cfg(test)]
#[path = "mandate_persistence_tests.rs"]
mod tests;
