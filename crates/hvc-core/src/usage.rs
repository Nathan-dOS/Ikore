//! Supplier-reported generation usage, independent of conversation retention.
//! Cache and reasoning are breakdowns of normalized input/output, not extra totals.

use std::{path::Path, time::Duration};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

pub mod api;
pub mod cli;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageMeasurement {
    pub counts: TokenCounts,
    pub observed_model: Option<String>,
    pub partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageTransport {
    Api,
    Cli,
}

impl UsageTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageContext {
    pub pulsation_id: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub project_id: Option<String>,
    pub transport: UsageTransport,
    pub provider_id: String,
    pub requested_model: Option<String>,
    pub started_at: String,
}

fn connection(path: &Path, read_only: bool) -> rusqlite::Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let db = Connection::open_with_flags(path, flags)?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}

/// Upsert a snapshot for one accounting unit. Callers identify real requests,
/// CLI turns or steps; they must never submit message summaries as extra units.
pub fn record_usage(
    path: &Path,
    context: &UsageContext,
    unit_key: &str,
    measurement: &UsageMeasurement,
) -> rusqlite::Result<()> {
    record_usage_on(&connection(path, false)?, context, unit_key, measurement)
}

pub fn record_usage_on(
    db: &Connection,
    context: &UsageContext,
    unit_key: &str,
    measurement: &UsageMeasurement,
) -> rusqlite::Result<()> {
    let mut counts = measurement.counts.clone();
    let mut partial = measurement.partial;
    for value in [
        &mut counts.input_tokens,
        &mut counts.output_tokens,
        &mut counts.total_tokens,
        &mut counts.cache_read_tokens,
        &mut counts.cache_write_tokens,
        &mut counts.reasoning_tokens,
    ] {
        if value.is_some_and(|n| n < 0) {
            *value = None;
            partial = true;
        }
    }
    if counts.total_tokens.is_none()
        && (counts.input_tokens.is_some() || counts.output_tokens.is_some())
    {
        counts.total_tokens = counts
            .input_tokens
            .unwrap_or(0)
            .checked_add(counts.output_tokens.unwrap_or(0));
        partial |= counts.total_tokens.is_none();
    }
    let quality = if counts.input_tokens.is_none()
        && counts.output_tokens.is_none()
        && counts.total_tokens.is_none()
    {
        "unavailable"
    } else if partial || counts.input_tokens.is_none() || counts.output_tokens.is_none() {
        "partial"
    } else {
        "complete"
    };
    // Only bounded identifiers cross this boundary, never prompt content.
    let observed = model_identifier(measurement.observed_model.as_deref());
    let requested = model_identifier(context.requested_model.as_deref());
    db.execute(
        "INSERT INTO token_usage_records (
            source_kind,source_id,unit_key,project_id,transport,provider_id,
            requested_model,observed_model,input_tokens,output_tokens,total_tokens,
            cache_read_tokens,cache_write_tokens,reasoning_tokens,quality,started_at,updated_at,pulsation_id
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
         ON CONFLICT(source_kind,source_id,unit_key) DO UPDATE SET
            pulsation_id=COALESCE(token_usage_records.pulsation_id,excluded.pulsation_id),
            observed_model=COALESCE(excluded.observed_model,token_usage_records.observed_model),
            input_tokens=COALESCE(excluded.input_tokens,token_usage_records.input_tokens),
            output_tokens=COALESCE(excluded.output_tokens,token_usage_records.output_tokens),
            total_tokens=COALESCE(excluded.total_tokens,token_usage_records.total_tokens),
            cache_read_tokens=COALESCE(excluded.cache_read_tokens,token_usage_records.cache_read_tokens),
            cache_write_tokens=COALESCE(excluded.cache_write_tokens,token_usage_records.cache_write_tokens),
            reasoning_tokens=COALESCE(excluded.reasoning_tokens,token_usage_records.reasoning_tokens),
            quality=CASE WHEN excluded.quality='unavailable' THEN token_usage_records.quality ELSE excluded.quality END,
            updated_at=excluded.updated_at",
        params![context.source_kind, context.source_id, unit_key, context.project_id,
            context.transport.as_str(), context.provider_id, requested, observed,
            counts.input_tokens, counts.output_tokens, counts.total_tokens,
            counts.cache_read_tokens, counts.cache_write_tokens, counts.reasoning_tokens,
            quality, context.started_at, Utc::now().to_rfc3339(), context.pulsation_id],
    )?;
    Ok(())
}

fn model_identifier(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| {
        !v.is_empty()
            && *v != "default"
            && v.len() <= 256
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.:/@+".contains(c))
            && crate::secret_redactor::redact_secrets(v).is_ok_and(|r| r.count == 0)
    })
}

/// Composition identity stamped on usage rows after a work-unit attempt
/// completes (ADR-022). Attribution never creates rows and never changes
/// counts; a reused or host-only step produces no usage at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitAttribution {
    pub stage_execution_id: String,
    pub work_unit_id: String,
    pub attempt_id: String,
    pub executor_kind: String,
}

/// Stamps `unit` on every usage row produced by one LLM execution: the API
/// attempts of that execution or the CLI run that carried its execution id.
/// Idempotent; returns the number of rows touched.
pub fn attribute_execution_on(
    db: &Connection,
    llm_execution_id: &str,
    unit: &UnitAttribution,
) -> rusqlite::Result<usize> {
    db.execute(
        "UPDATE token_usage_records
         SET stage_execution_id=?1, work_unit_id=?2, attempt_id=?3, executor_kind=?4
         WHERE (source_kind='llm_attempt'
                AND source_id IN (SELECT id FROM llm_attempts WHERE execution_id=?5))
            OR (source_kind='terminal_run'
                AND source_id IN (SELECT id FROM terminal_runs WHERE llm_execution_id=?5))",
        params![
            unit.stage_execution_id,
            unit.work_unit_id,
            unit.attempt_id,
            unit.executor_kind,
            llm_execution_id
        ],
    )
}

/// Actual supplier-reported usage for one composed stage execution, with
/// unknown counters kept unknown.
pub fn stage_execution_totals(
    db: &Connection,
    stage_execution_id: &str,
) -> rusqlite::Result<UsageTotals> {
    db.query_row(
        &format!("SELECT {TOTALS_SQL} FROM token_usage_records WHERE stage_execution_id=?1"),
        [stage_execution_id],
        |row| read_totals(row, 0),
    )
}

/// Actual usage for one work-unit attempt.
pub fn attempt_totals(db: &Connection, attempt_id: &str) -> rusqlite::Result<UsageTotals> {
    db.query_row(
        &format!("SELECT {TOTALS_SQL} FROM token_usage_records WHERE attempt_id=?1"),
        [attempt_id],
        |row| read_totals(row, 0),
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePeriod {
    #[default]
    AllTime,
    Last7Days,
    Last30Days,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageQuery {
    pub pulsation_id: Option<String>,
    pub project_id: Option<String>,
    pub transport: Option<UsageTransport>,
    pub period: UsagePeriod,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub records: u64,
    pub partial_records: u64,
    pub unavailable_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub provider_id: String,
    pub model_id: String,
    pub model_source: String,
    pub transport: UsageTransport,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub period: String,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageProject {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub pulsations: Vec<PulsationUsage>,
    pub without_pulsation: UsageTotals,
    pub totals: UsageTotals,
    pub models: Vec<ModelUsage>,
    pub series: Vec<UsageBucket>,
    pub projects: Vec<UsageProject>,
    /// Composed-stage attempts grouped by executor specialty (ADR-022);
    /// empty when no record carries a unit attribution. The same ledger
    /// rows the totals count - a breakdown, never an addition.
    #[serde(default)]
    pub executors: Vec<ExecutorUsage>,
    /// API attempts attributed to their persisted canonical or FreeForm target.
    /// This is a breakdown of the same ledger rows included in `totals`.
    #[serde(default)]
    pub agent_targets: Vec<AgentTargetUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorUsage {
    pub executor_kind: String,
    pub attempts: i64,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTargetUsage {
    pub target_key: String,
    pub attempts: i64,
    pub estimated_cost_usd_micros: Option<i64>,
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PulsationUsage {
    pub pulsation_id: String,
    pub name: String,
    pub totals: UsageTotals,
    pub models: Vec<ModelUsage>,
}

const TOTALS_SQL: &str = "SUM(input_tokens),SUM(output_tokens),SUM(total_tokens),
    SUM(cache_read_tokens),SUM(cache_write_tokens),SUM(reasoning_tokens),COUNT(*),
    COALESCE(SUM(quality='partial'),0),COALESCE(SUM(quality='unavailable'),0)";
const FILTER_SQL: &str = "(?1 IS NULL OR project_id=?1) AND (?2 IS NULL OR transport=?2)
    AND (?3 IS NULL OR julianday(started_at)>=julianday(?3))
    AND julianday(started_at)<=julianday(?4) AND (?5 IS NULL OR pulsation_id=?5)";

fn read_totals(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<UsageTotals> {
    Ok(UsageTotals {
        input_tokens: row.get(offset)?,
        output_tokens: row.get(offset + 1)?,
        total_tokens: row.get(offset + 2)?,
        cache_read_tokens: row.get(offset + 3)?,
        cache_write_tokens: row.get(offset + 4)?,
        reasoning_tokens: row.get(offset + 5)?,
        records: row.get(offset + 6)?,
        partial_records: row.get(offset + 7)?,
        unavailable_records: row.get(offset + 8)?,
    })
}

pub fn query_usage(path: &Path, query: &UsageQuery) -> rusqlite::Result<UsageSummary> {
    query_usage_at(&connection(path, true)?, query, Utc::now())
}

fn query_usage_at(
    db: &Connection,
    query: &UsageQuery,
    now: DateTime<Utc>,
) -> rusqlite::Result<UsageSummary> {
    let since = match query.period {
        UsagePeriod::AllTime => None,
        UsagePeriod::Last7Days => Some(now.date_naive() - chrono::Duration::days(6)),
        UsagePeriod::Last30Days => Some(now.date_naive() - chrono::Duration::days(29)),
    }
    .map(|d| format!("{d}T00:00:00Z"));
    let transport = query.transport.map(UsageTransport::as_str);
    let until = now.to_rfc3339();
    let args = params![
        query.project_id,
        transport,
        since,
        until,
        query.pulsation_id
    ];
    // A single snapshot makes cards, table and graph agree during active calls.
    let transaction = db.unchecked_transaction()?;
    let totals = transaction.query_row(
        &format!("SELECT {TOTALS_SQL} FROM all_token_usage_records WHERE {FILTER_SQL}"),
        args,
        |row| read_totals(row, 0),
    )?;
    let models = {
        let mut statement = transaction.prepare(&format!(
            "SELECT provider_id,COALESCE(observed_model,requested_model,'Unknown model') AS model,
             CASE WHEN observed_model IS NOT NULL THEN 'observed' WHEN requested_model IS NOT NULL
             THEN 'requested' ELSE 'unknown' END AS model_source,transport,{TOTALS_SQL}
             FROM all_token_usage_records WHERE {FILTER_SQL}
             GROUP BY provider_id,model,model_source,transport
             ORDER BY SUM(total_tokens) DESC,provider_id,model,transport,model_source"
        ))?;
        statement
            .query_map(args, |row| {
                Ok(ModelUsage {
                    provider_id: row.get(0)?,
                    model_id: row.get(1)?,
                    model_source: row.get(2)?,
                    transport: if row.get::<_, String>(3)? == "cli" {
                        UsageTransport::Cli
                    } else {
                        UsageTransport::Api
                    },
                    totals: read_totals(row, 4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut series = {
        let format = if query.period == UsagePeriod::AllTime {
            "%Y-%m"
        } else {
            "%Y-%m-%d"
        };
        let mut statement = transaction.prepare(&format!(
            "SELECT strftime('{format}',started_at) AS period,{TOTALS_SQL}
             FROM all_token_usage_records WHERE {FILTER_SQL} GROUP BY period ORDER BY period"
        ))?;
        statement
            .query_map(args, |row| {
                Ok(UsageBucket {
                    period: row.get(0)?,
                    totals: read_totals(row, 1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    if query.period != UsagePeriod::AllTime && totals.records > 0 {
        let days = if query.period == UsagePeriod::Last7Days {
            7
        } else {
            30
        };
        let mut by_day: std::collections::BTreeMap<_, _> = series
            .into_iter()
            .map(|bucket| (bucket.period.clone(), bucket))
            .collect();
        series = (0..days)
            .rev()
            .map(|offset| {
                let period = (now.date_naive() - chrono::Duration::days(offset)).to_string();
                by_day.remove(&period).unwrap_or(UsageBucket {
                    period,
                    totals: UsageTotals::default(),
                })
            })
            .collect();
    }
    let projects = {
        let mut statement = transaction.prepare(
            "SELECT p.id,p.name FROM projects p
             UNION SELECT r.project_id,r.project_name FROM retained_usage_projects r
             UNION SELECT DISTINCT r.project_id,'Deleted project' FROM token_usage_records r
             WHERE r.project_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM projects p WHERE p.id=r.project_id)
             ORDER BY 2,1")?;
        statement
            .query_map([], |row| {
                Ok(UsageProject {
                    id: row.get(0)?,
                    name: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let without_pulsation = transaction.query_row(
        &format!("SELECT {TOTALS_SQL} FROM all_token_usage_records WHERE {FILTER_SQL} AND pulsation_id IS NULL"),
        args, |row| read_totals(row, 0),
    )?;
    let mut pulsations = {
        let mut statement = transaction.prepare(&format!(
            "SELECT pulsation_id,COALESCE((SELECT feature_name FROM pulsations p WHERE p.id=r.pulsation_id),'Deleted pulsation'),{TOTALS_SQL}
             FROM all_token_usage_records r WHERE {FILTER_SQL} AND pulsation_id IS NOT NULL
             GROUP BY pulsation_id ORDER BY SUM(total_tokens) DESC,pulsation_id"
        ))?;
        statement
            .query_map(args, |row| {
                Ok(PulsationUsage {
                    pulsation_id: row.get(0)?,
                    name: row.get(1)?,
                    totals: read_totals(row, 2)?,
                    models: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // One grouped query for all pulsation/model pairs, rather than one read per row.
    {
        let mut statement = transaction.prepare(&format!(
            "SELECT pulsation_id,provider_id,COALESCE(observed_model,requested_model,'Unknown model') AS model,
             CASE WHEN observed_model IS NOT NULL THEN 'observed' WHEN requested_model IS NOT NULL
             THEN 'requested' ELSE 'unknown' END AS model_source,transport,{TOTALS_SQL}
             FROM all_token_usage_records WHERE {FILTER_SQL} AND pulsation_id IS NOT NULL
             GROUP BY pulsation_id,provider_id,model,model_source,transport
             ORDER BY SUM(total_tokens) DESC,provider_id,model,transport,model_source"
        ))?;
        let indices: std::collections::HashMap<_, _> = pulsations
            .iter()
            .enumerate()
            .map(|(index, pulse)| (pulse.pulsation_id.clone(), index))
            .collect();
        for entry in statement.query_map(args, |row| {
            Ok((
                row.get::<_, String>(0)?,
                ModelUsage {
                    provider_id: row.get(1)?,
                    model_id: row.get(2)?,
                    model_source: row.get(3)?,
                    transport: if row.get::<_, String>(4)? == "cli" {
                        UsageTransport::Cli
                    } else {
                        UsageTransport::Api
                    },
                    totals: read_totals(row, 5)?,
                },
            ))
        })? {
            let (id, model) = entry?;
            if let Some(index) = indices.get(&id) {
                pulsations[*index].models.push(model);
            }
        }
    }
    let executors = transaction
        .prepare(&format!(
            "SELECT executor_kind,COUNT(DISTINCT attempt_id),{TOTALS_SQL}
             FROM all_token_usage_records WHERE {FILTER_SQL} AND executor_kind IS NOT NULL
             GROUP BY executor_kind ORDER BY SUM(total_tokens) DESC,executor_kind"
        ))?
        .query_map(args, |row| {
            Ok(ExecutorUsage {
                executor_kind: row.get(0)?,
                attempts: row.get(1)?,
                totals: read_totals(row, 2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let agent_targets = transaction
        .prepare(&format!(
            "SELECT target_key,COUNT(DISTINCT source_id),{TOTALS_SQL},
                    CAST(ROUND(SUM(estimated_cost_usd) * 1000000.0) AS INTEGER)
             FROM (
               SELECT r.*,
                 CASE WHEN e.free_form_slot_id IS NOT NULL
                   THEN 'free_form:' || e.free_form_slot_id
                   ELSE 'canonical:' || e.agent_role END AS target_key,
                 a.estimated_cost_usd
               FROM all_token_usage_records r
               JOIN llm_attempts a ON r.source_kind='llm_attempt' AND r.source_id=a.id
               JOIN llm_route_executions e ON e.id=a.execution_id
             )
             WHERE {FILTER_SQL} AND target_key IS NOT NULL
             GROUP BY target_key ORDER BY SUM(total_tokens) DESC,target_key"
        ))?
        .query_map(args, |row| {
            Ok(AgentTargetUsage {
                target_key: row.get(0)?,
                attempts: row.get(1)?,
                totals: read_totals(row, 2)?,
                estimated_cost_usd_micros: row.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    transaction.commit()?;
    Ok(UsageSummary {
        pulsations,
        without_pulsation,
        totals,
        models,
        series,
        projects,
        executors,
        agent_targets,
    })
}

#[cfg(test)]
mod tests;
