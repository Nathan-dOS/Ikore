//! The trust layers of a pulsation, read from the host's rows (ADR-035 M2).
//!
//! Every fact is read on the connection the caller passes, inside the
//! caller's transaction: the detail snapshot reads the layers next to the
//! composed execution, and a second connection opened here could wait for a
//! writer that waits for this read (rollback journal). The rules are pure, in
//! [`crate::orchestration::layers`].
//!
//! Included into `persistence_service` so it can use the service's private
//! helpers.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    Connection, OptionalExtension, PersistenceService, PersistenceServiceError, PulsationStage,
    params,
};
use crate::orchestration::PipelineVersion;
use crate::orchestration::composition::policy::{ToolPolicy, policy_for};
use crate::orchestration::composition::recipes::{STAGE_PAYLOAD_KIND, StagePayloadRecord};
use crate::orchestration::composition::risk::RiskPolicyV1;
use crate::orchestration::composition::{
    self as comp, CheckSummaryV1, CompletedPayloadV1, ExecutionStatus, ExecutorKind,
    UnitModelResponseV1, VerificationMode, WorkUnitSpecV1,
};
use crate::orchestration::layers::{
    ApprovalFact, BudgetFact, CoverageRowFact, CriteriaFact, CriterionVerdict, HostCheckFact,
    LayerInputs, MandateFact, PoolFact, PublicationFact, PulsationLayersSnapshot, RefusalFact,
    RiskFact, RoundsFact, RunFact, derive_layers,
};

fn runtime(error: impl std::fmt::Display) -> PersistenceServiceError {
    PersistenceServiceError::Runtime(error.to_string())
}

const fn tool_policy_name(policy: ToolPolicy) -> &'static str {
    match policy {
        ToolPolicy::ReadOnlyScoped => "read_only_scoped",
        ToolPolicy::HostContextOnly => "host_context_only",
    }
}

fn path_key(path: &str) -> String {
    path.trim().replace('\\', "/").to_ascii_lowercase()
}

/// The stage payload a stage left for the current input revision, newest
/// first among the given acceptances.
fn stage_payload(
    db: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
    input_revision: u64,
    acceptances: &str,
) -> Result<Option<StagePayloadRecord>, PersistenceServiceError> {
    let content: Option<String> = db
        .query_row(
            &format!(
                "SELECT content FROM artifacts WHERE pulsation_id=?1 AND producer_stage=?2 AND kind=?3 AND input_revision=?4 AND acceptance IN ({acceptances}) ORDER BY created_at DESC,rowid DESC LIMIT 1"
            ),
            params![pulsation_id, stage.as_db_value(), STAGE_PAYLOAD_KIND, input_revision],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    content
        .map(|content| crate::orchestration::review_brief::decode_record(&content).map_err(runtime))
        .transpose()
}

impl PersistenceService {
    /// The trust layers of a pulsation (ADR-035 M2), for the report and
    /// explicit callers. The detail snapshot reads them in its own
    /// transaction instead.
    pub fn pulsation_layers(
        &self,
        pulsation_id: &str,
    ) -> Result<PulsationLayersSnapshot, PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction()?;
        let snapshot = Self::pulsation_layers_on(&tx, pulsation_id, None)?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// The layers read on `db`, inside the caller's transaction, as the
    /// detail snapshot reads them. `checks` are the host checks of the latest
    /// composed execution when the caller already read them.
    pub fn pulsation_layers_on(
        db: &Connection,
        pulsation_id: &str,
        checks: Option<&[CheckSummaryV1]>,
    ) -> Result<PulsationLayersSnapshot, PersistenceServiceError> {
        Ok(derive_layers(&Self::layer_inputs(
            db,
            pulsation_id,
            checks,
        )?))
    }

    fn layer_inputs(
        db: &Connection,
        pulsation_id: &str,
        checks: Option<&[CheckSummaryV1]>,
    ) -> Result<LayerInputs, PersistenceServiceError> {
        let (project_id, version, stage, input_revision): (String, u8, String, u64) = db
            .query_row(
                "SELECT project_id,pipeline_version,stage,input_revision FROM pulsations WHERE id=?1",
                [pulsation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?
            .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".into()))?;
        let composed =
            PipelineVersion::from_db(version).is_some_and(PipelineVersion::supports_composition);
        let stage = PulsationStage::from_db_value(&stage)
            .ok_or_else(|| runtime(format!("Unknown stage `{stage}`.")))?;
        let mut inputs = LayerInputs {
            composed,
            past_specification: Self::mandate_governs(stage),
            ..LayerInputs::default()
        };
        let settings = Self::composition_settings_for(db, &project_id)?;
        let policy = RiskPolicyV1::effective(&settings.risk_policy);

        // Identity: who ran, which models, which mandate, which decisions.
        let mut granted = Vec::new();
        match Self::current_mandate(db, pulsation_id) {
            Ok(Some((mandate, _))) => {
                granted = mandate
                    .scope
                    .grants
                    .iter()
                    .map(|grant| grant.path.clone())
                    .collect();
                let transport_fallback: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM execution_mandates WHERE pulsation_id=?1 AND cause='transport_fallback')",
                    [pulsation_id],
                    |row| row.get(0),
                )?;
                inputs.mandate = Some(MandateFact {
                    revision: mandate.revision,
                    cause: mandate.cause,
                    granted: mandate.scope.grants.len(),
                    transport_fallback,
                });
            }
            Ok(None) => {}
            Err(PersistenceServiceError::Runtime(message))
                if message.starts_with("mandate_integrity_failed") =>
            {
                inputs.mandate_integrity_failed = true;
            }
            Err(error) => return Err(error),
        }
        inputs.runs = if composed {
            Self::composed_runs(db, pulsation_id)?
        } else {
            Self::legacy_runs(db, pulsation_id)?
        };
        inputs.approvals = db
            .prepare(
                "SELECT created_at,COALESCE(json_extract(receipt_json,'$.stage'),''),purpose,decision FROM checkpoint_approval_receipts WHERE pulsation_id=?1",
            )?
            .query_map([pulsation_id], |row| {
                Ok(ApprovalFact {
                    decided_at: row.get(0)?,
                    stage: row.get(1)?,
                    purpose: row.get(2)?,
                    decision: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Scope: the latest publication of each path, and the refusals.
        let mut latest = BTreeMap::<String, (String, String)>::new();
        for row in db
            .prepare(
                "SELECT relative_path,state FROM artifact_publications WHERE pulsation_id=?1 AND job_class='main' ORDER BY updated_at,rowid",
            )?
            .query_map([pulsation_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
        {
            let (path, state) = row?;
            latest.insert(path_key(&path), (path, state));
        }
        // One glob match over granted and written paths together.
        let sensitive = policy
            .sensitive_paths(
                granted
                    .iter()
                    .map(String::as_str)
                    .chain(latest.values().map(|(path, _)| path.as_str())),
            )
            .into_iter()
            .collect::<BTreeSet<_>>();
        inputs.sensitive_granted = granted
            .into_iter()
            .filter(|path| sensitive.contains(&path_key(path)))
            .collect();
        inputs.publications = latest
            .into_iter()
            .map(|(key, (path, state))| PublicationFact {
                sensitive: sensitive.contains(&key),
                path,
                state,
            })
            .collect();
        inputs.refusals = db
            .prepare(
                "SELECT rule,relative_path,occurrences FROM mandate_enforcement_events WHERE pulsation_id=?1",
            )?
            .query_map([pulsation_id], |row| {
                Ok(RefusalFact {
                    rule: row.get(0)?,
                    path: row.get(1)?,
                    occurrences: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        inputs.ownership_violations = db.query_row(
            "SELECT COUNT(*) FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1 AND a.error_code='ownership_violation'",
            [pulsation_id],
            |row| row.get(0),
        )?;

        // Guardrails: policies, the latest execution's budget and checks,
        // usage, redactions and revision rounds.
        let executors = db
            .prepare(
                "SELECT DISTINCT a.executor_kind FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id WHERE e.pulsation_id=?1",
            )?
            .query_map([pulsation_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        inputs.tool_policies = executors
            .into_iter()
            .filter_map(|kind| {
                ExecutorKind::parse(&kind)
                    .map(|parsed| (kind, tool_policy_name(policy_for(parsed).tools).to_owned()))
            })
            .collect();
        if composed
            && let Some(execution) = Self::latest_execution_for_pulsation(db, pulsation_id, None)?
        {
            let ledger = Self::load_ledger(db, &execution.budget_account_id)?;
            let remaining = ledger.remaining();
            // A unit whose bounded repair or retry ran out parks the stage
            // with a budget failure class; the unit keeps its own error code.
            let (exhausted, unit_waiting): (bool, bool) = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM stage_failure_requests f JOIN checkpoints c ON c.id=f.checkpoint_id WHERE f.stage_execution_id=?1 AND c.status='waiting' AND f.failure_class IN ('budget_exhausted','retry_budget_exhausted'))
                        OR EXISTS(SELECT 1 FROM stage_work_units WHERE execution_id=?1 AND failure_code='budget_exhausted'),
                        EXISTS(SELECT 1 FROM stage_work_units WHERE execution_id=?1 AND state IN ('pending','failed'))",
                [&execution.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let limits = ledger.limits.clone();
            inputs.budget = Some(BudgetFact {
                stage: execution.stage.as_db_value().to_owned(),
                active: matches!(
                    execution.status,
                    ExecutionStatus::Planned
                        | ExecutionStatus::Running
                        | ExecutionStatus::Validating
                        | ExecutionStatus::Ready
                        | ExecutionStatus::Parked
                ),
                exhausted,
                unit_waiting,
                extra_pools: limits
                    .map(|limits| {
                        vec![
                            PoolFact {
                                name: "invocations".into(),
                                left: remaining.extra_invocations.into(),
                                limit: limits.extra_invocation_limit.into(),
                            },
                            PoolFact {
                                name: "output tokens".into(),
                                left: remaining.extra_output_tokens.into(),
                                limit: limits.extra_output_token_limit.into(),
                            },
                            PoolFact {
                                name: "prompt characters".into(),
                                left: remaining.extra_prompt_characters.into(),
                                limit: limits.extra_prompt_character_limit.into(),
                            },
                        ]
                    })
                    .unwrap_or_default(),
            });
            let summaries = match checks {
                Some(checks) => checks.to_vec(),
                None => Self::execution_checks(db, &execution)?,
            };
            inputs.checks = summaries
                .into_iter()
                .map(|check| HostCheckFact {
                    check_ref: check.check_ref.to_string(),
                    required: check.required,
                    status: check.status,
                })
                .collect();
        }
        for row in db
            .prepare(
                "SELECT quality,COUNT(*) FROM token_usage_records WHERE pulsation_id=?1 AND historical=0 GROUP BY quality",
            )?
            .query_map([pulsation_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
            })?
        {
            let (quality, count) = row?;
            match quality.as_str() {
                "complete" => inputs.usage.complete = count,
                "partial" => inputs.usage.partial = count,
                _ => inputs.usage.unavailable += count,
            }
        }
        let sanitization =
            crate::sanitization::SanitizationStore::summary_on(db, &project_id, Some(pulsation_id))
                .map_err(runtime)?;
        inputs.redactions =
            sanitization.input_redaction_count + sanitization.output_redaction_count;
        if composed {
            let convergence = Self::convergence_status_on(db, pulsation_id)?;
            inputs.rounds = Some(RoundsFact {
                rounds: convergence.rounds,
                threshold: convergence.threshold,
            });
        }

        // Intent: approved criteria, units, risk, coverage and verdicts.
        if composed {
            Self::read_intent(db, pulsation_id, input_revision, &mut inputs)?;
        }
        Ok(inputs)
    }

    fn composed_runs(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Vec<RunFact>, PersistenceServiceError> {
        Ok(db
            .prepare(
                // A rejected answer keeps no model on the attempt; the router
                // and the CLI record what the provider reported anyway.
                "SELECT a.executor_kind,COALESCE(json_extract(s.snapshot_json,'$.transport'),'api'),json_extract(s.snapshot_json,'$.backend'),a.effective_model,
                        COALESCE(a.observed_model,
                          (SELECT o.observed_model FROM llm_model_observations o WHERE o.parent_invocation_id=a.id AND o.observed_model IS NOT NULL ORDER BY o.created_at DESC,o.ordinal DESC LIMIT 1),
                          (SELECT t.observed_model FROM terminal_runs t WHERE t.agent_run_id=a.agent_run_id AND t.observed_model IS NOT NULL ORDER BY t.created_at DESC LIMIT 1)),
                        a.state,
                        (SELECT MAX(t.adapter_contract_version) FROM terminal_runs t WHERE t.agent_run_id=a.agent_run_id)
                 FROM stage_work_unit_attempts a JOIN stage_executions e ON e.id=a.execution_id
                 JOIN composition_policy_snapshots s ON s.id=a.profile_snapshot_id
                 WHERE e.pulsation_id=?1 AND a.state IN ('running','succeeded','rejected')",
            )?
            .query_map([pulsation_id], |row| {
                let state: String = row.get(5)?;
                Ok(RunFact {
                    actor: row.get(0)?,
                    transport: row.get(1)?,
                    provider: row.get(2)?,
                    requested_model: row.get(3)?,
                    observed_model: row.get(4)?,
                    adapter_version: row.get(6)?,
                    completed: state != "running",
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Pipelines v1–v3: their CLI runs and their API usage records.
    fn legacy_runs(
        db: &Connection,
        pulsation_id: &str,
    ) -> Result<Vec<RunFact>, PersistenceServiceError> {
        let mut runs = db
            .prepare(
                "SELECT agent_role,CASE execution_mode WHEN 'cli_process' THEN 'cli' ELSE 'api' END,backend,model,observed_model,adapter_contract_version,status
                 FROM terminal_runs WHERE pulsation_id=?1 AND origin='pipeline' AND status IN ('running','completed','failed')",
            )?
            .query_map([pulsation_id], |row| {
                let status: String = row.get(6)?;
                Ok(RunFact {
                    actor: row.get::<_, Option<String>>(0)?.unwrap_or_else(|| "pipeline".into()),
                    transport: row.get(1)?,
                    provider: row.get(2)?,
                    requested_model: row.get(3)?,
                    observed_model: row.get(4)?,
                    adapter_version: row.get(5)?,
                    completed: status == "completed",
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        runs.extend(
            db.prepare(
                "SELECT COALESCE((SELECT replace(j.kind,'run_stage::','') FROM llm_attempts l JOIN agent_runs r ON r.llm_execution_id=l.execution_id JOIN jobs j ON j.id=r.job_id WHERE l.id=u.source_id LIMIT 1),'api run'),
                        u.provider_id,u.requested_model,u.observed_model
                 FROM token_usage_records u WHERE u.pulsation_id=?1 AND u.source_kind='llm_attempt' AND u.historical=0",
            )?
            .query_map([pulsation_id], |row| {
                Ok(RunFact {
                    actor: row.get(0)?,
                    transport: "api".into(),
                    provider: row.get(1)?,
                    requested_model: row.get(2)?,
                    observed_model: row.get(3)?,
                    adapter_version: None,
                    completed: true,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(runs)
    }

    fn read_intent(
        db: &Connection,
        pulsation_id: &str,
        input_revision: u64,
        inputs: &mut LayerInputs,
    ) -> Result<(), PersistenceServiceError> {
        if let Some(record) = stage_payload(
            db,
            pulsation_id,
            PulsationStage::Specification,
            input_revision,
            "'accepted'",
        )? && let Some(UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Specification(specification),
            ..
        }) = record.primary()
        {
            let criteria = specification
                .requirements
                .iter()
                .map(|requirement| requirement.criterion_ref.to_string())
                .collect::<BTreeSet<_>>();
            let mut assigned = BTreeSet::new();
            let mut validated = BTreeSet::new();
            for row in db
                .prepare(
                    "SELECT u.spec_json,u.state FROM stage_work_units u JOIN stage_executions e ON e.id=u.execution_id
                     WHERE e.pulsation_id=?1 AND e.parent_input_revision=?2 AND e.status NOT IN ('superseded','cancelled')
                       AND u.executor_kind IN ('code_implementer','code_repairer','test_author','test_repairer')",
                )?
                .query_map(params![pulsation_id, input_revision], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
            {
                let (spec, state) = row?;
                let spec: WorkUnitSpecV1 = serde_json::from_str(&spec).map_err(runtime)?;
                for criterion in spec.criterion_refs {
                    let criterion = criterion.to_string();
                    if state == "validated" {
                        validated.insert(criterion.clone());
                    }
                    assigned.insert(criterion);
                }
            }
            inputs.criteria = Some(CriteriaFact {
                total: criteria.len(),
                assigned: assigned.intersection(&criteria).count(),
                validated: validated.intersection(&criteria).count(),
                without_check: specification
                    .requirements
                    .iter()
                    .filter(|requirement| {
                        requirement.verification_mode == VerificationMode::ExecutedCheck
                            && requirement.required_check_refs.is_empty()
                    })
                    .map(|requirement| requirement.criterion_ref.to_string())
                    .collect(),
            });
        }
        for stage in [
            comp::Stage::Integrator,
            comp::Stage::Test,
            comp::Stage::Code,
        ] {
            if let Some(assessment) =
                Self::stage_risk_assessment(db, pulsation_id, stage, input_revision)?
            {
                inputs.risk = Some(RiskFact {
                    level: assessment.level,
                    reasons: assessment
                        .reasons
                        .iter()
                        .map(|reason| reason.describe())
                        .collect(),
                });
                break;
            }
        }
        if let Some(matrix) = Self::current_coverage(db, pulsation_id, input_revision)? {
            inputs.coverage = matrix
                .rows
                .into_iter()
                .map(|row| CoverageRowFact {
                    kind: row.kind,
                    subject: row.subject.to_string(),
                    state: row.state,
                })
                .collect();
        }
        if let Some(record) = stage_payload(
            db,
            pulsation_id,
            PulsationStage::Integrator,
            input_revision,
            "'candidate','accepted'",
        )? {
            for response in record.responses() {
                if let UnitModelResponseV1::Completed {
                    payload: CompletedPayloadV1::Integration(integration),
                    ..
                } = response
                {
                    inputs
                        .verdicts
                        .extend(integration.assessments.iter().map(|assessment| {
                            CriterionVerdict {
                                criterion: assessment.criterion_ref.to_string(),
                                verdict: assessment.verdict,
                            }
                        }));
                }
            }
        }
        inputs.acknowledgements = db.query_row(
            "SELECT COUNT(*) FROM pulsation_risk_acknowledgements WHERE pulsation_id=?1 AND input_revision=?2",
            params![pulsation_id, input_revision],
            |row| row.get(0),
        )?;
        if let Some(manifest) = crate::feature_forge::pipeline::manifest(db, pulsation_id)? {
            for feature in &manifest.features {
                let dismissed: u32 = db.query_row(
                    "SELECT COUNT(*) FROM forge_preflight_findings WHERE feature_id=?1 AND resolution='dismissed'",
                    [&feature.id],
                    |row| row.get(0),
                )?;
                inputs.preflight_dismissals += dismissed;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "layers_persistence_tests.rs"]
mod tests;
