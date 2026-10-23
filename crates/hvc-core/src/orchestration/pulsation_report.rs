//! The end-of-pulsation HTML report on demand (ADR-027 §5, M6).
//!
//! The run context comes from the same telemetry analysis as
//! `analyze-pulsation`. The outcome comes from the latest Integrator
//! checkpoint: its typed payload briefed exactly as Review briefs it, the host
//! checks of every stage at that input revision with their receipts, and the
//! files that revision publishes. Nothing is parsed back from Markdown, and a
//! pulsation that never reached the Integrator gets a telemetry-only report
//! that says so.
use std::collections::BTreeMap;

use super::{
    Connection, OptionalExtension, PathBuf, PersistenceService, PersistenceServiceError,
    coordination, serial,
};
use crate::orchestration::PublicationState;
use crate::orchestration::composition::CheckSummaryV1;
use crate::orchestration::review_brief::{self, StageVisual};
use crate::reports::{
    self, FileChange, LineCounts, PulsationReport, ReportCheck, ReportCriterion, ReportFile,
    ReportOutcome, analysis,
};
use coordination::invalid;

impl PersistenceService {
    /// The typed input of a pulsation's report. Refused until a checkpoint
    /// opened at the Integrator, unless the pulsation already ended.
    pub fn build_pulsation_report(
        &self,
        pulsation_id: &str,
    ) -> Result<PulsationReport, PersistenceServiceError> {
        let pulse = Self::normalize_pulsation_id(pulsation_id)?;
        let outcome = {
            let mut db = self.connection()?;
            let tx = db.transaction()?;
            let (status, stage, integrator): (String, String, Option<String>) = tx
                .query_row(
                    "SELECT p.status,p.stage,(SELECT c.id FROM checkpoints c WHERE c.pulsation_id=p.id
                       AND c.purpose='artifact_review' AND c.stage='Integrator'
                       ORDER BY c.created_at DESC,c.rowid DESC LIMIT 1)
                     FROM pulsations p WHERE p.id=?1",
                    [&pulse],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?
                .ok_or_else(|| PersistenceServiceError::NotFound("Pulsation not found.".into()))?;
            let ended = matches!(status.as_str(), "done" | "failed");
            // A stage failure parked at the Integrator has no payload to
            // brief yet; its report is telemetry only.
            let parked_at_integrator = stage == "Integrator" && status == "checkpoint";
            match integrator {
                Some(checkpoint) => report_outcome(&tx, &pulse, &checkpoint)?,
                None if ended || parked_at_integrator => None,
                None => {
                    return Err(invalid(
                        "The pulsation report is available once the Integrator checkpoint opens or the pulsation ends.",
                    ));
                }
            }
        };
        let telemetry =
            analysis::analyze(self.database_path(), &analysis::Selector::Id(pulse.clone()))
                .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
        let mut report = analysis::to_html_report(&telemetry)
            .map_err(|error| PersistenceServiceError::Runtime(error.to_string()))?;
        report.outcome = outcome;
        Ok(report)
    }

    /// Renders the report and replaces `reports/<pulsation id>/pulsation-report.html`
    /// under the data directory. Returns the written path.
    pub fn write_pulsation_report(
        &self,
        pulsation_id: &str,
    ) -> Result<PathBuf, PersistenceServiceError> {
        let report = self.build_pulsation_report(pulsation_id)?;
        let html = reports::render_pulsation_report(&report);
        let data_dir = self
            .database_path()
            .parent()
            .ok_or_else(|| invalid("Database has no parent directory."))?;
        reports::store::write_report(data_dir, &report.pulsation.id, &html)
    }
}

/// Verdicts and blockers from the Integrator payload, with the checks and
/// files of its input revision. `None` when the stage keeps no typed payload
/// (pipelines before v4).
fn report_outcome(
    db: &Connection,
    pulsation_id: &str,
    checkpoint_id: &str,
) -> Result<Option<ReportOutcome>, PersistenceServiceError> {
    // The latest Integrator output, also once its review was resolved.
    let Some(inputs) = serial::brief_inputs(db, checkpoint_id, pulsation_id, false)? else {
        return Ok(None);
    };
    let Some(brief) = review_brief::stage_brief(&inputs.record, &inputs.context())
        .map_err(|error| invalid(error.to_string()))?
    else {
        return Ok(None);
    };
    let StageVisual::Integrator(visual) = brief.visual else {
        return Ok(None);
    };
    Ok(Some(ReportOutcome {
        criteria: visual
            .verdicts
            .into_iter()
            .map(|verdict| ReportCriterion {
                criterion_id: verdict.criterion_id,
                description: verdict.description,
                task_title: verdict.task_title,
                verdict: verdict.verdict,
                explanation: verdict.explanation,
                check_refs: verdict.checks,
            })
            .collect(),
        blockers: visual.blockers,
        checks: report_checks(db, checkpoint_id)?,
        files: report_files(db, checkpoint_id)?,
    }))
}

/// Every host check that ran for a stage of the Integrator's input revision,
/// with its latest receipt: a later stage's run of the same check replaces an
/// earlier one. The Integrator's own execution usually runs none; its
/// criteria cite the checks of Code and Test.
fn report_checks(
    db: &Connection,
    checkpoint_id: &str,
) -> Result<Vec<ReportCheck>, PersistenceServiceError> {
    let reviews = db
        .prepare(
            "SELECT r.id FROM checkpoints r JOIN checkpoints c ON c.pulsation_id=r.pulsation_id
               AND c.input_revision=r.input_revision
             WHERE c.id=?1 AND r.purpose='artifact_review'
             ORDER BY r.created_at,r.rowid",
        )?
        .query_map([checkpoint_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut order: Vec<String> = Vec::new();
    let mut latest: BTreeMap<String, CheckSummaryV1> = BTreeMap::new();
    for review in reviews {
        for check in serial::checkpoint_checks(db, &review)? {
            let check_ref = check.check_ref.to_string();
            if !latest.contains_key(&check_ref) {
                order.push(check_ref.clone());
            }
            latest.insert(check_ref, check);
        }
    }
    order
        .into_iter()
        .filter_map(|check_ref| latest.remove(&check_ref))
        .map(|summary| report_check(db, summary))
        .collect()
}

/// The `host_validation_runs` row behind a receipt, with its profile snapshot.
struct ReceiptRun {
    exit_code: Option<i32>,
    started: Option<String>,
    finished: Option<String>,
    snapshot: Option<String>,
}

/// A check summary with what its receipt and profile snapshot record: the
/// exact command, its exit code and how long it ran.
fn report_check(
    db: &Connection,
    summary: CheckSummaryV1,
) -> Result<ReportCheck, PersistenceServiceError> {
    let mut check = ReportCheck {
        check_ref: summary.check_ref.to_string(),
        required: summary.required,
        status: summary.status,
        command: None,
        exit_code: None,
        duration_seconds: None,
        receipt_id: summary.receipt_id.as_ref().map(ToString::to_string),
    };
    let Some(receipt) = check.receipt_id.clone() else {
        return Ok(check);
    };
    let run = db
        .query_row(
            "SELECT r.exit_code,r.started_at,r.finished_at,s.snapshot_json FROM host_validation_runs r
             LEFT JOIN composition_policy_snapshots s ON s.id=r.profile_snapshot_id
             WHERE r.id=?1",
            [&receipt],
            |row| {
                Ok(ReceiptRun {
                    exit_code: row.get(0)?,
                    started: row.get(1)?,
                    finished: row.get(2)?,
                    snapshot: row.get(3)?,
                })
            },
        )
        .optional()?;
    let Some(ReceiptRun {
        exit_code,
        started,
        finished,
        snapshot,
    }) = run
    else {
        return Ok(check);
    };
    check.exit_code = exit_code;
    check.duration_seconds = match (started, finished) {
        (Some(started), Some(finished)) => {
            match (
                chrono::DateTime::parse_from_rfc3339(&started),
                chrono::DateTime::parse_from_rfc3339(&finished),
            ) {
                (Ok(started), Ok(finished)) => {
                    Some((finished - started).num_milliseconds().max(0) as f64 / 1000.0)
                }
                _ => None,
            }
        }
        _ => None,
    };
    if let Some(profile) = snapshot
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .map(|snapshot| snapshot["profile"].clone())
    {
        if let Some(required) = profile["required"].as_bool() {
            check.required = required;
        }
        if let Some(executable) = profile["executable"].as_str() {
            let mut words = vec![shell_word(executable)];
            words.extend(
                profile["argv"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(shell_word),
            );
            check.command = Some(words.join(" "));
        }
    }
    Ok(check)
}

/// One command word as a reader would type it: quoted when it holds a space
/// or a quote.
fn shell_word(word: &str) -> String {
    if !word.is_empty() && !word.contains([' ', '\t', '"']) {
        word.to_owned()
    } else {
        format!("\"{}\"", word.replace('"', "\\\""))
    }
}

/// The files the Integrator's input revision publishes, one row per path.
/// A path is new when its first publication expected no file there; then
/// every line of its latest content is added. The host keeps no copy of a
/// modified file's previous version, so those carry no line counts.
fn report_files(
    db: &Connection,
    checkpoint_id: &str,
) -> Result<Vec<ReportFile>, PersistenceServiceError> {
    let rows = db
        .prepare(
            "SELECT a.relative_path,a.publication_state,a.content,
                    ap.artifact_id IS NOT NULL,ap.expected_sha256 IS NULL
             FROM artifacts a
             JOIN checkpoints c ON c.pulsation_id=a.pulsation_id AND c.input_revision=a.input_revision
             LEFT JOIN artifact_publications ap ON ap.artifact_id=a.id
             WHERE c.id=?1 AND a.acceptance IN ('accepted','candidate')
               AND a.relative_path IS NOT NULL AND a.publication_state<>'not_required'
             ORDER BY a.relative_path,a.created_at,a.id",
        )?
        .query_map([checkpoint_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut files: Vec<ReportFile> = Vec::new();
    for (path, state, content, scheduled, expected_absent) in rows {
        let publication = match state.as_str() {
            "pending" => PublicationState::Pending,
            "applying" => PublicationState::Applying,
            "applied" => PublicationState::Applied,
            "conflict" => PublicationState::Conflict,
            _ => continue,
        };
        let lines = content.map(|content| LineCounts {
            added: u32::try_from(content.lines().count()).unwrap_or(u32::MAX),
            removed: 0,
        });
        match files.last_mut().filter(|file| file.path == path) {
            // A later stage of the same revision wrote the path again: the
            // change against the workspace stays the first one.
            Some(file) => {
                file.publication = publication;
                if file.change == FileChange::Added {
                    file.lines = lines;
                }
            }
            None => {
                // Every v4 publication records what it expected at the path.
                let change = if scheduled && expected_absent {
                    FileChange::Added
                } else {
                    FileChange::Modified
                };
                files.push(ReportFile {
                    lines: lines.filter(|_| change == FileChange::Added),
                    path,
                    change,
                    publication,
                });
            }
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::shell_word;

    #[test]
    fn command_words_are_quoted_only_when_needed() {
        assert_eq!(shell_word("cargo"), "cargo");
        assert_eq!(
            shell_word(r"C:\Program Files\tool.exe"),
            r#""C:\Program Files\tool.exe""#
        );
        assert_eq!(shell_word("say \"hi\""), r#""say \"hi\"""#);
        assert_eq!(shell_word(""), "\"\"");
    }
}
