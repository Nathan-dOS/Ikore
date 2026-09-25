//! Read-only report of one CLI observation trial (ADR-025).
//!
//! Duration alone did not explain the slow Pulsations that motivated the
//! trial: a remote model's latency is tokens over throughput, and a large
//! share of a Pulsation's wall time was host preparation, discovery probes
//! and human checkpoints (ToDoListWebView, 2026-09-22). Every figure here is
//! derived from rows the runtime already persists - runs, events, token
//! usage, Job Object samples, jobs and checkpoints - so the report never
//! changes what it measures.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{CliObservationReport, CliObservationTrial, TerminalError};

/// How one admitted run ended, for the statistics it may enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOutcome {
    /// The CLI exited and the stage (or Learning) accepted its output.
    Accepted,
    /// The CLI exited normally but the host or the worker rejected the output
    /// (for example `outcome: failed`); kept out of the main mean.
    StageRejected,
    TransportFailed,
    Cancelled,
    /// Still running: a censored observation, never projected into durations.
    Active,
}

/// One admitted run with the timings and usage that explain its duration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliObservationRun {
    pub run_id: String,
    pub pulsation_id: Option<String>,
    pub agent_role: String,
    pub backend: String,
    pub outcome: ObservationOutcome,
    pub requested_model: Option<String>,
    pub observed_model: Option<String>,
    /// The CLI served a different model than the one configured.
    pub model_mismatch: bool,
    /// Every assistant generation has a reported identity compatible with
    /// the requested model. Historical runs without per-generation evidence
    /// remain false and are excluded from Haiku trial success metrics.
    pub model_verified: bool,
    /// Process start to exit.
    pub duration_ms: Option<u64>,
    /// Run record to process start: workspace snapshot and launch checks.
    pub prep_ms: Option<u64>,
    /// Process start to the first stdout frame.
    pub boot_ms: Option<u64>,
    /// Job start (or the previous run of the job) to this run's record:
    /// admission, discovery probes and prompt assembly.
    pub admission_ms: Option<u64>,
    /// Longest wait between two reads of the CLI's pipes.
    pub transport_silence_ms: Option<u64>,
    /// Longest wait between two audited frames.
    pub frame_silence_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    /// Output plus reasoning tokens.
    pub generated_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    /// Generated tokens per second of model time (duration minus boot).
    pub tokens_per_second: Option<f64>,
    pub cpu_ms: Option<u64>,
    pub peak_job_memory_bytes: Option<u64>,
}

/// Accepted-run statistics of one backend and role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliObservationGroup {
    pub backend: String,
    pub agent_role: String,
    pub accepted_runs: u32,
    pub other_runs: u32,
    pub mean_accepted_duration_ms: Option<u64>,
    pub median_accepted_duration_ms: Option<u64>,
    /// Withheld below three accepted runs.
    pub p95_accepted_duration_ms: Option<u64>,
    pub max_accepted_duration_ms: Option<u64>,
    /// Every accepted duration while there are fewer than three, instead of
    /// a percentile.
    pub accepted_durations_ms: Vec<u64>,
    pub mean_tokens_per_second: Option<f64>,
    pub max_transport_silence_ms: Option<u64>,
}

/// Where the critical-path wall time of one Pulsation went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationTimeBreakdown {
    pub pulsation_id: String,
    pub feature_name: String,
    /// Creation to the last stage job's end (Learning excluded).
    pub total_ms: u64,
    pub cli_ms: u64,
    pub prep_ms: u64,
    pub admission_ms: u64,
    /// Resolved checkpoints: clarifications, reviews and failure decisions.
    pub human_wait_ms: u64,
    /// Queueing, persistence and everything else the host did.
    pub other_ms: u64,
}

fn parse(value: Option<&str>) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value?).ok()
}

fn span_ms(from: Option<DateTime<FixedOffset>>, to: Option<DateTime<FixedOffset>>) -> Option<u64> {
    Some((to? - from?).num_milliseconds().max(0) as u64)
}

/// A mismatch uses the same exact provider-aware family rules as execution.
pub fn models_differ(requested: Option<&str>, observed: Option<&str>) -> bool {
    matches!(
        super::model_identity::verify_model(None, requested, observed),
        super::model_identity::ModelVerification::Mismatch
    )
}

fn model_identity_status(
    db: &Connection,
    run_id: &str,
    backend: &str,
    requested_model: Option<&str>,
) -> Result<(bool, bool), TerminalError> {
    let mut statement = db.prepare(
        "SELECT model_id,is_generation FROM terminal_model_observations
         WHERE run_id=?1 ORDER BY ordinal",
    )?;
    let rows = statement
        .query_map([run_id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let generations = rows.into_iter().filter(|(_, is_generation)| *is_generation).collect::<Vec<_>>();
    if generations.is_empty() {
        return Ok((false, false));
    }
    let mut verified = true;
    let mut mismatch = false;
    for (observed, _) in generations {
        match super::model_identity::verify_model(
            Some(backend),
            requested_model,
            observed.as_deref(),
        ) {
            super::model_identity::ModelVerification::Verified => {}
            super::model_identity::ModelVerification::Mismatch => {
                verified = false;
                mismatch = true;
            }
            super::model_identity::ModelVerification::Unverified => verified = false,
        }
    }
    Ok((verified, mismatch))
}

pub fn tokens_per_second(
    generated_tokens: Option<u64>,
    duration_ms: Option<u64>,
    boot_ms: Option<u64>,
) -> Option<f64> {
    let generated = generated_tokens.filter(|tokens| *tokens > 0)?;
    let model_ms = duration_ms?.saturating_sub(boot_ms.unwrap_or(0));
    (model_ms > 0).then(|| generated as f64 * 1000.0 / model_ms as f64)
}

/// Mean, median, nearest-rank P95 (three or more samples) and maximum.
pub fn duration_stats(durations: &[u64]) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    if sorted.is_empty() {
        return (None, None, None, None);
    }
    let mean = sorted.iter().sum::<u64>() / sorted.len() as u64;
    let median = sorted[(sorted.len() - 1) / 2];
    let p95 =
        (sorted.len() >= 3).then(|| sorted[((sorted.len() * 95).div_ceil(100)).saturating_sub(1)]);
    (Some(mean), Some(median), p95, sorted.last().copied())
}

pub fn group_runs(runs: &[CliObservationRun]) -> Vec<CliObservationGroup> {
    let mut groups: BTreeMap<(String, String), Vec<&CliObservationRun>> = BTreeMap::new();
    for run in runs {
        groups
            .entry((run.backend.clone(), run.agent_role.clone()))
            .or_default()
            .push(run);
    }
    groups
        .into_iter()
        .map(|((backend, agent_role), members)| {
            let accepted = members
                .iter()
                .filter(|run| {
                    run.outcome == ObservationOutcome::Accepted
                        && run.model_verified
                        && !run.model_mismatch
                })
                .collect::<Vec<_>>();
            let durations = accepted
                .iter()
                .filter_map(|run| run.duration_ms)
                .collect::<Vec<_>>();
            let (mean, median, p95, max) = duration_stats(&durations);
            let rates = accepted
                .iter()
                .filter_map(|run| run.tokens_per_second)
                .collect::<Vec<_>>();
            let mut individual = durations.clone();
            individual.sort_unstable();
            CliObservationGroup {
                backend,
                agent_role,
                accepted_runs: accepted.len() as u32,
                other_runs: (members.len() - accepted.len()) as u32,
                mean_accepted_duration_ms: mean,
                median_accepted_duration_ms: median,
                p95_accepted_duration_ms: p95,
                max_accepted_duration_ms: max,
                accepted_durations_ms: if individual.len() < 3 {
                    individual
                } else {
                    Vec::new()
                },
                mean_tokens_per_second: (!rates.is_empty())
                    .then(|| rates.iter().sum::<f64>() / rates.len() as f64),
                max_transport_silence_ms: members
                    .iter()
                    .filter_map(|run| run.transport_silence_ms)
                    .max(),
            }
        })
        .collect()
}

fn outcome(
    db: &Connection,
    status: &str,
    agent_run_id: Option<&str>,
    job_id: Option<&str>,
    origin: &str,
) -> Result<ObservationOutcome, TerminalError> {
    match status {
        "completed" => {}
        "failed" | "interrupted" => return Ok(ObservationOutcome::TransportFailed),
        "cancelled" => return Ok(ObservationOutcome::Cancelled),
        _ => return Ok(ObservationOutcome::Active),
    }
    let accepted = if let Some(agent_run_id) = agent_run_id {
        let attempt: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT state,error_code FROM stage_work_unit_attempts WHERE agent_run_id=?1 ORDER BY created_at DESC LIMIT 1",
                [agent_run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match attempt {
            Some((state, error_code)) => state == "succeeded" && error_code.is_none(),
            None => db
                .query_row(
                    "SELECT status FROM agent_runs WHERE id=?1",
                    [agent_run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .is_some_and(|status| status == "done"),
        }
    } else if origin == "learning" {
        job_id.is_some_and(|job_id| {
            db.query_row(
                "SELECT state FROM learning_runs WHERE job_id=?1",
                [job_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some_and(|state| state == "published")
        })
    } else {
        true
    };
    Ok(if accepted {
        ObservationOutcome::Accepted
    } else {
        ObservationOutcome::StageRejected
    })
}

#[derive(Default)]
struct Usage {
    requested_model: Option<String>,
    observed_model: Option<String>,
    input_tokens: Option<u64>,
    generated_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
}

fn usage(db: &Connection, run_id: &str) -> Result<Usage, TerminalError> {
    let mut statement = db.prepare(
        "SELECT requested_model,observed_model,input_tokens,output_tokens,total_tokens,reasoning_tokens,cache_read_tokens
         FROM token_usage_records WHERE source_kind='terminal_run' AND source_id=?1",
    )?;
    let rows = statement
        .query_map([run_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut usage = Usage::default();
    let mut largest_output = -1_i64;
    let add = |slot: &mut Option<u64>, value: Option<i64>| {
        if let Some(value) = value {
            *slot = Some(slot.unwrap_or(0) + value.max(0) as u64);
        }
    };
    for (requested, observed, input, output, total, reasoning, cache_read) in rows {
        if usage.requested_model.is_none() {
            usage.requested_model = requested;
        }
        // Several models may serve one run (a CLI's helper calls); the one
        // that generated most is the model that did the work.
        if output.unwrap_or(0) > largest_output && observed.is_some() {
            largest_output = output.unwrap_or(0);
            usage.observed_model = observed;
        }
        add(&mut usage.input_tokens, input);
        add(&mut usage.reasoning_tokens, reasoning);
        add(&mut usage.cache_read_tokens, cache_read);
        // Providers disagree on whether output includes reasoning; the
        // total minus input is the generated count either way.
        let generated = match (total, input) {
            (Some(total), Some(input)) if total >= input => Some(total - input),
            _ => output.map(|output| output + reasoning.unwrap_or(0)),
        };
        add(&mut usage.generated_tokens, generated);
    }
    Ok(usage)
}

fn admission_ms(
    db: &Connection,
    job_id: Option<&str>,
    created_at: Option<&str>,
) -> Result<Option<u64>, TerminalError> {
    let Some(job_id) = job_id else {
        return Ok(None);
    };
    let job_started: Option<String> = db
        .query_row("SELECT started_at FROM jobs WHERE id=?1", [job_id], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    // Same-format strings from the same writer order correctly.
    let previous_finished: Option<String> = db
        .query_row(
            "SELECT MAX(finished_at) FROM terminal_runs WHERE job_id=?1 AND created_at < ?2",
            params![job_id, created_at],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let since = [
        parse(job_started.as_deref()),
        parse(previous_finished.as_deref()),
    ]
    .into_iter()
    .flatten()
    .max();
    Ok(span_ms(since, parse(created_at)))
}

fn boot_ms(
    db: &Connection,
    run_id: &str,
    started_at: Option<DateTime<FixedOffset>>,
) -> Result<Option<u64>, TerminalError> {
    let first: Option<String> = db
        .query_row(
            "SELECT created_at FROM terminal_events WHERE run_id=?1 AND stream='stdout' ORDER BY sequence LIMIT 1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(span_ms(started_at, parse(first.as_deref())))
}

fn run_rows(db: &Connection, trial_id: &str) -> Result<Vec<CliObservationRun>, TerminalError> {
    let mut statement = db.prepare(
        "SELECT id,pulsation_id,agent_role,backend,status,origin,agent_run_id,job_id,model,
                created_at,started_at,finished_at,max_transport_silence_ms
         FROM terminal_runs WHERE observation_trial_id=?1 ORDER BY created_at",
    )?;
    let rows = statement
        .query_map([trial_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<i64>>(12)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut runs = Vec::with_capacity(rows.len());
    for (
        run_id,
        pulsation_id,
        agent_role,
        backend,
        status,
        origin,
        agent_run_id,
        job_id,
        configured_model,
        created_at,
        started_at,
        finished_at,
        transport_silence,
    ) in rows
    {
        let (created, started, finished) = (
            parse(created_at.as_deref()),
            parse(started_at.as_deref()),
            parse(finished_at.as_deref()),
        );
        let mut outcome = outcome(
            db,
            &status,
            agent_run_id.as_deref(),
            job_id.as_deref(),
            &origin,
        )?;
        let usage = usage(db, &run_id)?;
        let requested_model = usage.requested_model.or(configured_model);
        let (model_verified, generation_mismatch) = model_identity_status(
            db,
            &run_id,
            &backend,
            requested_model.as_deref(),
        )?;
        let model_mismatch = generation_mismatch
            || models_differ(requested_model.as_deref(), usage.observed_model.as_deref());
        if outcome == ObservationOutcome::Accepted && !model_verified {
            outcome = ObservationOutcome::StageRejected;
        }
        let duration_ms = (outcome != ObservationOutcome::Active)
            .then(|| span_ms(started, finished))
            .flatten();
        let boot_ms = boot_ms(db, &run_id, started)?;
        let (cpu_100ns, peak_job_memory): (Option<i64>, Option<i64>) = db.query_row(
            "SELECT (SELECT user_cpu_100ns+kernel_cpu_100ns FROM cli_observation_samples WHERE run_id=?1 ORDER BY elapsed_ms DESC LIMIT 1),
                    (SELECT MAX(peak_job_memory_bytes) FROM cli_observation_samples WHERE run_id=?1)",
            [&run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        runs.push(CliObservationRun {
            frame_silence_ms: super::observation_frame_silence_ms(
                db,
                &run_id,
                started_at.as_deref(),
                finished_at.as_deref(),
            )?,
            admission_ms: admission_ms(db, job_id.as_deref(), created_at.as_deref())?,
            prep_ms: span_ms(created, started),
            tokens_per_second: tokens_per_second(usage.generated_tokens, duration_ms, boot_ms),
            model_mismatch,
            model_verified,
            run_id,
            pulsation_id,
            agent_role,
            backend,
            outcome,
            requested_model,
            observed_model: usage.observed_model,
            duration_ms,
            boot_ms,
            transport_silence_ms: transport_silence.map(|value| value.max(0) as u64),
            input_tokens: usage.input_tokens,
            generated_tokens: usage.generated_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cpu_ms: cpu_100ns.map(|value| value.max(0) as u64 / 10_000),
            peak_job_memory_bytes: peak_job_memory.map(|value| value.max(0) as u64),
        });
    }
    Ok(runs)
}

fn pulsation_breakdown(
    db: &Connection,
    pulsation_id: &str,
) -> Result<Option<PulsationTimeBreakdown>, TerminalError> {
    let pulsation: Option<(String, String, String)> = db
        .query_row(
            "SELECT feature_name,created_at,updated_at FROM pulsations WHERE id=?1",
            [pulsation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((feature_name, created_at, updated_at)) = pulsation else {
        return Ok(None);
    };
    let last_stage_end: Option<String> = db.query_row(
        "SELECT MAX(COALESCE(finished_at,updated_at)) FROM jobs WHERE pulsation_id=?1 AND job_class='main'",
        [pulsation_id],
        |row| row.get(0),
    )?;
    let Some(total_ms) = span_ms(
        parse(Some(&created_at)),
        parse(last_stage_end.as_deref().or(Some(&updated_at))),
    ) else {
        return Ok(None);
    };
    let mut cli_ms = 0;
    let mut prep_ms = 0;
    let mut admission = 0;
    let mut statement = db.prepare(
        "SELECT r.job_id,r.created_at,r.started_at,r.finished_at FROM terminal_runs r
         JOIN jobs j ON j.id=r.job_id
         WHERE r.pulsation_id=?1 AND j.job_class='main' ORDER BY r.created_at",
    )?;
    let runs = statement
        .query_map([pulsation_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (job_id, created_at, started_at, finished_at) in runs {
        let (created, started, finished) = (
            parse(created_at.as_deref()),
            parse(started_at.as_deref()),
            parse(finished_at.as_deref()),
        );
        cli_ms += span_ms(started, finished).unwrap_or(0);
        prep_ms += span_ms(created, started).unwrap_or(0);
        admission += admission_ms(db, job_id.as_deref(), created_at.as_deref())?.unwrap_or(0);
    }
    let mut statement = db.prepare(
        "SELECT created_at,updated_at FROM checkpoints WHERE pulsation_id=?1 AND status<>'waiting'",
    )?;
    let human_wait_ms = statement
        .query_map([pulsation_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|(opened, resolved)| span_ms(parse(Some(&opened)), parse(Some(&resolved))))
        .sum::<u64>();
    Ok(Some(PulsationTimeBreakdown {
        pulsation_id: pulsation_id.to_owned(),
        feature_name,
        total_ms,
        cli_ms,
        prep_ms,
        admission_ms: admission,
        human_wait_ms,
        other_ms: total_ms
            .saturating_sub(cli_ms)
            .saturating_sub(prep_ms)
            .saturating_sub(admission)
            .saturating_sub(human_wait_ms),
    }))
}

pub(super) fn build(
    db: &Connection,
    trial: CliObservationTrial,
) -> Result<CliObservationReport, TerminalError> {
    let runs = run_rows(db, &trial.id)?;
    let count = |outcome: ObservationOutcome| {
        runs.iter().filter(|run| run.outcome == outcome).count() as u32
    };
    let accepted = runs
        .iter()
        .filter(|run| {
            run.outcome == ObservationOutcome::Accepted && run.model_verified && !run.model_mismatch
        })
        .filter_map(|run| run.duration_ms)
        .collect::<Vec<_>>();
    let (mean, median, p95, max) = duration_stats(&accepted);
    let mut pulsation_ids = Vec::new();
    for pulsation_id in runs.iter().filter_map(|run| run.pulsation_id.as_ref()) {
        if !pulsation_ids.contains(pulsation_id) {
            pulsation_ids.push(pulsation_id.clone());
        }
    }
    let mut pulsations = Vec::new();
    for pulsation_id in pulsation_ids {
        if let Some(breakdown) = pulsation_breakdown(db, &pulsation_id)? {
            pulsations.push(breakdown);
        }
    }
    let (peak_process_memory_bytes, peak_job_memory_bytes): (Option<i64>, Option<i64>) = db
        .query_row(
            "SELECT MAX(peak_process_memory_bytes),MAX(peak_job_memory_bytes) FROM cli_observation_samples WHERE run_id IN (SELECT id FROM terminal_runs WHERE observation_trial_id=?1)",
            [&trial.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    Ok(CliObservationReport {
        trial,
        successful_runs: count(ObservationOutcome::Accepted),
        stage_rejected_runs: count(ObservationOutcome::StageRejected),
        failed_runs: count(ObservationOutcome::TransportFailed),
        cancelled_runs: count(ObservationOutcome::Cancelled),
        active_runs: count(ObservationOutcome::Active),
        mean_success_duration_ms: mean,
        median_success_duration_ms: median,
        p95_success_duration_ms: p95,
        max_success_duration_ms: max,
        max_frame_silence_ms: runs.iter().filter_map(|run| run.frame_silence_ms).max(),
        max_transport_silence_ms: runs.iter().filter_map(|run| run.transport_silence_ms).max(),
        peak_process_memory_bytes: peak_process_memory_bytes.map(|v| v.max(0) as u64),
        peak_job_memory_bytes: peak_job_memory_bytes.map(|v| v.max(0) as u64),
        build_profile: if cfg!(debug_assertions) {
            "debug".into()
        } else {
            "release".into()
        },
        groups: group_runs(&runs),
        pulsations,
        runs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(backend: &str, role: &str, outcome: ObservationOutcome, ms: u64) -> CliObservationRun {
        CliObservationRun {
            run_id: format!("{backend}-{role}-{ms}"),
            pulsation_id: Some("p".into()),
            agent_role: role.into(),
            backend: backend.into(),
            outcome,
            requested_model: None,
            observed_model: None,
            model_mismatch: false,
            model_verified: true,
            duration_ms: Some(ms),
            prep_ms: None,
            boot_ms: None,
            admission_ms: None,
            transport_silence_ms: Some(ms / 2),
            frame_silence_ms: None,
            input_tokens: None,
            generated_tokens: None,
            reasoning_tokens: None,
            cache_read_tokens: None,
            tokens_per_second: Some(ms as f64 / 1000.0),
            cpu_ms: None,
            peak_job_memory_bytes: None,
        }
    }

    #[test]
    fn percentiles_are_nearest_rank_and_withheld_below_three_samples() {
        assert_eq!(duration_stats(&[]), (None, None, None, None));
        assert_eq!(
            duration_stats(&[200, 100]),
            (Some(150), Some(100), None, Some(200))
        );
        let twenty = (1..=20).map(|n| n * 1_000).collect::<Vec<_>>();
        let (mean, median, p95, max) = duration_stats(&twenty);
        assert_eq!(mean, Some(10_500));
        assert_eq!(median, Some(10_000));
        assert_eq!(p95, Some(19_000), "nearest rank: ceil(0.95 * 20) = 19th");
        assert_eq!(max, Some(20_000));
    }

    #[test]
    fn groups_keep_rejected_runs_out_of_the_accepted_statistics() {
        let runs = vec![
            run(
                "opencode",
                "task_specifier",
                ObservationOutcome::Accepted,
                200_000,
            ),
            run(
                "opencode",
                "task_specifier",
                ObservationOutcome::Accepted,
                20_000,
            ),
            // Exit 0 with `outcome: failed` - timed separately.
            run(
                "codex",
                "backend_agent",
                ObservationOutcome::StageRejected,
                16_000,
            ),
            run(
                "opencode",
                "task_specifier",
                ObservationOutcome::Active,
                999_000,
            ),
        ];
        let groups = group_runs(&runs);
        assert_eq!(groups.len(), 2);
        let code = &groups[0];
        assert_eq!(
            (code.backend.as_str(), code.accepted_runs, code.other_runs),
            ("codex", 0, 1)
        );
        assert_eq!(code.mean_accepted_duration_ms, None);
        let spec = &groups[1];
        assert_eq!((spec.accepted_runs, spec.other_runs), (2, 1));
        assert_eq!(spec.mean_accepted_duration_ms, Some(110_000));
        assert_eq!(spec.p95_accepted_duration_ms, None);
        assert_eq!(spec.accepted_durations_ms, vec![20_000, 200_000]);
        assert_eq!(spec.mean_tokens_per_second, Some(110.0));
        assert_eq!(spec.max_transport_silence_ms, Some(499_500));
    }

    #[test]
    fn unverified_and_mismatched_runs_never_enter_accepted_trial_statistics() {
        let mut unverified = run("claude", "task_specifier", ObservationOutcome::Accepted, 90_000);
        unverified.model_verified = false;
        let mut mismatched = run("claude", "task_specifier", ObservationOutcome::Accepted, 80_000);
        mismatched.model_verified = false;
        mismatched.model_mismatch = true;
        let group = group_runs(&[unverified, mismatched]).remove(0);
        assert_eq!(group.accepted_runs, 0);
        assert_eq!(group.other_runs, 2);
        assert_eq!(group.mean_accepted_duration_ms, None);
        assert!(group.accepted_durations_ms.is_empty());
    }

    #[test]
    fn throughput_uses_model_time_and_mismatch_ignores_aliases() {
        // 4,891 generated tokens over 216.5 s with a 4.5 s boot: ~23 tok/s,
        // the free Specification model of 2026-09-22.
        let rate = tokens_per_second(Some(4_891), Some(216_500), Some(4_500)).unwrap();
        assert!((rate - 23.07).abs() < 0.01, "{rate}");
        assert_eq!(tokens_per_second(Some(0), Some(1_000), None), None);
        assert_eq!(tokens_per_second(Some(10), Some(500), Some(500)), None);
        assert!(!models_differ(
            Some("haiku"),
            Some("claude-haiku-4-5-20251001")
        ));
        assert!(models_differ(Some("haiku"), Some("claude-sonnet-5")));
        assert!(!models_differ(
            Some("openrouter/nex-agi/nex-n2.5-pro:free"),
            Some("nex-agi/nex-n2.5-pro")
        ));
        assert!(!models_differ(Some("gpt-5.6-luna"), None));
    }
}
