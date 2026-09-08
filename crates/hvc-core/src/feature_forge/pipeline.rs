//! Frozen batch manifests and host-validated task coverage for the six stages.
use super::{service::*, *};
use crate::persistence_service::PersistenceServiceError as Error;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::HashSet;

pub fn manifest(db: &Connection, pulse: &str) -> Result<Option<BatchManifest>, Error> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT manifest_json,manifest_sha256 FROM pulsation_batches WHERE pulsation_id=?1",
            [pulse],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(value, hash)| {
        if crate::orchestration::ContentSha256::of(value.as_bytes()).as_str() != hash {
            return Err(invalid("Batch manifest integrity check failed"));
        }
        decode(&value)
    })
    .transpose()
}
pub fn extend_schema(schema: &mut Value) {
    schema["required"]
        .as_array_mut()
        .expect("stage schema")
        .push(json!("taskEvidence"));
    schema["properties"]["taskEvidence"] = json!({"type":"array","maxItems":100,"items":{"type":"object","additionalProperties":false,"required":["taskId","criterionIds","artifactPositions","summary"],"properties":{"taskId":{"type":"string"},"criterionIds":{"type":"array","items":{"type":"string"}},"artifactPositions":{"type":"array","items":{"type":"integer","minimum":0}},"summary":{"type":"string","minLength":1}}}});
}
/// Distinct from `invalid()`: every check `accept_evidence` performs reads
/// the model's own `taskEvidence` output, so a rejection here is the model's
/// fault, not the host's - and worth a corrective retry. `manifest()` and
/// `complete_batch()` keep using `invalid()`/`Validation` for their own
/// host-side integrity/workflow checks.
fn model_output_invalid(error: impl std::fmt::Display) -> Error {
    Error::ModelOutputInvalid(error.to_string())
}
pub fn accept_evidence(
    db: &Connection,
    pulse: &str,
    job: &str,
    generation: u64,
    stage: &str,
    output: &Value,
) -> Result<(), Error> {
    let Some(manifest) = manifest(db, pulse)? else {
        return Ok(());
    };
    let evidence: Vec<TaskEvidence> =
        serde_json::from_value(output["taskEvidence"].clone()).map_err(model_output_invalid)?;
    if matches!(
        output["statusUpdate"].as_str(),
        Some("needs_clarification" | "failed")
    ) {
        if !evidence.is_empty() {
            return Err(model_output_invalid(
                "Incomplete stage results cannot claim task evidence",
            ));
        }
        if output["statusUpdate"] == "needs_clarification" {
            for question in output["questions"].as_array().into_iter().flatten() {
                let question = question["question"].as_str().unwrap_or("");
                if !manifest.tasks.iter().any(|t| question.contains(&t.id)) {
                    return Err(model_output_invalid(
                        "Batch clarification must name an affected task ID",
                    ));
                }
            }
        }
        return Ok(());
    }
    if evidence.len() != manifest.tasks.len() {
        return Err(model_output_invalid(
            "Stage must account for every task exactly once",
        ));
    }
    let artifacts = output["artifacts"]
        .as_array()
        .ok_or_else(|| model_output_invalid("Missing artifacts"))?;
    let mut seen = HashSet::new();
    for entry in &evidence {
        let task = manifest
            .tasks
            .iter()
            .find(|t| t.id == entry.task_id)
            .ok_or_else(|| model_output_invalid("Evidence references an unknown task"))?;
        if !seen.insert(&entry.task_id) {
            return Err(model_output_invalid("Duplicate task evidence"));
        }
        crate::agent_output_language::validate_english_text("task evidence", &entry.summary)
            .map_err(model_output_invalid)?;
        if entry.artifact_positions.is_empty() {
            return Err(model_output_invalid(format!(
                "Task evidence for `{}` has an empty artifactPositions; every task needs at least one artifact.",
                entry.task_id
            )));
        }
        if let Some(out_of_range) = entry
            .artifact_positions
            .iter()
            .find(|position| **position >= artifacts.len())
        {
            return Err(model_output_invalid(format!(
                "Task evidence for `{}` references artifactPositions {:?}, but this response returned only {} artifact{}; index {out_of_range} does not exist. artifactPositions count is independent of criterionIds count - do not pad it to match.",
                entry.task_id,
                entry.artifact_positions,
                artifacts.len(),
                if artifacts.len() == 1 { "" } else { "s" },
            )));
        }
        let criteria = entry.criterion_ids.iter().collect::<HashSet<_>>();
        if criteria.len() != entry.criterion_ids.len()
            || criteria
                .iter()
                .any(|c| !task.draft.criteria.iter().any(|known| &known.id == *c))
        {
            return Err(model_output_invalid(
                "Unknown or duplicate acceptance criterion",
            ));
        }
        if matches!(stage, "Test" | "Integrator") && criteria.len() != task.draft.criteria.len() {
            return Err(model_output_invalid(
                "Test and Integrator must cover all task acceptance criteria",
            ));
        }
        db.execute("INSERT INTO task_evidence(pulsation_id,job_id,generation,stage,task_id,evidence_json) VALUES(?1,?2,?3,?4,?5,?6)",params![pulse,job,generation,stage,entry.task_id,service::json(entry)?])?;
    }
    Ok(())
}
pub(crate) fn complete_batch(db: &Connection, pulse: &str) -> Result<(), Error> {
    let Some(manifest) = manifest(db, pulse)? else {
        return Ok(());
    };
    for task in &manifest.tasks {
        let covered:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM task_evidence e JOIN checkpoints c ON c.producer_job_id=e.job_id AND c.generation=e.generation WHERE e.pulsation_id=?1 AND e.task_id=?2 AND e.stage='Integrator' AND c.status='approved' AND c.id=(SELECT integration_checkpoint_id FROM pulsations WHERE id=?1))",params![pulse,task.id],|r|r.get(0))?;
        if !covered {
            return Err(invalid("Task lacks approved integration evidence"));
        }
        if db.execute("UPDATE work_tasks SET state='done',completed_pulsation_id=?2,active_pulsation_id=NULL,updated_at=?4 WHERE id=?1 AND active_pulsation_id=?2 AND revision=?3",params![task.id,pulse,task.revision,now()])?!=1 { return Err(invalid("Task reservation changed during integration")); }
    }
    for feature in &manifest.features {
        db.execute("UPDATE forge_features SET state=CASE WHEN EXISTS(SELECT 1 FROM work_tasks WHERE feature_id=?1 AND active_pulsation_id IS NOT NULL) THEN 'executing' WHEN EXISTS(SELECT 1 FROM work_tasks WHERE feature_id=?1 AND feature_revision=?2 AND state<>'done') THEN 'planned' ELSE 'ready_for_review' END,updated_at=?3 WHERE id=?1 AND revision=?2",params![feature.id,feature.revision,now()])?;
    }
    Ok(())
}
