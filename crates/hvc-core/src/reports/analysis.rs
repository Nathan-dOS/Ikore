//! Post-run analysis of a persisted pulsation.
//!
//! The GPUI panels answer "what is happening now"; this answers "where did
//! this run spend its time". It backs `analyze-pulsation` in
//! `hvc-test-support` and the run context of the in-app pulsation report
//! (ADR-027 §5). Without it, a pipeline change can only be argued about,
//! never measured.
//!
//! The database is opened read-only so this never disturbs a running app.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::llm::AgentRole;
use crate::orchestration::catalog::STAGES;
use crate::reports::{
    PulsationReport, RejectedOutput, ReportPlanQuota, ReportPulsation, ReportRun, ReportStage,
    ReportTokens,
};
use chrono::{DateTime, FixedOffset};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

pub type AnalysisError = Box<dyn std::error::Error + Send + Sync>;

/// Which pulsation to analyze.
#[derive(Debug, Clone)]
pub enum Selector {
    Id(String),
    /// The most recently created pulsation, whatever its status.
    Latest,
    /// The most recently created pulsation that reached `done`.
    LatestDone,
}

/// Opens the execution database read-only. A read-write handle would create
/// `-wal`/`-shm` files next to a database another process may own.
pub fn open_readonly(database: &Path) -> Result<Connection, AnalysisError> {
    if !database.exists() {
        return Err(format!("database not found: {}", database.display()).into());
    }
    Ok(Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

fn parse_time(raw: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(raw).ok()
}

fn elapsed_seconds(from: &str, to: &str) -> Option<f64> {
    let (from, to) = (parse_time(from)?, parse_time(to)?);
    Some((to - from).num_milliseconds() as f64 / 1000.0)
}

fn optional_elapsed(from: Option<&String>, to: Option<&String>) -> Option<f64> {
    elapsed_seconds(from?, to?)
}

fn ratio(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| (part / whole * 1000.0).round() / 10.0)
}

/// How an agent run reached its model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    /// A CLI process (`terminal_runs`).
    Cli,
    /// A provider API call made through the router (`llm_attempts`).
    Api,
}

impl Transport {
    /// The `token_usage_records.source_kind` of this transport's usage.
    fn source_kind(self) -> &'static str {
        match self {
            Self::Cli => "terminal_run",
            Self::Api => "llm_attempt",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Api => "api",
        }
    }
}

/// One model call of an agent: a CLI process or a provider API call.
#[derive(Debug, Clone)]
struct AgentRun {
    transport: Transport,
    id: String,
    job_id: Option<String>,
    agent_role: Option<String>,
    backend: Option<String>,
    model: Option<String>,
    observed_model: Option<String>,
    status: String,
    exit_code: Option<i64>,
    error_code: Option<String>,
    termination_reason: Option<String>,
    output_bytes: i64,
    event_count: i64,
    started_at: Option<String>,
    finished_at: Option<String>,
    deadline_at: Option<String>,
    agent_run_id: Option<String>,
}

impl AgentRun {
    fn duration_seconds(&self) -> Option<f64> {
        optional_elapsed(self.started_at.as_ref(), self.finished_at.as_ref())
    }

    fn budget_seconds(&self) -> Option<f64> {
        optional_elapsed(self.started_at.as_ref(), self.deadline_at.as_ref())
    }
}

#[derive(Debug, Clone, Default)]
struct Tokens {
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
    /// Records the provider reported as unusable - real spend that went unmeasured.
    unavailable_records: i64,
    /// Records with an all-zero body, emitted alongside a real one by some backends.
    phantom_records: i64,
}

impl Tokens {
    fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.unavailable_records += other.unavailable_records;
        self.phantom_records += other.phantom_records;
    }

    fn to_json(&self) -> Value {
        json!({
            "input": self.input,
            "output": self.output,
            "reasoning": self.reasoning,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
            "cache_read_ratio_percent": ratio(self.cache_read as f64, self.input as f64),
            "unavailable_records": self.unavailable_records,
            "phantom_records": self.phantom_records,
        })
    }
}

/// Runs of agents outside the six stages: counted in the totals, never in a
/// stage.
#[derive(Default)]
struct BackgroundUsage {
    runs: usize,
    tokens: Tokens,
    cli_seconds: f64,
    api_seconds: f64,
}

impl BackgroundUsage {
    fn add(&mut self, run: &AgentRun, tokens: &HashMap<String, Tokens>) {
        self.runs += 1;
        self.tokens
            .add(&tokens.get(&run.id).cloned().unwrap_or_default());
        let seconds = run.duration_seconds().unwrap_or(0.0);
        match run.transport {
            Transport::Cli => self.cli_seconds += seconds,
            Transport::Api => self.api_seconds += seconds,
        }
    }
}

/// Resolves the pulsation id for the requested selector.
fn resolve_pulsation(
    connection: &Connection,
    selector: &Selector,
) -> Result<String, AnalysisError> {
    let found: Option<String> = match selector {
        Selector::Id(id) => connection
            .query_row("SELECT id FROM pulsations WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .ok(),
        Selector::Latest => connection
            .query_row(
                "SELECT id FROM pulsations ORDER BY created_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok(),
        Selector::LatestDone => connection
            .query_row(
                "SELECT id FROM pulsations WHERE status='done' ORDER BY created_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok(),
    };
    found.ok_or_else(|| format!("no pulsation matched {selector:?}").into())
}

fn load_terminal_runs(
    connection: &Connection,
    pulsation: &str,
) -> Result<Vec<AgentRun>, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT id,job_id,agent_role,backend,model,observed_model,status,exit_code,error_code,termination_reason,output_bytes,event_count,started_at,finished_at,deadline_at,agent_run_id FROM terminal_runs WHERE pulsation_id=?1 ORDER BY created_at",
    )?;
    let runs = statement
        .query_map([pulsation], |row| {
            Ok(AgentRun {
                transport: Transport::Cli,
                id: row.get(0)?,
                job_id: row.get(1)?,
                agent_role: row.get(2)?,
                backend: row.get(3)?,
                model: row.get(4)?,
                observed_model: row.get(5)?,
                status: row.get(6)?,
                exit_code: row.get(7)?,
                error_code: row.get(8)?,
                termination_reason: row.get(9)?,
                output_bytes: row.get(10)?,
                event_count: row.get(11)?,
                started_at: row.get(12)?,
                finished_at: row.get(13)?,
                deadline_at: row.get(14)?,
                agent_run_id: row.get(15)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(runs)
}

/// Provider API calls of the pulsation's agents, as runs. A route execution
/// that a CLI process served is left out: that process is already a run.
fn load_api_attempts(
    connection: &Connection,
    pulsation: &str,
) -> Result<Vec<AgentRun>, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT a.id,e.job_id,e.agent_role,a.provider_id,a.model_id,a.observed_model,a.outcome,a.error_kind,a.started_at,a.finished_at
         FROM llm_attempts a JOIN llm_route_executions e ON e.id=a.execution_id
         WHERE e.pulsation_id=?1
           AND NOT EXISTS(SELECT 1 FROM terminal_runs t WHERE t.llm_execution_id=e.id)
         ORDER BY a.started_at,a.ordinal",
    )?;
    let attempts = statement
        .query_map([pulsation], |row| {
            let outcome: String = row.get(6)?;
            Ok(AgentRun {
                transport: Transport::Api,
                id: row.get(0)?,
                job_id: row.get(1)?,
                agent_role: row.get(2)?,
                backend: row.get(3)?,
                model: row.get(4)?,
                observed_model: row.get(5)?,
                status: if outcome == "success" {
                    "completed"
                } else {
                    "failed"
                }
                .to_owned(),
                exit_code: None,
                error_code: row.get(7)?,
                termination_reason: None,
                output_bytes: 0,
                event_count: 0,
                started_at: row.get(8)?,
                finished_at: row.get(9)?,
                deadline_at: None,
                agent_run_id: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(attempts)
}

/// Token usage keyed by run id. A single CLI run can hold several records:
/// one per model call the CLI made internally.
fn load_tokens(
    connection: &Connection,
    runs: &[AgentRun],
) -> Result<HashMap<String, Tokens>, AnalysisError> {
    let mut by_run: HashMap<String, Tokens> = HashMap::new();
    let mut statement = connection.prepare(
        "SELECT input_tokens,output_tokens,reasoning_tokens,cache_read_tokens,cache_write_tokens,quality FROM token_usage_records WHERE source_kind=?1 AND source_id=?2",
    )?;
    for run in runs {
        let entry = by_run.entry(run.id.clone()).or_default();
        let rows = statement.query_map([run.transport.source_kind(), &run.id], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (input, output, reasoning, cache_read, cache_write, quality) = row?;
            if quality == "unavailable" {
                entry.unavailable_records += 1;
                continue;
            }
            if input.unwrap_or(0) == 0 && output.unwrap_or(0) == 0 {
                entry.phantom_records += 1;
                continue;
            }
            entry.input += input.unwrap_or(0);
            entry.output += output.unwrap_or(0);
            entry.reasoning += reasoning.unwrap_or(0);
            entry.cache_read += cache_read.unwrap_or(0);
            entry.cache_write += cache_write.unwrap_or(0);
        }
    }
    Ok(by_run)
}

/// Agent runs whose output was rejected after the CLI itself succeeded. These
/// runs cost full tokens and produced nothing, so they count as waste even
/// though `terminal_runs.status` says `completed`.
fn load_rejected_agent_runs(
    connection: &Connection,
    pulsation: &str,
) -> Result<HashSet<String>, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT agent_run_id FROM agent_run_outputs WHERE pulsation_id=?1 AND validation_error IS NOT NULL",
    )?;
    let ids = statement
        .query_map([pulsation], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(ids)
}

/// Agent runs of one job in attempt order. Runs recorded before the host
/// persisted `terminal_runs.agent_run_id` carry no link, so the Nth CLI
/// process of a job is matched to the Nth attempt of that same job - the
/// scheduler only ever has one stage call in flight, so the order is exact.
fn load_agent_run_order(
    connection: &Connection,
    pulsation: &str,
) -> Result<HashMap<String, Vec<String>>, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT run.job_id,run.id FROM agent_runs run JOIN jobs job ON job.id=run.job_id WHERE job.pulsation_id=?1 ORDER BY run.created_at",
    )?;
    let mut by_job: HashMap<String, Vec<String>> = HashMap::new();
    for row in statement.query_map([pulsation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (job_id, run_id) = row?;
        by_job.entry(job_id).or_default().push(run_id);
    }
    Ok(by_job)
}

type StageJob = (String, String, String, i64, Option<String>);

/// Stage wall time is bounded by when the next stage's job was enqueued. The
/// job row's own `started_at` only records the last attempt, so it cannot
/// describe a stage that was retried.
fn load_stage_jobs(
    connection: &Connection,
    pulsation: &str,
) -> Result<Vec<StageJob>, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT id,kind,created_at,attempts,last_error FROM jobs WHERE pulsation_id=?1 AND job_class='main' ORDER BY created_at",
    )?;
    let jobs = statement
        .query_map([pulsation], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(jobs)
}

/// Maps every CLI process to the agent run attempt it served, preferring the
/// persisted link and falling back to attempt order within the same job.
fn resolve_agent_runs(
    runs: &[AgentRun],
    order: &HashMap<String, Vec<String>>,
) -> (HashMap<String, String>, usize) {
    let mut resolved = HashMap::new();
    let mut seen_per_job: HashMap<&str, usize> = HashMap::new();
    let mut inferred = 0;
    for run in runs {
        let Some(job_id) = run.job_id.as_deref() else {
            continue;
        };
        let ordinal = seen_per_job.entry(job_id).or_insert(0);
        if let Some(direct) = run.agent_run_id.clone() {
            resolved.insert(run.id.clone(), direct);
        } else if let Some(candidate) = order.get(job_id).and_then(|ids| ids.get(*ordinal)) {
            resolved.insert(run.id.clone(), candidate.clone());
            inferred += 1;
        }
        *ordinal += 1;
    }
    (resolved, inferred)
}

struct StageTotals {
    stages: Vec<Value>,
    tokens: Tokens,
    discarded_tokens: Tokens,
    cli_seconds: f64,
    discarded_cli_seconds: f64,
    api_seconds: f64,
    discarded_api_seconds: f64,
}

fn analyze_stages(
    runs: &[AgentRun],
    tokens: &HashMap<String, Tokens>,
    rejected: &HashSet<String>,
    resolved: &HashMap<String, String>,
    jobs: &[StageJob],
    pulsation_end: &str,
) -> StageTotals {
    let mut totals = StageTotals {
        stages: Vec::new(),
        tokens: Tokens::default(),
        discarded_tokens: Tokens::default(),
        cli_seconds: 0.0,
        discarded_cli_seconds: 0.0,
        api_seconds: 0.0,
        discarded_api_seconds: 0.0,
    };

    for (index, (_stage, name, role)) in STAGES.iter().enumerate() {
        let expected_kind = format!("run_stage::{name}");
        let Some((_, _, created_at, attempts, last_error)) =
            jobs.iter().find(|(_, kind, ..)| kind == &expected_kind)
        else {
            continue;
        };
        // The stage ends when the next main job is enqueued; the last stage
        // ends with the pulsation itself.
        let ends_at = jobs
            .iter()
            .find(|(_, _, other, ..)| other.as_str() > created_at.as_str())
            .map(|(_, _, other, ..)| other.as_str())
            .unwrap_or(pulsation_end);
        let wall_seconds = elapsed_seconds(created_at, ends_at);

        let stage_runs: Vec<&AgentRun> = runs
            .iter()
            .filter(|run| run.agent_role.as_deref() == Some(role.as_str()))
            .collect();

        let mut stage_tokens = Tokens::default();
        let mut discarded_tokens = Tokens::default();
        let mut stage_cli = 0.0;
        let mut discarded_cli = 0.0;
        let mut stage_api = 0.0;
        let mut discarded_api = 0.0;
        let mut attempt_rows = Vec::new();

        for run in &stage_runs {
            let duration = run.duration_seconds().unwrap_or(0.0);
            let run_tokens = tokens.get(&run.id).cloned().unwrap_or_default();
            stage_tokens.add(&run_tokens);
            let output_rejected = resolved
                .get(&run.id)
                .is_some_and(|id| rejected.contains(id));
            let discarded = run.status == "failed" || output_rejected;
            if discarded {
                discarded_tokens.add(&run_tokens);
            }
            match run.transport {
                Transport::Cli => {
                    stage_cli += duration;
                    if discarded {
                        discarded_cli += duration;
                    }
                }
                Transport::Api => {
                    stage_api += duration;
                    if discarded {
                        discarded_api += duration;
                    }
                }
            }
            attempt_rows.push(json!({
                "transport": run.transport.as_str(),
                "started_at": run.started_at,
                "backend": run.backend,
                "requested_model": run.model,
                "observed_model": run.observed_model,
                "model_divergence": model_divergence(run),
                "status": run.status,
                "duration_seconds": duration,
                "budget_seconds": run.budget_seconds(),
                "exit_code": run.exit_code,
                "error_code": run.error_code,
                "termination_reason": run.termination_reason,
                "output_bytes": run.output_bytes,
                "event_count": run.event_count,
                "output_rejected_by_validation": output_rejected,
                "discarded": discarded,
                "tokens": run_tokens.to_json(),
            }));
        }

        totals.tokens.add(&stage_tokens);
        totals.discarded_tokens.add(&discarded_tokens);
        totals.cli_seconds += stage_cli;
        totals.discarded_cli_seconds += discarded_cli;
        totals.api_seconds += stage_api;
        totals.discarded_api_seconds += discarded_api;
        let cli_runs = stage_runs
            .iter()
            .filter(|run| run.transport == Transport::Cli)
            .count();

        totals.stages.push(json!({
            "position": index + 1,
            "stage": name,
            "agent_role": role.as_str(),
            "job_attempts": attempts,
            "job_last_error": last_error,
            "cli_runs": cli_runs,
            "api_calls": stage_runs.len() - cli_runs,
            "wall_seconds": wall_seconds,
            "cli_seconds": stage_cli,
            "cli_seconds_discarded": discarded_cli,
            "cli_waste_percent": ratio(discarded_cli, stage_cli),
            "api_seconds": stage_api,
            "api_seconds_discarded": discarded_api,
            "tokens": stage_tokens.to_json(),
            "tokens_discarded": discarded_tokens.to_json(),
            "attempts": attempt_rows,
        }));
    }

    totals
}

/// A requested model the provider silently replaced is a cost and behaviour
/// surprise, so it is reported rather than left for someone to notice.
fn model_divergence(run: &AgentRun) -> Option<String> {
    let requested = run.model.as_deref()?;
    let observed = run.observed_model.as_deref()?;
    (!observed.contains(requested)).then(|| format!("requested {requested}, observed {observed}"))
}

/// Splits the time a pulsation spent parked at a checkpoint into the part that
/// bought a human decision about the work and the part that only asked a human
/// to restart failed infrastructure.
fn analyze_checkpoint_waits(
    connection: &Connection,
    pulsation: &str,
) -> Result<Value, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT event_type,to_status,to_stage,created_at FROM pulse_events WHERE pulsation_id=?1 ORDER BY created_at,id",
    )?;
    let events = statement
        .query_map([pulsation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut review_seconds = 0.0;
    let mut unblock_seconds = 0.0;
    let mut waits = Vec::new();
    for pair in events.windows(2) {
        let (event_type, to_status, to_stage, opened_at) = &pair[0];
        if to_status.as_deref() != Some("checkpoint") {
            continue;
        }
        let Some(seconds) = elapsed_seconds(opened_at, &pair[1].3) else {
            continue;
        };
        // `checkpoint_ready` and a clarification are the only checkpoints that
        // exist to get a decision about the work; the rest exist because
        // something broke.
        let is_review = event_type == "checkpoint_ready" || event_type == "clarification_requested";
        if is_review {
            review_seconds += seconds;
        } else {
            unblock_seconds += seconds;
        }
        waits.push(json!({
            "stage": to_stage,
            "opened_by": event_type,
            "resolved_by": pair[1].0,
            "seconds": seconds,
            "purpose": if is_review { "review" } else { "unblock_failure" },
        }));
    }

    let mut kinds: BTreeMap<String, i64> = BTreeMap::new();
    let mut checkpoint_statement = connection
        .prepare("SELECT kind,COUNT(*) FROM checkpoints WHERE pulsation_id=?1 GROUP BY kind")?;
    for row in checkpoint_statement.query_map([pulsation], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (kind, count) = row?;
        kinds.insert(kind, count);
    }

    let total = review_seconds + unblock_seconds;
    Ok(json!({
        "total_seconds": total,
        "review_seconds": review_seconds,
        "unblock_failure_seconds": unblock_seconds,
        "unblock_share_percent": ratio(unblock_seconds, total),
        "checkpoints_by_kind": kinds,
        "waits": waits,
    }))
}

fn sanitized_analysis(mut report: Value) -> Result<Value, AnalysisError> {
    let original = canonical_report_json(&report)?;
    crate::llm::adapter::sanitize_json_strings(&mut report)
        .map_err(|_| "Report sanitization is unavailable.")?;
    let sanitized = canonical_report_json(&report)?;
    report
        .as_object_mut()
        .ok_or("The report envelope is not an object.")?
        .insert(
            "sanitization".into(),
            json!({
                "policy": crate::secret_redactor::SANITIZATION_POLICY_VERSION,
                "originalEvidenceSha256": crate::secret_redactor::text_sha256(&original),
                "sanitizedContentSha256": crate::secret_redactor::text_sha256(&sanitized),
                "hashScope": REPORT_HASH_JSON_SCOPE,
            }),
        );
    // Metadata must not push the exported serialized unit over the same cap.
    canonical_report_json(&report)?;
    Ok(report)
}

pub(super) const REPORT_HASH_JSON_SCOPE: &str = "UTF-8 compact JSON with recursively sorted object keys and arrays in original order; hashes cover the report envelope before sanitization metadata, not the exported file bytes.";

pub(super) fn canonical_report_json<T: serde::Serialize>(
    value: &T,
) -> Result<String, AnalysisError> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Object(fields) => {
                let ordered = fields.into_iter().collect::<BTreeMap<_, _>>();
                Value::Object(
                    ordered
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
            value => value,
        }
    }
    let value = serde_json::to_value(value).map_err(|_| "Report envelope serialization failed.")?;
    let text = serde_json::to_string(&sorted(value))
        .map_err(|_| "Report envelope serialization failed.")?;
    if text.len() > crate::secret_redactor::MAX_SANITIZATION_BYTES {
        return Err("Report envelope exceeds the sanitization limit.".into());
    }
    Ok(text)
}

fn analyze_retrieval(connection: &Connection, pulsation: &str) -> Result<Value, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT COUNT(*),SUM(fallback_used),SUM(duration_ms),SUM(considered_total),SUM(delivered_total),SUM(cited_total),COUNT(max_similarity) FROM context_retrieval_runs WHERE pulsation_id=?1",
    )?;
    let (runs, fallback, duration_ms, considered, delivered, cited, with_similarity) = statement
        .query_row([pulsation], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                row.get::<_, i64>(6)?,
            ))
        })?;
    Ok(json!({
        "runs": runs,
        "fallback_used": fallback,
        "fallback_percent": ratio(fallback as f64, runs as f64),
        "runs_with_semantic_similarity": with_similarity,
        "total_duration_ms": duration_ms,
        "considered": considered,
        "delivered": delivered,
        "cited": cited,
        "delivered_never_cited": delivered - cited,
    }))
}

fn analyze_learning(connection: &Connection, pulsation: &str) -> Result<Value, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT state,budget_calls,budget_grants,total_calls,transient_failures,diagnostic FROM learning_runs WHERE pulsation_id=?1 ORDER BY report_version DESC LIMIT 1",
    )?;
    let row = statement
        .query_row([pulsation], |row| {
            Ok(json!({
                "state": row.get::<_, String>(0)?,
                "budget_calls": row.get::<_, i64>(1)?,
                "budget_grants": row.get::<_, i64>(2)?,
                "total_calls": row.get::<_, i64>(3)?,
                "transient_failures": row.get::<_, i64>(4)?,
                "diagnostic": row.get::<_, Option<String>>(5)?,
            }))
        })
        .ok();
    Ok(row.unwrap_or(Value::Null))
}

fn load_validation_errors(
    connection: &Connection,
    pulsation: &str,
) -> Result<Vec<Value>, AnalysisError> {
    let mut statement = connection.prepare(
        "SELECT output.created_at,run.agent_name,LENGTH(output.raw_output),output.validation_error FROM agent_run_outputs output JOIN agent_runs run ON run.id=output.agent_run_id WHERE output.pulsation_id=?1 AND output.validation_error IS NOT NULL ORDER BY output.created_at",
    )?;
    let rows = statement
        .query_map([pulsation], |row| {
            Ok(json!({
                "created_at": row.get::<_, String>(0)?,
                "agent": row.get::<_, String>(1)?,
                "raw_output_bytes": row.get::<_, i64>(2)?,
                "validation_error": row.get::<_, String>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Produces the full report for one pulsation.
pub fn analyze(database: &Path, selector: &Selector) -> Result<Value, AnalysisError> {
    let connection = open_readonly(database)?;
    let pulsation = resolve_pulsation(&connection, selector)?;

    let (feature_name, status, stage, pipeline_version, created_at, updated_at) = connection
        .query_row(
            "SELECT feature_name,status,stage,pipeline_version,created_at,updated_at FROM pulsations WHERE id=?1",
            [&pulsation],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )?;

    let mut runs = load_terminal_runs(&connection, &pulsation)?;
    // Only CLI processes are matched to agent run attempts; API calls follow.
    let cli_runs = runs.len();
    runs.extend(load_api_attempts(&connection, &pulsation)?);
    let tokens_by_run = load_tokens(&connection, &runs)?;
    let rejected = load_rejected_agent_runs(&connection, &pulsation)?;
    let jobs = load_stage_jobs(&connection, &pulsation)?;
    let (resolved, inferred_links) = resolve_agent_runs(
        &runs[..cli_runs],
        &load_agent_run_order(&connection, &pulsation)?,
    );
    let staged = analyze_stages(
        &runs,
        &tokens_by_run,
        &rejected,
        &resolved,
        &jobs,
        &updated_at,
    );

    let wall_seconds = elapsed_seconds(&created_at, &updated_at).unwrap_or(0.0);
    let checkpoints = analyze_checkpoint_waits(&connection, &pulsation)?;
    let checkpoint_seconds = checkpoints["total_seconds"].as_f64().unwrap_or(0.0);

    // The learning job runs on the background lane after the pipeline closes,
    // and the Visualizer beside a checkpoint, so their runs belong in the
    // totals but never in a stage.
    let stage_roles: HashSet<&str> = STAGES.iter().map(|(_, _, role)| role.as_str()).collect();
    let mut learning = BackgroundUsage::default();
    let mut background = BackgroundUsage::default();
    for run in &runs {
        let Some(role) = run.agent_role.as_deref() else {
            continue;
        };
        let usage = if role == AgentRole::LearningAgent.as_str() {
            &mut learning
        } else if !stage_roles.contains(role) {
            &mut background
        } else {
            continue;
        };
        usage.add(run, &tokens_by_run);
    }
    let learning_usage = learning;
    let learning = analyze_learning(&connection, &pulsation)?;
    // A learning report that never published is spend with no product, even
    // though the pulsation itself is `done`.
    let learning_discarded = learning
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| state != "published");

    let mut overall_tokens = staged.tokens.clone();
    overall_tokens.add(&learning_usage.tokens);
    overall_tokens.add(&background.tokens);
    let mut discarded_tokens = staged.discarded_tokens.clone();
    let mut discarded_cli = staged.discarded_cli_seconds;
    let mut discarded_api = staged.discarded_api_seconds;
    if learning_discarded {
        discarded_tokens.add(&learning_usage.tokens);
        discarded_cli += learning_usage.cli_seconds;
        discarded_api += learning_usage.api_seconds;
    }
    let overall_cli = staged.cli_seconds + learning_usage.cli_seconds + background.cli_seconds;
    let overall_api = staged.api_seconds + learning_usage.api_seconds + background.api_seconds;

    sanitized_analysis(json!({
        "schema_version": 1,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "database": database.display().to_string(),
        "pulsation": {
            "id": pulsation,
            "feature_name": feature_name,
            "status": status,
            "stage": stage,
            "pipeline_version": pipeline_version,
            "created_at": created_at,
            "updated_at": updated_at,
        },
        "totals": {
            "wall_clock_seconds": wall_seconds,
            "cli_seconds": overall_cli,
            "cli_share_percent": ratio(overall_cli, wall_seconds),
            "checkpoint_seconds": checkpoint_seconds,
            "checkpoint_share_percent": ratio(checkpoint_seconds, wall_seconds),
            "cli_runs": cli_runs,
            "cli_runs_linked_by_inference": inferred_links,
            "cli_seconds_discarded": discarded_cli,
            "cli_waste_percent": ratio(discarded_cli, overall_cli),
            "api_seconds": overall_api,
            "api_share_percent": ratio(overall_api, wall_seconds),
            "api_calls": runs.len() - cli_runs,
            "api_seconds_discarded": discarded_api,
            "tokens": overall_tokens.to_json(),
            "tokens_discarded": discarded_tokens.to_json(),
            "token_waste_percent": ratio(
                (discarded_tokens.input + discarded_tokens.output) as f64,
                (overall_tokens.input + overall_tokens.output) as f64,
            ),
        },
        "stages": staged.stages,
        "learning": {
            "run": learning,
            "cli_seconds": learning_usage.cli_seconds,
            "api_seconds": learning_usage.api_seconds,
            "tokens": learning_usage.tokens.to_json(),
            "discarded": learning_discarded,
        },
        "background": {
            "runs": background.runs,
            "cli_seconds": background.cli_seconds,
            "api_seconds": background.api_seconds,
            "tokens": background.tokens.to_json(),
        },
        "checkpoints": checkpoints,
        "retrieval": analyze_retrieval(&connection, &pulsation)?,
        "validation_errors": load_validation_errors(&connection, &pulsation)?,
        "plan_quota": analyze_plan_quota(database, &runs[..cli_runs]),
    }))
}

/// Plan-window change summed over the pulsation's CLI runs, per CLI and window (ADR-028).
/// Runs without both boundary samples, or whose window reset mid-run, contribute nothing.
/// CLI label, scope and window length.
type QuotaWindowKey = (&'static str, Option<String>, Option<u32>);

fn analyze_plan_quota(database: &Path, runs: &[AgentRun]) -> Value {
    let store = crate::quota::QuotaStore::new(database);
    // Summed change and whether any run overlapped, per window; ordered for stable output.
    let mut totals: BTreeMap<QuotaWindowKey, (f64, bool)> = BTreeMap::new();
    let deltas = store
        .run_deltas_many(runs.iter().map(|run| run.id.as_str()))
        .unwrap_or_default();
    for run_deltas in deltas.into_values() {
        for delta in run_deltas {
            let total = totals
                .entry((
                    delta.backend.label(),
                    delta.scope.clone(),
                    delta.window_minutes,
                ))
                .or_default();
            total.0 += delta.delta();
            total.1 |= delta.approximate;
        }
    }
    Value::Array(
        totals
            .into_iter()
            .map(|((cli, scope, minutes), (delta, approximate))| {
                let window = crate::quota::window_label(minutes);
                json!({
                    "cli": cli,
                    "window": match scope {
                        Some(scope) => format!("{window} · {scope}"),
                        None => window,
                    },
                    "delta_percent": (delta * 10.0).round() / 10.0,
                    "approximate": approximate,
                })
            })
            .collect(),
    )
}

/// Maps an [`analyze`] report onto the typed input of the HTML pulsation report
/// (ADR-027 §5). Telemetry carries no criteria, checks or files, so `outcome`
/// stays `None` and the rendered report says so instead of guessing. The app's
/// report adds the outcome from the typed Integrator payload.
pub fn to_html_report(report: &Value) -> Result<PulsationReport, AnalysisError> {
    let text = |value: &Value, field: &str| -> Result<String, AnalysisError> {
        value[field]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("analysis report is missing {field}").into())
    };
    let seconds = |value: &Value| value.as_f64().unwrap_or(0.0);
    let count = |value: &Value| value.as_i64().unwrap_or(0);

    let stages = report["stages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|stage| -> Result<ReportStage, AnalysisError> {
            Ok(ReportStage {
                stage: serde_json::from_value(stage["stage"].clone())?,
                runs: count(&stage["cli_runs"]) + count(&stage["api_calls"]),
                agent_seconds: seconds(&stage["cli_seconds"]) + seconds(&stage["api_seconds"]),
                discarded_seconds: seconds(&stage["cli_seconds_discarded"])
                    + seconds(&stage["api_seconds_discarded"]),
                input_tokens: count(&stage["tokens"]["input"]),
                output_tokens: count(&stage["tokens"]["output"]),
            })
        })
        .collect::<Result<Vec<_>, AnalysisError>>()?;
    // Learning runs after the pipeline closes, so only stage time counts as
    // agents working inside this pulsation's wall clock.
    let agent_seconds: f64 = stages.iter().map(|stage| stage.agent_seconds).sum();

    let pulsation = &report["pulsation"];
    let totals = &report["totals"];
    Ok(PulsationReport {
        pulsation: ReportPulsation {
            id: text(pulsation, "id")?,
            feature_name: text(pulsation, "feature_name")?,
            status: serde_json::from_value(pulsation["status"].clone())?,
            stage: serde_json::from_value(pulsation["stage"].clone())?,
            pipeline_version: count(&pulsation["pipeline_version"]),
            created_at: text(pulsation, "created_at")?,
            updated_at: text(pulsation, "updated_at")?,
        },
        outcome: None,
        run: ReportRun {
            wall_clock_seconds: seconds(&totals["wall_clock_seconds"]),
            agent_seconds,
            review_wait_seconds: seconds(&report["checkpoints"]["review_seconds"]),
            unblock_wait_seconds: seconds(&report["checkpoints"]["unblock_failure_seconds"]),
            tokens: ReportTokens {
                input: count(&totals["tokens"]["input"]),
                output: count(&totals["tokens"]["output"]),
                cache_read: count(&totals["tokens"]["cache_read"]),
                discarded_input: count(&totals["tokens_discarded"]["input"]),
                discarded_output: count(&totals["tokens_discarded"]["output"]),
            },
            stages,
            plan_quota: report["plan_quota"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|quota| {
                    Some(ReportPlanQuota {
                        cli: quota["cli"].as_str()?.to_owned(),
                        window: quota["window"].as_str()?.to_owned(),
                        delta_percent: quota["delta_percent"].as_f64()?,
                        approximate: quota["approximate"].as_bool().unwrap_or(false),
                    })
                })
                .collect(),
        },
        rejected_outputs: report["validation_errors"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|error| RejectedOutput {
                agent: error["agent"].as_str().unwrap_or("-").to_owned(),
                raw_output_bytes: count(&error["raw_output_bytes"]),
                error: error["validation_error"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect(),
    })
}

fn duration(value: Option<f64>) -> String {
    match value {
        Some(value) if value >= 3600.0 => format!(
            "{}h{:02}m",
            (value / 3600.0) as i64,
            ((value % 3600.0) / 60.0) as i64
        ),
        Some(value) if value >= 60.0 => {
            format!("{}m{:02}s", (value / 60.0) as i64, (value % 60.0) as i64)
        }
        Some(value) => format!("{value:.0}s"),
        None => "-".into(),
    }
}

fn percent(value: &Value) -> String {
    value
        .as_f64()
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "-".into())
}

/// Renders the report as the scannable summary a human actually reads.
pub fn render_text(report: &Value) -> String {
    let mut out = String::new();
    let pulsation = &report["pulsation"];
    let totals = &report["totals"];
    let tokens = &totals["tokens"];
    out.push_str(&format!(
        "Pulsation {} \"{}\"\n  status={} stage={} pipeline=v{}\n\n",
        pulsation["id"].as_str().unwrap_or("-"),
        pulsation["feature_name"].as_str().unwrap_or("-"),
        pulsation["status"].as_str().unwrap_or("-"),
        pulsation["stage"].as_str().unwrap_or("-"),
        pulsation["pipeline_version"],
    ));

    out.push_str("== Totals ==\n");
    out.push_str(&format!(
        "  wall clock           {}\n  CLI time             {} ({} of wall clock)\n  API time             {} ({} of wall clock)\n  parked at checkpoint {} ({} of wall clock)\n  CLI runs             {}\n  API calls            {}\n  CLI time discarded   {} ({})\n  API time discarded   {}\n",
        duration(totals["wall_clock_seconds"].as_f64()),
        duration(totals["cli_seconds"].as_f64()),
        percent(&totals["cli_share_percent"]),
        duration(totals["api_seconds"].as_f64()),
        percent(&totals["api_share_percent"]),
        duration(totals["checkpoint_seconds"].as_f64()),
        percent(&totals["checkpoint_share_percent"]),
        totals["cli_runs"],
        totals["api_calls"],
        duration(totals["cli_seconds_discarded"].as_f64()),
        percent(&totals["cli_waste_percent"]),
        duration(totals["api_seconds_discarded"].as_f64()),
    ));
    out.push_str(&format!(
        "  tokens               {} in / {} out ({} reasoning)\n  cache read           {} ({} of input)\n  tokens discarded     {} in / {} out ({} of all tokens)\n",
        tokens["input"],
        tokens["output"],
        tokens["reasoning"],
        tokens["cache_read"],
        percent(&tokens["cache_read_ratio_percent"]),
        totals["tokens_discarded"]["input"],
        totals["tokens_discarded"]["output"],
        percent(&totals["token_waste_percent"]),
    ));
    if tokens["unavailable_records"].as_i64().unwrap_or(0) > 0
        || tokens["phantom_records"].as_i64().unwrap_or(0) > 0
    {
        out.push_str(&format!(
            "  usage gaps           {} unmeasured, {} phantom zero records\n",
            tokens["unavailable_records"], tokens["phantom_records"],
        ));
    }
    if totals["cli_runs_linked_by_inference"].as_i64().unwrap_or(0) > 0 {
        out.push_str(&format!(
            "  attribution          {} runs matched to their attempt by order (no persisted link)\n",
            totals["cli_runs_linked_by_inference"],
        ));
    }
    for quota in report["plan_quota"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  plan usage           {} {} {:+.1}%{}\n",
            quota["cli"].as_str().unwrap_or("-"),
            quota["window"].as_str().unwrap_or("-"),
            quota["delta_percent"].as_f64().unwrap_or(0.0),
            if quota["approximate"].as_bool() == Some(true) {
                " (approx.)"
            } else {
                ""
            },
        ));
    }

    out.push_str("\n== Stages ==\n");
    for stage in report["stages"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {}. {:<14} cli_runs={:<2} api_calls={:<2} wall={:<7} cli_time={:<7} api_time={:<7} discarded={:<7} ({})\n",
            stage["position"],
            stage["stage"].as_str().unwrap_or("-"),
            stage["cli_runs"],
            stage["api_calls"],
            duration(stage["wall_seconds"].as_f64()),
            duration(stage["cli_seconds"].as_f64()),
            duration(stage["api_seconds"].as_f64()),
            duration(stage["cli_seconds_discarded"].as_f64()),
            percent(&stage["cli_waste_percent"]),
        ));
        for attempt in stage["attempts"].as_array().into_iter().flatten() {
            let marker = if attempt["discarded"].as_bool() == Some(true) {
                "x"
            } else {
                "."
            };
            out.push_str(&format!(
                "       {} {:<3} {:<9} {:<10} {:>6} out={:<6} {}\n",
                marker,
                attempt["transport"].as_str().unwrap_or("-"),
                attempt["backend"].as_str().unwrap_or("-"),
                attempt["status"].as_str().unwrap_or("-"),
                duration(attempt["duration_seconds"].as_f64()),
                attempt["tokens"]["output"],
                attempt["error_code"]
                    .as_str()
                    .or_else(|| attempt["model_divergence"].as_str())
                    .unwrap_or(""),
            ));
        }
    }

    let checkpoints = &report["checkpoints"];
    out.push_str(&format!(
        "\n== Human attention ==\n  reviewing work       {}\n  unblocking failures  {} ({})\n",
        duration(checkpoints["review_seconds"].as_f64()),
        duration(checkpoints["unblock_failure_seconds"].as_f64()),
        percent(&checkpoints["unblock_share_percent"]),
    ));

    let retrieval = &report["retrieval"];
    out.push_str(&format!(
        "\n== Retrieval ==\n  runs={} fallback={} ({}) semantic={} delivered={} cited={}\n",
        retrieval["runs"],
        retrieval["fallback_used"],
        percent(&retrieval["fallback_percent"]),
        retrieval["runs_with_semantic_similarity"],
        retrieval["delivered"],
        retrieval["cited"],
    ));
    // A fallback that never stops being used is a broken dependency reporting
    // itself as normal operation, and nothing else says so out loud.
    if retrieval["runs"].as_i64().unwrap_or(0) > 0
        && retrieval["fallback_percent"].as_f64() == Some(100.0)
    {
        out.push_str(
            "  WARNING: every retrieval fell back to graph-only. The semantic half never ran - check that the embedding provider is reachable and that workspace_note_embeddings is populated.\n",
        );
    }
    if retrieval["delivered_never_cited"].as_i64().unwrap_or(0) > 0 {
        out.push_str(&format!(
            "  NOTE: {} delivered documents were never cited - context paid for and unused.\n",
            retrieval["delivered_never_cited"],
        ));
    }

    let learning = &report["learning"]["run"];
    out.push_str(&format!(
        "\n== Learning ==\n  state={} calls={}/{} {}\n",
        learning["state"].as_str().unwrap_or("(none)"),
        learning["total_calls"],
        learning["budget_grants"],
        learning["diagnostic"].as_str().unwrap_or(""),
    ));

    let errors = report["validation_errors"].as_array();
    if errors.is_some_and(|errors| !errors.is_empty()) {
        out.push_str("\n== Rejected model output ==\n");
        for error in errors.into_iter().flatten() {
            out.push_str(&format!(
                "  {:<14} raw={:<7} {}\n",
                error["agent"].as_str().unwrap_or("-"),
                error["raw_output_bytes"],
                error["validation_error"].as_str().unwrap_or(""),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence_service::{PulsationStage, PulsationStatus};
    use crate::reports::render_pulsation_report;

    #[test]
    fn analysis_exports_sanitize_nested_runtime_fields_without_rewriting_source_identity() {
        let original = json!({
            "pulsation": {"id":"a016468b-33f6-4db8-b451-10b58f43303c", "feature_name":"ghp_syntheticabcdefghijklmnop"},
            "validation_errors":[{"validation_error":r#"{"message":"sk-proj-\u0061bcdefghijklmnopqrst"}"#}],
            "criteria":[{"description":"Authorization: Bearer synthetic-credential-value"}],
            "original_file_sha256":"a".repeat(64),
        });
        let original_sha = crate::secret_redactor::text_sha256(&original.to_string());
        let observed = sanitized_analysis(original.clone()).unwrap();
        let metadata = observed["sanitization"].clone();
        let mut observed_data = observed.clone();
        observed_data
            .as_object_mut()
            .unwrap()
            .remove("sanitization");
        assert_eq!(
            metadata["policy"],
            crate::secret_redactor::SANITIZATION_POLICY_VERSION
        );
        assert_eq!(
            metadata["originalEvidenceSha256"],
            crate::secret_redactor::text_sha256(&canonical_report_json(&original).unwrap())
        );
        assert_eq!(
            metadata["sanitizedContentSha256"],
            crate::secret_redactor::text_sha256(&canonical_report_json(&observed_data).unwrap())
        );
        assert_ne!(
            metadata["originalEvidenceSha256"],
            metadata["sanitizedContentSha256"]
        );
        assert!(
            metadata["hashScope"]
                .as_str()
                .unwrap()
                .contains("before sanitization metadata")
        );
        let exported = observed.to_string();
        for forbidden in [
            "ghp_synthetic",
            "bcdefghijklmnopqrst",
            "synthetic-credential-value",
            "\\u0061",
        ] {
            assert!(!exported.contains(forbidden));
        }
        assert_eq!(observed["pulsation"]["id"], original["pulsation"]["id"]);
        assert_eq!(
            observed["original_file_sha256"],
            original["original_file_sha256"]
        );
        assert_eq!(
            crate::secret_redactor::text_sha256(&original.to_string()),
            original_sha
        );
    }

    #[test]
    fn report_json_hashing_is_ordered_and_serialized_units_are_bounded() {
        let first: Value = serde_json::from_str(r#"{"z":[{"b":2,"a":1}],"a":0}"#).unwrap();
        let reordered: Value = serde_json::from_str(r#"{"a":0,"z":[{"a":1,"b":2}]}"#).unwrap();
        assert_eq!(
            canonical_report_json(&first).unwrap(),
            canonical_report_json(&reordered).unwrap()
        );
        assert_eq!(
            canonical_report_json(&first).unwrap(),
            r#"{"a":0,"z":[{"a":1,"b":2}]}"#
        );
        let oversized = json!({"pulsation":{"feature_name":"x".repeat(crate::secret_redactor::MAX_SANITIZATION_BYTES)}});
        assert!(sanitized_analysis(oversized).is_err());
    }

    fn analysis() -> Value {
        json!({
            "pulsation": {
                "id": "5d0c2a6e-8b1f-4c3a-9e27-61f0b4a9d2c8",
                "feature_name": "Usage export to CSV",
                "status": "done",
                "stage": "Integrator",
                "pipeline_version": 4,
                "created_at": "2026-10-15T13:05:00Z",
                "updated_at": "2026-10-15T14:32:00Z",
            },
            "totals": {
                "wall_clock_seconds": 5220.0,
                "tokens": {"input": 180000, "output": 39500, "cache_read": 92000},
                "tokens_discarded": {"input": 1200, "output": 300},
            },
            "stages": [
                {"stage": "Intake", "cli_runs": 1, "cli_seconds": 64.0, "cli_seconds_discarded": 0.0,
                 "tokens": {"input": 7000, "output": 1400}},
                {"stage": "SPECIFICATION", "cli_runs": 2, "cli_seconds": 220.0, "cli_seconds_discarded": 40.0,
                 "tokens": {"input": 26000, "output": 5200}},
            ],
            "checkpoints": {"review_seconds": 3430.0, "unblock_failure_seconds": 120.0},
            "validation_errors": [
                {"agent": "Contract", "raw_output_bytes": 2048, "validation_error": "missing <groups>"},
            ],
        })
    }

    #[test]
    fn maps_telemetry_onto_the_html_report_without_inventing_an_outcome() {
        let report = to_html_report(&analysis()).unwrap();
        assert_eq!(report.pulsation.status, PulsationStatus::Done);
        assert_eq!(report.pulsation.stage, PulsationStage::Integrator);
        assert_eq!(report.run.stages[1].stage, PulsationStage::Specification);
        assert_eq!(report.run.agent_seconds, 284.0);
        assert_eq!(report.run.unblock_wait_seconds, 120.0);
        assert_eq!(report.run.tokens.discarded_input, 1200);
        assert!(report.outcome.is_none());

        let html = render_pulsation_report(&report);
        assert!(html.contains("<h1>Usage export to CSV</h1>"));
        assert!(html.contains("built from run telemetry only"));
        assert!(html.contains("missing &lt;groups&gt;"));
    }

    #[test]
    fn rejects_an_analysis_without_pulsation_identity() {
        let mut value = analysis();
        value["pulsation"]["id"] = Value::Null;
        assert!(to_html_report(&value).is_err());
    }
}
