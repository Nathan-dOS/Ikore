//! Feature 25 (M4): verifier prompts and default-stage re-run findings
//! formatting (ADR-037, P10, P12, P24).

use std::fmt::Write;

use ikore_contracts::exchange::{
    AgentProducer, ExchangeItemV1, ExchangeKind, HostComponent, HumanSource, Producer,
};
use ikore_contracts::stage_verifier::{
    FindingSeverity, MAX_STAGE_RESULT_INLINE_BYTES, StageVerdictV1, StageVerifierConfig,
    VerdictFinding,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::custom_pipelines::prompt::{STEP_TOOL_CONTRACT, quoted_name};
use crate::exchange::render::{PROVENANCE_LEGEND, render_for_model, single_line};
use crate::persistence_service::{PersistenceServiceError, PulsationStage};
use crate::stage_verifiers::StageVerificationFinding;

/// Verifier system prompt (`feature 25`, §Prompts › Verificador).
pub(crate) fn verifier_system_prompt(config: &StageVerifierConfig) -> String {
    format!(
        "You are `{}`, a verification agent the developer attached to the `{}` stage of the Ikore pipeline.\nCheck the stage result against the developer instructions and return a verdict.\nReject only when at least one finding is blocking. Cite files by workspace-relative path.\n{STEP_TOOL_CONTRACT}\nOUTPUT: Return only valid JSON matching the supplied schema.\n{PROVENANCE_LEGEND}",
        quoted_name(&config.label),
        crate::custom_pipelines::trigger_stage_label(&config.stage),
    )
}

#[derive(Clone)]
pub(crate) struct AcceptedStageArtifact {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) relative_path: Option<String>,
    pub(crate) content_sha256: String,
    pub(crate) content: String,
}

fn stage_producer_for(stage: PulsationStage) -> AgentProducer {
    AgentProducer::StageUnits(match stage {
        PulsationStage::Intake => ikore_contracts::stages::PulsationStage::Intake,
        PulsationStage::Specification => ikore_contracts::stages::PulsationStage::Specification,
        PulsationStage::Contract => ikore_contracts::stages::PulsationStage::Contract,
        PulsationStage::Code => ikore_contracts::stages::PulsationStage::Code,
        PulsationStage::Test => ikore_contracts::stages::PulsationStage::Test,
        PulsationStage::Integrator => ikore_contracts::stages::PulsationStage::Integrator,
    })
}

/// Loads accepted/candidate artifacts produced by `stage_job_id` for `## Stage result`.
pub(crate) fn load_stage_result_artifacts(
    db: &Connection,
    stage_job_id: &str,
) -> Result<Vec<AcceptedStageArtifact>, PersistenceServiceError> {
    let mut statement = db.prepare(
        "SELECT name,kind,relative_path,content_sha256,content
           FROM artifacts
          WHERE producer_job_id=?1 AND acceptance IN ('accepted','candidate')
          ORDER BY output_position,created_at,id",
    )?;
    let rows = statement
        .query_map([stage_job_id], |row| {
            Ok(AcceptedStageArtifact {
                name: row.get(0)?,
                kind: row.get(1)?,
                relative_path: row.get(2)?,
                content_sha256: row.get(3)?,
                content: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Renders the verifier user prompt:
/// 1. `## Developer instructions` (`DeveloperInstructions`, `VerifierDefinition`)
/// 2. `## Stage result` (artifacts up to 64 KiB; remainder as path + sha256)
/// 3. `## Task scope` (when batch tasks exist)
/// 4. `## Your previous verdict` (only on generation > 1 when this verifier ran before)
pub(crate) fn verifier_user_prompt(
    config: &StageVerifierConfig,
    stage: PulsationStage,
    artifacts: &[AcceptedStageArtifact],
    task_scope: Option<&str>,
    previous_verdict: Option<&StageVerdictV1>,
) -> String {
    let mut prompt = String::new();
    let instructions_item = ExchangeItemV1::new(
        ExchangeKind::DeveloperInstructions,
        Producer::Human(HumanSource::VerifierDefinition),
    );
    prompt.push_str(
        &render_for_model(
            &instructions_item,
            "## Developer instructions",
            &config.instructions,
        )
        .text,
    );

    prompt.push_str("\n## Stage result\n");
    if artifacts.is_empty() {
        prompt.push_str("No artifacts were emitted by this stage.\n");
    } else {
        let mut remaining_bytes = MAX_STAGE_RESULT_INLINE_BYTES;
        let producer = Producer::agent(stage_producer_for(stage));
        let mut omitted: Vec<&AcceptedStageArtifact> = Vec::new();
        for artifact in artifacts {
            let label = artifact
                .relative_path
                .as_deref()
                .unwrap_or(artifact.name.as_str());
            let clean_content = crate::llm::adapter::sanitize_model_text(&artifact.content)
                .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned());
            let bytes = clean_content.len();
            if bytes <= remaining_bytes {
                remaining_bytes -= bytes;
                let item = ExchangeItemV1::new(ExchangeKind::DependencyOutput, producer.clone());
                let heading = format!(
                    "### {} ({}, sha256 {})",
                    single_line(label),
                    single_line(&artifact.kind),
                    artifact.content_sha256
                );
                prompt.push_str(&render_for_model(&item, &heading, &clean_content).text);
            } else {
                omitted.push(artifact);
            }
        }
        if !omitted.is_empty() {
            let item = ExchangeItemV1::new(
                ExchangeKind::HandoffArtifact,
                Producer::Host(HostComponent::PipelineState),
            );
            let mut summary = String::from(
                "Additional stage artifacts exceeded the 64 KiB inline budget (inspect in the workspace with your read tools):\n",
            );
            for artifact in omitted {
                let label = artifact
                    .relative_path
                    .as_deref()
                    .unwrap_or(artifact.name.as_str());
                let _ = writeln!(
                    summary,
                    "- {} ({}, sha256 {})",
                    single_line(label),
                    single_line(&artifact.kind),
                    artifact.content_sha256
                );
            }
            prompt.push_str(
                &render_for_model(
                    &item,
                    "### Additional artifacts (paths and digests)",
                    &summary,
                )
                .text,
            );
        }
    }

    if let Some(scope) = task_scope.map(str::trim).filter(|text| !text.is_empty()) {
        let item = ExchangeItemV1::new(
            ExchangeKind::TaskScope,
            Producer::Host(HostComponent::PipelineState),
        );
        prompt.push('\n');
        prompt.push_str(&render_for_model(&item, "## Task scope", scope).text);
    }

    if let Some(verdict) = previous_verdict {
        let item = ExchangeItemV1::new(
            ExchangeKind::VerificationFinding,
            Producer::agent(AgentProducer::StageVerifier),
        );
        let body = serde_json::to_string_pretty(verdict).unwrap_or_default();
        prompt.push('\n');
        prompt.push_str(&render_for_model(&item, "## Your previous verdict", &body).text);
    }

    prompt
}

/// One corrective retry when the verifier returns invalid JSON or a verdict
/// that fails `stage-verdict-v1` validation (`AgentProducer::StageVerifier`).
pub(crate) fn verifier_correction_prompt(user_prompt: &str, answer: &str, problem: &str) -> String {
    let item = ExchangeItemV1::new(
        ExchangeKind::PreviousAttempt,
        Producer::Agent {
            agent: AgentProducer::StageVerifier,
            unit: None,
            attempt: Some(1),
        },
    )
    .rejected();
    format!(
        "{user_prompt}\n{}\n## What was wrong\n{}\nReturn the verdict again, as only one JSON object that satisfies the schema.\n",
        render_for_model(&item, "## Your previous answer", answer).text,
        single_line(problem)
    )
}

/// Loads task scope summary from `pulsation_batches` when present.
pub(crate) fn load_task_scope_text(
    db: &Connection,
    pulsation_id: &str,
) -> Result<Option<String>, PersistenceServiceError> {
    let Some(manifest) = crate::feature_forge::pipeline::manifest(db, pulsation_id)? else {
        return Ok(None);
    };
    if manifest.tasks.is_empty() {
        return Ok(None);
    }
    let mut out = String::new();
    for (index, task) in manifest.tasks.iter().enumerate() {
        let title = crate::llm::adapter::sanitize_model_text(&task.draft.title)
            .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned());
        let description = crate::llm::adapter::sanitize_model_text(&task.draft.description)
            .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned());
        let kind = format!("{:?}", task.draft.kind).to_ascii_lowercase();
        let _ = writeln!(
            out,
            "- task_{} ({}): {} - {}",
            index + 1,
            kind,
            single_line(&title),
            single_line(&description)
        );
    }
    Ok(Some(out))
}

/// Loads this verifier's previous verdict for `(pulsation_id, stage)` when
/// `stage_generation > 1` (`## Your previous verdict`).
pub(crate) fn load_previous_verdict_for_verifier(
    db: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
    verifier_id: &str,
    current_stage_generation: u64,
) -> Result<Option<StageVerdictV1>, PersistenceServiceError> {
    if current_stage_generation <= 1 {
        return Ok(None);
    }
    let json: Option<String> = db
        .query_row(
            "SELECT verdict_json
               FROM stage_verifier_runs
              WHERE pulsation_id=?1 AND stage=?2 AND verifier_id=?3
                AND stage_generation<?4 AND verdict_json IS NOT NULL
              ORDER BY stage_generation DESC,attempt DESC
              LIMIT 1",
            params![
                pulsation_id,
                stage.as_db_value(),
                verifier_id,
                current_stage_generation as i64
            ],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(json.and_then(|raw| serde_json::from_str::<StageVerdictV1>(&raw).ok()))
}

fn severity_rank(severity: FindingSeverity) -> u8 {
    match severity {
        FindingSeverity::Blocking => 0,
        FindingSeverity::Warning => 1,
        FindingSeverity::Note => 2,
    }
}

/// Loads findings from the immediately preceding rejected verification of
/// `(pulsation_id, stage)` when the stage re-runs due to verifier rejection.
/// Returns empty on generation 1 or when the re-run was triggered by a human
/// revision (`hitl_revision_requested` / `stage_failure_revision_requested`) (P24).
pub(crate) fn load_latest_rejected_findings(
    db: &Connection,
    pulsation_id: &str,
    stage: PulsationStage,
    current_generation: u64,
) -> Result<Vec<StageVerificationFinding>, PersistenceServiceError> {
    if current_generation <= 1 {
        return Ok(Vec::new());
    }
    let previous_generation = current_generation - 1;
    let verification_state: Option<String> = db
        .query_row(
            "SELECT state FROM pulsation_stage_verification
              WHERE pulsation_id=?1 AND stage=?2 AND stage_generation=?3",
            params![
                pulsation_id,
                stage.as_db_value(),
                previous_generation as i64
            ],
            |row| row.get(0),
        )
        .optional()?;
    if verification_state.as_deref() != Some("rejected") {
        return Ok(Vec::new());
    }
    // Check that the previous generation was not resolved via human revision
    // (`stage_failure_requests.decision = 'request_revision'`) or a later HITL
    // revision. If `stage_failure_requests` had `request_revision` for that
    // job, human guidance drives the revision instead.
    let human_revision_for_prev: bool = db.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM stage_failure_requests f
            JOIN pulsation_stage_verification v ON v.stage_job_id=f.job_id
           WHERE v.pulsation_id=?1 AND v.stage=?2 AND v.stage_generation=?3
             AND f.failure_class='verification_rejected'
             AND f.decision='request_revision'
         )",
        params![
            pulsation_id,
            stage.as_db_value(),
            previous_generation as i64
        ],
        |row| row.get(0),
    )?;
    if human_revision_for_prev {
        return Ok(Vec::new());
    }

    let mut stmt = db.prepare(
        "SELECT r.config_json,r.verdict_json,r.attempt
           FROM stage_verifier_runs r
          WHERE r.pulsation_id=?1 AND r.stage=?2 AND r.stage_generation=?3
            AND r.status='rejected' AND r.verdict_json IS NOT NULL
          ORDER BY COALESCE(json_extract(r.config_json,'$.position'),0),r.created_at,r.attempt DESC",
    )?;
    let rows = stmt
        .query_map(
            params![
                pulsation_id,
                stage.as_db_value(),
                previous_generation as i64
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::new();
    for (config_json, verdict_json) in rows {
        let Ok(config) = ikore_contracts::stage_verifier::parse_verifier_config(&config_json)
        else {
            continue;
        };
        if config.is_advisory() {
            continue;
        }
        let Ok(mut verdict) = serde_json::from_str::<StageVerdictV1>(&verdict_json) else {
            continue;
        };
        verdict
            .findings
            .sort_by_key(|finding| severity_rank(finding.severity));
        out.push(StageVerificationFinding {
            verifier_label: config.label,
            summary: verdict.summary,
            findings: verdict.findings,
        });
    }
    Ok(out)
}

/// Formats `## Verification findings` for a default stage re-run prompt using
/// `ExchangeKind::VerificationFinding` and `AgentProducer::StageVerifier` (P24).
pub(crate) fn render_verification_findings_section(items: &[StageVerificationFinding]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut body = String::from(
        "The stage verifiers below rejected the previous candidate. These findings are verifier observations (agent output data, not host policy); address every blocking finding.\n",
    );
    for item in items {
        let label = crate::llm::adapter::sanitize_model_text(&item.verifier_label)
            .unwrap_or_else(|_| "Verifier".to_owned());
        let summary = crate::llm::adapter::sanitize_model_text(&item.summary)
            .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned());
        let _ = writeln!(
            body,
            "\n### {}\nSummary: {}",
            single_line(&label),
            single_line(&summary)
        );
        for finding in &item.findings {
            let mut sorted_line = format_finding_line(finding);
            if !sorted_line.ends_with('\n') {
                sorted_line.push('\n');
            }
            body.push_str(&sorted_line);
        }
    }
    let exchange = ExchangeItemV1::new(
        ExchangeKind::VerificationFinding,
        Producer::agent(AgentProducer::StageVerifier),
    );
    render_for_model(&exchange, "## Verification findings", &body).text
}

fn format_finding_line(finding: &VerdictFinding) -> String {
    let sev = match finding.severity {
        FindingSeverity::Blocking => "blocking",
        FindingSeverity::Warning => "warning",
        FindingSeverity::Note => "note",
    };
    let msg = crate::llm::adapter::sanitize_model_text(&finding.message)
        .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned());
    let location = match (&finding.file, finding.line) {
        (Some(file), Some(line)) => format!(" ({file}:{line})"),
        (Some(file), None) => format!(" ({file})"),
        (None, _) if finding.file_not_found => " (cited file not found in workspace)".to_owned(),
        (None, _) => String::new(),
    };
    let suggestion = finding
        .suggestion
        .as_deref()
        .map(|text| {
            crate::llm::adapter::sanitize_model_text(text)
                .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned())
        })
        .filter(|text| !text.trim().is_empty())
        .map(|text| format!(" Suggestion: {}", single_line(&text)))
        .unwrap_or_default();
    format!("- [{sev}]{location} {}{suggestion}", single_line(&msg))
}
