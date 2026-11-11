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

pub(super) fn ratio(part: f64, whole: f64) -> Option<f64> {
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

/// One compatible set of supplier-reported accounting units. A missing cache
/// breakdown is unknown, even when the total input is known.
#[derive(Debug, Clone, Default)]
struct CacheMeasurement {
    records: u64,
    // eligible + unknown + invalid + unavailable partitions all records.
    eligible_records: u64,
    // Quality and zero-input are subsets; they overlap the eligibility partition.
    partial_records: u64,
    unknown_records: u64,
    invalid_records: u64,
    unavailable_records: u64,
    zero_input_records: u64,
    input: i64,
    read: i64,
    overflow: bool,
}

impl CacheMeasurement {
    fn observe(&mut self, input: Option<i64>, read: Option<i64>, quality: &str) {
        self.records = self.records.saturating_add(1);
        if quality == "unavailable" {
            self.unavailable_records = self.unavailable_records.saturating_add(1);
            return;
        }
        if quality == "partial" {
            self.partial_records = self.partial_records.saturating_add(1);
        }
        let (Some(input), Some(read)) = (input, read) else {
            self.unknown_records = self.unknown_records.saturating_add(1);
            return;
        };
        if input < 0 || read < 0 || read > input {
            self.invalid_records = self.invalid_records.saturating_add(1);
            return;
        }
        self.eligible_records = self.eligible_records.saturating_add(1);
        if input == 0 {
            self.zero_input_records = self.zero_input_records.saturating_add(1);
        }
        match (self.input.checked_add(input), self.read.checked_add(read)) {
            (Some(input), Some(read)) => {
                self.input = input;
                self.read = read;
            }
            _ => self.overflow = true,
        }
    }

    fn add(&mut self, other: &Self) {
        self.records = self.records.saturating_add(other.records);
        self.eligible_records = self.eligible_records.saturating_add(other.eligible_records);
        self.partial_records = self.partial_records.saturating_add(other.partial_records);
        self.unknown_records = self.unknown_records.saturating_add(other.unknown_records);
        self.invalid_records = self.invalid_records.saturating_add(other.invalid_records);
        self.unavailable_records = self
            .unavailable_records
            .saturating_add(other.unavailable_records);
        self.zero_input_records = self
            .zero_input_records
            .saturating_add(other.zero_input_records);
        self.overflow |= other.overflow;
        match (
            self.input.checked_add(other.input),
            self.read.checked_add(other.read),
        ) {
            (Some(input), Some(read)) => {
                self.input = input;
                self.read = read;
            }
            _ => self.overflow = true,
        }
    }

    fn ratio_percent(&self) -> Option<f64> {
        if self.overflow {
            None
        } else {
            ratio(self.read as f64, self.input as f64)
        }
    }

    fn state(&self) -> &'static str {
        let incomplete = self.unknown_records > 0
            || self.invalid_records > 0
            || self.unavailable_records > 0
            || self.partial_records > 0;
        if self.overflow {
            "unknown"
        } else if self.input > 0 {
            if incomplete { "partial" } else { "known" }
        } else if self.zero_input_records > 0 && !incomplete {
            "not_applicable"
        } else {
            "unknown"
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "state": self.state(),
            "basis": "reported_normalized_input",
            "input_tokens": (!self.overflow && self.eligible_records > 0).then_some(self.input),
            "cache_read_tokens": (!self.overflow && self.eligible_records > 0).then_some(self.read),
            "ratio_percent": self.ratio_percent(),
            "records": self.records,
            "eligible_records": self.eligible_records,
            "partial_records": self.partial_records,
            "unknown_records": self.unknown_records,
            "invalid_records": self.invalid_records,
            "unavailable_records": self.unavailable_records,
            "zero_input_records": self.zero_input_records,
            "overflow": self.overflow,
        })
    }
}

/// Group from each ledger row, rather than its enclosing CLI process: one
/// process can report several models. Prompt/schema versions were not recorded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheGroup {
    transport: String,
    provider: Option<String>,
    model: Option<String>,
    model_source: &'static str,
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
    /// Numeric legacy totals saturate instead of wrapping; this flag discloses it.
    totals_overflow: bool,
    cache: CacheMeasurement,
    cache_groups: BTreeMap<CacheGroup, CacheMeasurement>,
}

fn cache_model_identifier(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty() && value != "default").then(|| value.to_owned())
    })
}

/// Preserve the numeric legacy DTO shape while flagging unrepresentable totals.
fn add_token_count(target: &mut i64, value: i64, overflow: &mut bool) {
    match target.checked_add(value) {
        Some(total) => *target = total,
        None => {
            *target = target.saturating_add(value);
            *overflow = true;
        }
    }
}

impl Tokens {
    fn add(&mut self, other: &Tokens) {
        self.totals_overflow |= other.totals_overflow;
        add_token_count(&mut self.input, other.input, &mut self.totals_overflow);
        add_token_count(&mut self.output, other.output, &mut self.totals_overflow);
        add_token_count(
            &mut self.reasoning,
            other.reasoning,
            &mut self.totals_overflow,
        );
        add_token_count(
            &mut self.cache_read,
            other.cache_read,
            &mut self.totals_overflow,
        );
        add_token_count(
            &mut self.cache_write,
            other.cache_write,
            &mut self.totals_overflow,
        );
        self.unavailable_records = self
            .unavailable_records
            .saturating_add(other.unavailable_records);
        self.phantom_records = self.phantom_records.saturating_add(other.phantom_records);
        self.cache.add(&other.cache);
        for (group, measurement) in &other.cache_groups {
            self.cache_groups
                .entry(group.clone())
                .or_default()
                .add(measurement);
        }
    }

    fn to_json(&self) -> Value {
        let groups: Vec<Value> = self
            .cache_groups
            .iter()
            .map(|(group, measurement)| {
                json!({
                    "transport": group.transport,
                    "provider": group.provider,
                    "model": group.model,
                    "model_source": group.model_source,
                    "prompt_version": Value::Null,
                    "response_schema_version": Value::Null,
                    "measurement": measurement.to_json(),
                })
            })
            .collect();
        json!({
            "input": self.input,
            "output": self.output,
            "reasoning": self.reasoning,
            "cache_read": self.cache_read,
            "cache_write": self.cache_write,
            "cache_read_ratio_percent": self.cache.ratio_percent(),
            "cache_measurement": self.cache.to_json(),
            "cache_groups": groups,
            "totals_overflow": self.totals_overflow,
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
        "SELECT input_tokens,output_tokens,reasoning_tokens,cache_read_tokens,cache_write_tokens,quality,transport,provider_id,requested_model,observed_model FROM token_usage_records WHERE source_kind=?1 AND source_id=?2",
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
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?;
        for row in rows {
            let (
                input,
                output,
                reasoning,
                cache_read,
                cache_write,
                quality,
                transport,
                provider,
                requested_model,
                observed_model,
            ) = row?;
            // Older rows may predate the recorder's identifier guard. An empty
            // string or the dynamic default is not an observed model identity.
            let observed_model = cache_model_identifier(observed_model);
            let requested_model = cache_model_identifier(requested_model);
            let provider = (!provider.trim().is_empty()).then(|| provider.trim().to_owned());
            let (model, model_source) = if let Some(observed) = observed_model {
                (Some(observed), "observed")
            } else if let Some(requested) = requested_model {
                (Some(requested), "requested")
            } else {
                (None, "unknown")
            };
            let group = CacheGroup {
                transport,
                provider,
                model,
                model_source,
            };
            // Cache coverage is classified even for zero/phantom usage rows.
            entry.cache.observe(input, cache_read, &quality);
            entry
                .cache_groups
                .entry(group)
                .or_default()
                .observe(input, cache_read, &quality);
            if quality == "unavailable" {
                entry.unavailable_records += 1;
                continue;
            }
            if input.unwrap_or(0) == 0 && output.unwrap_or(0) == 0 {
                entry.phantom_records += 1;
                continue;
            }
            add_token_count(
                &mut entry.input,
                input.unwrap_or(0),
                &mut entry.totals_overflow,
            );
            add_token_count(
                &mut entry.output,
                output.unwrap_or(0),
                &mut entry.totals_overflow,
            );
            add_token_count(
                &mut entry.reasoning,
                reasoning.unwrap_or(0),
                &mut entry.totals_overflow,
            );
            add_token_count(
                &mut entry.cache_read,
                cache_read.unwrap_or(0),
                &mut entry.totals_overflow,
            );
            add_token_count(
                &mut entry.cache_write,
                cache_write.unwrap_or(0),
                &mut entry.totals_overflow,
            );
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

fn table_exists(connection: &Connection, table: &str) -> Result<bool, AnalysisError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )?)
}

/// What the mandate allowed and refused, what units asked for and were
/// granted, and how the record was sealed (ADR-035, ADR-036). A block is null
/// when the database predates its tables or the pulsation has none of it.
fn analyze_governance(connection: &Connection, pulsation: &str) -> Result<Value, AnalysisError> {
    let mandates = if table_exists(connection, "execution_mandates")? {
        let mut by_cause: BTreeMap<String, i64> = BTreeMap::new();
        let mut statement = connection.prepare(
            "SELECT cause,COUNT(*) FROM execution_mandates WHERE pulsation_id=?1 GROUP BY cause",
        )?;
        for row in statement.query_map([pulsation], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (cause, count) = row?;
            by_cause.insert(cause, count);
        }
        let enforcement = connection
            .query_row(
                "SELECT enforcement FROM execution_mandates WHERE pulsation_id=?1 ORDER BY revision DESC LIMIT 1",
                [pulsation],
                |row| row.get::<_, String>(0),
            )
            .ok();
        // Every event is a write or dispatch the host refused; a healthy run
        // has none, so anything else here is worth reading.
        let (refused_writes, capability_mismatches) = connection.query_row(
            "SELECT COALESCE(SUM(CASE WHEN rule='scope_violation' THEN occurrences END),0),COALESCE(SUM(CASE WHEN rule='capability_mismatch' THEN occurrences END),0) FROM mandate_enforcement_events WHERE pulsation_id=?1",
            [pulsation],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        json!({
            "revisions": by_cause.values().sum::<i64>(),
            "revisions_by_cause": by_cause,
            "enforcement": enforcement,
            "refused_writes": refused_writes,
            "capability_mismatches": capability_mismatches,
        })
    } else {
        Value::Null
    };

    let scope_requests = if table_exists(connection, "mandate_scope_requests")? {
        let mut by_state: BTreeMap<String, i64> = BTreeMap::new();
        let mut files_granted = 0;
        let mut parked_seconds = 0.0;
        let mut statement = connection.prepare(
            "SELECT state,json_array_length(accepted_json),created_at,decided_at FROM mandate_scope_requests WHERE pulsation_id=?1",
        )?;
        for row in statement.query_map([pulsation], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })? {
            let (state, files, created_at, decided_at) = row?;
            // A superseded request was granted before a revision replaced it.
            if state == "granted" || state == "superseded" {
                files_granted += files;
            }
            parked_seconds +=
                optional_elapsed(Some(&created_at), decided_at.as_ref()).unwrap_or(0.0);
            *by_state.entry(state).or_default() += 1;
        }
        let count = |state: &str| by_state.get(state).copied().unwrap_or(0);
        let total = by_state.values().sum::<i64>();
        let granted = count("granted") + count("superseded");
        let decided = total - count("pending");
        json!({
            "total": total,
            "by_state": by_state,
            "grant_percent": ratio(granted as f64, decided as f64),
            "files_granted": files_granted,
            "parked_seconds": parked_seconds,
        })
    } else {
        Value::Null
    };

    // The intent is written with the terminal transition, so its creation is
    // when sealing could start.
    let attestation = if table_exists(connection, "attestation_seal_intents")? {
        connection
            .query_row(
                "SELECT i.state,i.created_at,a.signature_state,a.outcome,length(CAST(a.statement_json AS BLOB)),a.subject_count,a.sealed_at
                   FROM attestation_seal_intents i LEFT JOIN pulsation_attestations a ON a.pulsation_id=i.pulsation_id
                  WHERE i.pulsation_id=?1",
                [pulsation],
                |row| {
                    let intended_at = row.get::<_, String>(1)?;
                    let sealed_at = row.get::<_, Option<String>>(6)?;
                    Ok(json!({
                        "intent": row.get::<_, String>(0)?,
                        "signature": row.get::<_, Option<String>>(2)?,
                        "outcome": row.get::<_, Option<String>>(3)?,
                        "statement_bytes": row.get::<_, Option<i64>>(4)?,
                        "subject_count": row.get::<_, Option<i64>>(5)?,
                        "seal_latency_seconds": optional_elapsed(Some(&intended_at), sealed_at.as_ref()),
                    }))
                },
            )
            .ok()
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    Ok(json!({
        "mandates": mandates,
        "scope_requests": scope_requests,
        "attestation": attestation,
    }))
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
            "token_waste_percent": if discarded_tokens.totals_overflow || overall_tokens.totals_overflow {
                None
            } else {
                ratio(
                    discarded_tokens.input as f64 + discarded_tokens.output as f64,
                    overall_tokens.input as f64 + overall_tokens.output as f64,
                )
            },
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
        "governance": analyze_governance(&connection, &pulsation)?,
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
                cache_measurement: serde_json::from_value(
                    totals["tokens"]["cache_measurement"].clone(),
                )
                .ok(),
                totals_overflow: totals["tokens"]["totals_overflow"]
                    .as_bool()
                    .unwrap_or(false),
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
        review: None,
        convergence: None,
        risk: None,
        layers: None,
        provenance: None,
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
    let cache_basis = if tokens
        .get("cache_measurement")
        .is_some_and(Value::is_object)
    {
        "eligible reported input"
    } else {
        "input; legacy coverage unavailable"
    };
    out.push_str(&format!(
        "  tokens               {} in / {} out ({} reasoning)\n  cache read           {} reported tokens\n  cache read ratio     {} of {}\n  tokens discarded     {} in / {} out ({} of all tokens)\n",
        tokens["input"],
        tokens["output"],
        tokens["reasoning"],
        tokens["cache_read"],
        percent(&tokens["cache_read_ratio_percent"]),
        cache_basis,
        totals["tokens_discarded"]["input"],
        totals["tokens_discarded"]["output"],
        percent(&totals["token_waste_percent"]),
    ));
    if let Some(cache) = tokens
        .get("cache_measurement")
        .filter(|cache| cache.is_object())
    {
        out.push_str(&format!(
            "  cache coverage       state={} eligible={}/{} unknown={} invalid={} unavailable={} partial={}\n",
            cache["state"].as_str().unwrap_or("unknown"),
            cache["eligible_records"], cache["records"], cache["unknown_records"],
            cache["invalid_records"], cache["unavailable_records"], cache["partial_records"],
        ));
    }
    if tokens["totals_overflow"].as_bool() == Some(true) {
        out.push_str(
            "  WARNING: token totals overflowed; numeric totals are saturated, not exact.\n",
        );
    }
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

    let governance = &report["governance"];
    let mandates = &governance["mandates"];
    let requests = &governance["scope_requests"];
    let attestation = &governance["attestation"];
    if !mandates.is_null() || !requests.is_null() || !attestation.is_null() {
        out.push_str("\n== Governance ==\n");
    }
    if !mandates.is_null() {
        let causes = mandates["revisions_by_cause"]
            .as_object()
            .map(|causes| {
                causes
                    .iter()
                    .map(|(cause, count)| format!("{cause}={count}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "  mandate              revisions={} {} enforcement={}\n  refused by mandate   writes={} capability={}\n",
            mandates["revisions"],
            causes,
            mandates["enforcement"].as_str().unwrap_or("(none)"),
            mandates["refused_writes"],
            mandates["capability_mismatches"],
        ));
        if mandates["refused_writes"].as_i64().unwrap_or(0) > 0 {
            out.push_str(
                "  WARNING: the mandate refused writes. A stage tried to change a file it was not given - read mandate_enforcement_events.\n",
            );
        }
    }
    if !requests.is_null() {
        let states = requests["by_state"]
            .as_object()
            .map(|states| {
                states
                    .iter()
                    .map(|(state, count)| format!("{state}={count}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "  scope requests       total={} {} grant rate={} files={} parked {}\n",
            requests["total"],
            states,
            percent(&requests["grant_percent"]),
            requests["files_granted"],
            duration(requests["parked_seconds"].as_f64()),
        ));
    }
    if !attestation.is_null() {
        out.push_str(&format!(
            "  attestation          {} {} outcome={} bytes={} subjects={} sealed after {}\n",
            attestation["intent"].as_str().unwrap_or("-"),
            attestation["signature"].as_str().unwrap_or("-"),
            attestation["outcome"].as_str().unwrap_or("-"),
            attestation["statement_bytes"],
            attestation["subject_count"],
            duration(attestation["seal_latency_seconds"].as_f64()),
        ));
    }

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

    // FH-T08: cache measurements use the same reported accounting units in
    // numerator and denominator. Missing optional counters are not zero.
    fn cache_fixture_run(id: &str, transport: Transport) -> AgentRun {
        AgentRun {
            transport,
            id: id.to_owned(),
            job_id: None,
            agent_role: Some("Code".to_owned()),
            backend: Some("provider-a".to_owned()),
            model: Some("requested-model".to_owned()),
            observed_model: Some("observed-model".to_owned()),
            status: "completed".to_owned(),
            exit_code: Some(0),
            error_code: None,
            termination_reason: None,
            output_bytes: 0,
            event_count: 0,
            started_at: None,
            finished_at: None,
            deadline_at: None,
            agent_run_id: None,
        }
    }

    fn cache_fixture_database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE token_usage_records(
                source_kind TEXT,source_id TEXT,input_tokens INTEGER,
                output_tokens INTEGER,reasoning_tokens INTEGER,
                cache_read_tokens INTEGER,cache_write_tokens INTEGER,quality TEXT,
                transport TEXT,provider_id TEXT,requested_model TEXT,observed_model TEXT
            );",
        )
        .unwrap();
        db
    }

    fn insert_cache_fixture_row(
        db: &Connection,
        run: &AgentRun,
        input: Option<i64>,
        cache_read: Option<i64>,
        quality: &str,
    ) {
        db.execute(
            "INSERT INTO token_usage_records VALUES(?1,?2,?3,1,NULL,?4,NULL,?5,?6,?7,?8,?9)",
            rusqlite::params![
                run.transport.source_kind(),
                run.id,
                input,
                cache_read,
                quality,
                run.transport.as_str(),
                run.backend,
                run.model,
                run.observed_model,
            ],
        )
        .unwrap();
    }

    fn fixture_tokens(db: &Connection, run: &AgentRun) -> Tokens {
        load_tokens(db, std::slice::from_ref(run))
            .unwrap()
            .remove(&run.id)
            .unwrap()
    }

    #[test]
    fn cache_measurement_distinguishes_missing_from_reported_zero() {
        let db = cache_fixture_database();
        let run = cache_fixture_run("missing", Transport::Api);
        insert_cache_fixture_row(&db, &run, Some(100), None, "complete");
        let missing = fixture_tokens(&db, &run).to_json();
        assert!(missing["cache_read_ratio_percent"].is_null(), "{missing}");
        assert_eq!(missing["cache_measurement"]["state"], "unknown");
        assert_eq!(missing["cache_measurement"]["unknown_records"], 1);
        assert_eq!(missing["cache_measurement"]["eligible_records"], 0);
        assert_eq!(missing["input"], 100);

        let zero_run = cache_fixture_run("zero", Transport::Api);
        insert_cache_fixture_row(&db, &zero_run, Some(100), Some(0), "complete");
        let zero = fixture_tokens(&db, &zero_run).to_json();
        assert_eq!(zero["cache_read_ratio_percent"], 0.0);
        assert_eq!(zero["cache_measurement"]["state"], "known");
        assert_eq!(zero["cache_measurement"]["eligible_records"], 1);
    }

    #[test]
    fn cache_measurement_uses_paired_weighted_observations_and_reports_coverage() {
        let db = cache_fixture_database();
        let run = cache_fixture_run("mixed", Transport::Api);
        insert_cache_fixture_row(&db, &run, Some(100), Some(50), "complete");
        insert_cache_fixture_row(&db, &run, Some(900), None, "complete");
        insert_cache_fixture_row(&db, &run, Some(300), Some(30), "complete");
        let tokens = fixture_tokens(&db, &run);
        let measured = tokens.to_json();
        assert_eq!(measured["input"], 1300);
        assert_eq!(measured["cache_read"], 80);
        assert_eq!(measured["cache_read_ratio_percent"], 20.0);
        assert_eq!(measured["cache_measurement"]["input_tokens"], 400);
        assert_eq!(measured["cache_measurement"]["cache_read_tokens"], 80);
        assert_eq!(measured["cache_measurement"]["eligible_records"], 2);
        assert_eq!(measured["cache_measurement"]["unknown_records"], 1);
        assert_eq!(measured["cache_measurement"]["records"], 3);
        assert_eq!(measured["cache_measurement"]["state"], "partial");
        let mut aggregated = Tokens::default();
        aggregated.add(&tokens);
        aggregated.add(&tokens);
        let aggregated = aggregated.to_json();
        assert_eq!(aggregated["cache_read_ratio_percent"], 20.0);
        assert_eq!(aggregated["cache_measurement"]["input_tokens"], 800);
        assert_eq!(aggregated["cache_groups"].as_array().unwrap().len(), 1);
        assert_eq!(aggregated["cache_groups"][0]["measurement"]["records"], 6);
    }

    #[test]
    fn cache_measurement_retains_zero_input_even_in_phantom_records() {
        let db = cache_fixture_database();
        let run = cache_fixture_run("empty", Transport::Api);
        insert_cache_fixture_row(&db, &run, Some(0), Some(0), "complete");
        db.execute("UPDATE token_usage_records SET output_tokens=0", [])
            .unwrap();
        let measured = fixture_tokens(&db, &run).to_json();
        assert!(measured["cache_read_ratio_percent"].is_null());
        assert_eq!(measured["cache_measurement"]["state"], "not_applicable");
        assert_eq!(measured["cache_measurement"]["zero_input_records"], 1);
        assert_eq!(measured["cache_measurement"]["eligible_records"], 1);
        assert_eq!(measured["phantom_records"], 1);
    }

    #[test]
    fn cache_measurement_rejects_impossible_pairs_and_labels_partial_usage() {
        let db = cache_fixture_database();
        let invalid_run = cache_fixture_run("invalid", Transport::Api);
        insert_cache_fixture_row(&db, &invalid_run, Some(100), Some(101), "complete");
        insert_cache_fixture_row(&db, &invalid_run, Some(-1), Some(0), "complete");
        let invalid = fixture_tokens(&db, &invalid_run).to_json();
        assert!(invalid["cache_read_ratio_percent"].is_null(), "{invalid}");
        assert_eq!(invalid["cache_measurement"]["state"], "unknown");
        assert_eq!(invalid["cache_measurement"]["invalid_records"], 2);

        let partial_run = cache_fixture_run("partial", Transport::Cli);
        insert_cache_fixture_row(&db, &partial_run, Some(100), Some(25), "partial");
        insert_cache_fixture_row(&db, &partial_run, None, None, "unavailable");
        let partial = fixture_tokens(&db, &partial_run).to_json();
        assert_eq!(partial["cache_read_ratio_percent"], 25.0);
        assert_eq!(partial["cache_measurement"]["state"], "partial");
        assert_eq!(partial["cache_measurement"]["partial_records"], 1);
        assert_eq!(partial["cache_measurement"]["unavailable_records"], 1);
        assert_eq!(partial["unavailable_records"], 1);
    }

    #[test]
    fn cache_measurement_groups_each_accounting_rows_transport_provider_and_model() {
        let db = cache_fixture_database();
        let api_run = cache_fixture_run("api", Transport::Api);
        insert_cache_fixture_row(&db, &api_run, Some(100), Some(50), "complete");
        let mut cli_run = cache_fixture_run("cli", Transport::Cli);
        cli_run.backend = Some("provider-b".to_owned());
        cli_run.observed_model = Some("model-one".to_owned());
        insert_cache_fixture_row(&db, &cli_run, Some(100), Some(10), "complete");
        cli_run.observed_model = Some("model-two".to_owned());
        insert_cache_fixture_row(&db, &cli_run, Some(100), None, "complete");
        cli_run.observed_model = None;
        insert_cache_fixture_row(&db, &cli_run, Some(100), Some(20), "complete");
        let loaded = load_tokens(&db, &[api_run, cli_run]).unwrap();
        let mut total = Tokens::default();
        for tokens in loaded.values() {
            total.add(tokens);
        }
        let report = total.to_json();
        let groups = report["cache_groups"].as_array().unwrap();
        assert_eq!(groups.len(), 4);
        assert_eq!(groups[0]["transport"], "api");
        assert_eq!(groups[0]["provider"], "provider-a");
        assert_eq!(groups[0]["model_source"], "observed");
        assert_eq!(groups[0]["measurement"]["ratio_percent"], 50.0);
        let unavailable = groups
            .iter()
            .find(|group| group["model"] == "model-two")
            .unwrap();
        assert_eq!(unavailable["measurement"]["state"], "unknown");
        assert!(unavailable["measurement"]["ratio_percent"].is_null());
        let requested = groups
            .iter()
            .find(|group| group["model_source"] == "requested")
            .unwrap();
        assert_eq!(requested["model"], "requested-model");
        assert!(requested["prompt_version"].is_null());
        assert!(requested["response_schema_version"].is_null());
        assert_eq!(requested["measurement"]["ratio_percent"], 20.0);
    }

    #[test]
    fn cache_measurement_does_not_promote_empty_or_default_model_metadata() {
        let db = cache_fixture_database();
        let mut run = cache_fixture_run("metadata", Transport::Api);
        run.observed_model = Some("  ".to_owned());
        run.model = Some(" requested-model ".to_owned());
        insert_cache_fixture_row(&db, &run, Some(100), Some(20), "complete");
        run.observed_model = Some("default".to_owned());
        run.model = None;
        run.backend = Some("  ".to_owned());
        insert_cache_fixture_row(&db, &run, Some(100), Some(10), "complete");
        let measured = fixture_tokens(&db, &run).to_json();
        let groups = measured["cache_groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2);
        let requested = groups
            .iter()
            .find(|group| group["model_source"] == "requested")
            .unwrap();
        assert_eq!(requested["model"], "requested-model");
        let unknown = groups
            .iter()
            .find(|group| group["model_source"] == "unknown")
            .unwrap();
        assert!(unknown["model"].is_null());
        assert!(unknown["provider"].is_null());
        assert_eq!(unknown["measurement"]["ratio_percent"], 10.0);
    }

    #[test]
    fn cache_measurement_overflow_is_unknown_instead_of_wrapping_or_panicking() {
        let db = cache_fixture_database();
        let run = cache_fixture_run("overflow", Transport::Api);
        insert_cache_fixture_row(&db, &run, Some(i64::MAX), Some(1), "complete");
        insert_cache_fixture_row(&db, &run, Some(1), Some(0), "complete");
        let tokens = fixture_tokens(&db, &run);
        let measured = tokens.to_json();
        assert!(measured["cache_read_ratio_percent"].is_null());
        assert_eq!(measured["cache_measurement"]["state"], "unknown");
        assert_eq!(measured["cache_measurement"]["overflow"], true);
        assert!(measured["cache_measurement"]["input_tokens"].is_null());
        assert_eq!(measured["totals_overflow"], true);
        let mut aggregated = tokens.clone();
        aggregated.add(&tokens);
        assert!(aggregated.to_json()["cache_read_ratio_percent"].is_null());
    }

    #[test]
    fn cache_measurement_text_labels_eligible_input_and_unknown_coverage() {
        let db = cache_fixture_database();
        let run = cache_fixture_run("render", Transport::Api);
        insert_cache_fixture_row(&db, &run, Some(100), None, "complete");
        let mut report = analysis();
        report["totals"]["tokens"] = fixture_tokens(&db, &run).to_json();
        let text = render_text(&report);
        assert!(text.contains("of eligible reported input"), "{text}");
        assert!(text.contains("cache coverage"), "{text}");
        assert!(text.contains("state=unknown"), "{text}");
        assert!(text.contains("unknown=1"), "{text}");
    }

    #[test]
    fn cache_measurement_text_keeps_legacy_coverage_unavailable() {
        let mut report = analysis();
        report["totals"]["tokens"]["cache_read_ratio_percent"] = json!(51.1);
        let text = render_text(&report);
        assert!(text.contains("legacy coverage unavailable"), "{text}");
        assert!(!text.contains("of eligible reported input"), "{text}");
        report["totals"]["tokens"]["cache_measurement"] = Value::Null;
        assert!(render_text(&report).contains("legacy coverage unavailable"));
    }

    #[test]
    fn cache_measurement_html_and_json_use_one_rounding_rule() {
        for (input, read, expected) in [(400, 1, 0.3), (80, 1, 1.3), (400, 3, 0.8)] {
            let db = cache_fixture_database();
            let run = cache_fixture_run("rounding", Transport::Api);
            insert_cache_fixture_row(&db, &run, Some(input), Some(read), "complete");
            let mut report = analysis();
            report["totals"]["tokens"] = fixture_tokens(&db, &run).to_json();
            let normalized = report["totals"]["tokens"]["cache_read_ratio_percent"]
                .as_f64()
                .unwrap();
            assert_eq!(normalized, expected);
            let percentage = format!("{normalized:.1}%");
            assert!(render_text(&report).contains(&percentage));
            let html = crate::reports::render_pulsation_report(&to_html_report(&report).unwrap());
            assert!(
                html.contains(&format!(
                    "Cache reuse: {percentage} of eligible reported input."
                )),
                "cache={read}, input={input}: HTML must match the normalized JSON value {percentage}"
            );
        }
    }

    #[test]
    fn cache_measurement_survives_the_typed_html_report_boundary() {
        let db = cache_fixture_database();
        let run = cache_fixture_run("typed", Transport::Api);
        insert_cache_fixture_row(&db, &run, Some(100), Some(25), "complete");
        let mut report = analysis();
        report["totals"]["tokens"] = fixture_tokens(&db, &run).to_json();
        let typed = serde_json::to_value(to_html_report(&report).unwrap()).unwrap();
        assert_eq!(
            typed["run"]["tokens"]["cache_measurement"]["eligible_records"],
            1
        );
        assert_eq!(
            typed["run"]["tokens"]["cache_measurement"]["input_tokens"],
            100
        );
        assert_eq!(
            typed["run"]["tokens"]["cache_measurement"]["cache_read_tokens"],
            25
        );
        report["totals"]["tokens"]["totals_overflow"] = json!(true);
        let overflowed = serde_json::to_value(to_html_report(&report).unwrap()).unwrap();
        assert_eq!(overflowed["run"]["tokens"]["totals_overflow"], true);
    }

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
    fn governance_tolerates_an_older_database_and_flags_refused_writes() {
        let db = Connection::open_in_memory().unwrap();
        let governance = analyze_governance(&db, "p1").unwrap();
        assert_eq!(
            governance,
            json!({"mandates": null, "scope_requests": null, "attestation": null})
        );
        let mut report = analysis();
        report["governance"] = governance;
        assert!(!render_text(&report).contains("== Governance =="));

        db.execute_batch(
            "CREATE TABLE execution_mandates(pulsation_id TEXT,revision INTEGER,cause TEXT,enforcement TEXT);
             CREATE TABLE mandate_enforcement_events(pulsation_id TEXT,rule TEXT,occurrences INTEGER);
             INSERT INTO execution_mandates VALUES('p1',1,'issued','enforced_v4'),('p1',2,'transport_fallback','enforced_v4');
             INSERT INTO mandate_enforcement_events VALUES('p1','scope_violation',3),('p1','capability_mismatch',1),('p2','scope_violation',9);",
        )
        .unwrap();
        let governance = analyze_governance(&db, "p1").unwrap();
        assert_eq!(governance["mandates"]["revisions"], 2);
        assert_eq!(
            governance["mandates"]["revisions_by_cause"],
            json!({"issued": 1, "transport_fallback": 1})
        );
        assert_eq!(governance["mandates"]["refused_writes"], 3);
        assert_eq!(governance["mandates"]["capability_mismatches"], 1);
        assert!(governance["scope_requests"].is_null());
        report["governance"] = governance;
        let text = render_text(&report);
        assert!(text.contains("== Governance =="), "{text}");
        assert!(text.contains("writes=3 capability=1"), "{text}");
        assert!(
            text.contains("WARNING: the mandate refused writes"),
            "{text}"
        );
    }

    #[test]
    fn rejects_an_analysis_without_pulsation_identity() {
        let mut value = analysis();
        value["pulsation"]["id"] = Value::Null;
        assert!(to_html_report(&value).is_err());
    }
}
