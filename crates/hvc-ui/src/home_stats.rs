//! Home's numbers. Seven days of token usage are read from the usage ledger off the UI thread;
//! run and pulsation timings are derived from the agents and pulsations snapshots the shell
//! already keeps fresh, so the charts cost no extra polling.

use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use gpui::Context;
use hvc_core::llm::AgentRole;
use hvc_core::persistence_service::{Pulsation, PulsationStatus};
use hvc_core::terminals::{TerminalRun, TerminalRunStatus, TerminalsSnapshot};
use hvc_core::usage::{UsagePeriod, UsageQuery, UsageSummary};

use crate::app_model::{AppModel, AppView};

/// A loaded usage read is reused for this long while Home stays open; run and turn completions
/// refresh it sooner.
const USAGE_STALE_AFTER: Duration = Duration::from_secs(60);
/// Days the token and run charts cover (the ledger's own `Last7Days` window, UTC days).
pub(crate) const WINDOW_DAYS: i64 = 7;
/// Pulsations in the duration chart, oldest to newest.
pub(crate) const PULSATION_BARS: usize = 8;
/// Models named in the share bar; the rest are summed as "Other".
pub(crate) const MODEL_SLICES: usize = 5;

/// The seven-day usage read for the open project.
#[derive(Debug, Default)]
pub(crate) struct HomeStatsState {
    pub(crate) usage: Option<UsageSummary>,
    pub(crate) project_id: Option<String>,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    loaded_at: Option<Instant>,
    in_flight: bool,
    requested: bool,
    refresh_pending: bool,
}

impl AppModel {
    /// Loads Home's seven-day usage for the open project. Cheap to call: a fresh read is reused
    /// (unless `force`), and a call while one is in flight queues exactly one more.
    pub(crate) fn refresh_home_stats(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.is_disposed() {
            return;
        }
        let project_id = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        if self.home_stats.project_id != project_id {
            self.home_stats = HomeStatsState {
                project_id: project_id.clone(),
                ..Default::default()
            };
        }
        let Some(project_id) = project_id else {
            return;
        };
        let fresh = self
            .home_stats
            .loaded_at
            .is_some_and(|at| at.elapsed() < USAGE_STALE_AFTER);
        if fresh && !force {
            return;
        }
        if self.home_stats.in_flight {
            self.home_stats.requested = true;
            return;
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        self.home_stats.in_flight = true;
        self.home_stats.requested = false;
        self.home_stats.loading = true;
        let query = UsageQuery {
            project_id: Some(project_id.clone()),
            period: UsagePeriod::Last7Days,
            ..Default::default()
        };
        let runtime = core.runtime_handle().clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || {
                    hvc_core::usage::query_usage(core.persistence().database_path(), &query)
                })
                .await
                .map_err(|e| format!("Usage worker failed: {e}"))
                .and_then(|result| result.map_err(|e| format!("Unable to load token usage: {e}")));
            let _ = this.update(cx, |model, cx| {
                if model.is_disposed() {
                    return;
                }
                let state = &mut model.home_stats;
                state.in_flight = false;
                state.loading = false;
                let again = std::mem::take(&mut state.requested);
                // A project switch while reading leaves the answer for the old project unused.
                if state.project_id.as_deref() == Some(project_id.as_str()) {
                    match result {
                        Ok(summary) => {
                            state.usage = Some(summary);
                            state.error = None;
                            state.loaded_at = Some(Instant::now());
                        }
                        Err(error) => state.error = Some(error),
                    }
                }
                if again {
                    model.refresh_home_stats(true, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// After a usage-changing event: refresh Home's numbers when Home is on screen (debounced
    /// like the other usage readers), otherwise just mark them stale for the next visit.
    pub(crate) fn schedule_home_stats_refresh(&mut self, cx: &mut Context<Self>) {
        self.home_stats.loaded_at = None;
        if self.active_view() != AppView::Home || self.home_stats.refresh_pending {
            return;
        }
        self.home_stats.refresh_pending = true;
        let timer = cx.background_executor().timer(Duration::from_millis(350));
        cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| {
                model.home_stats.refresh_pending = false;
                if model.active_view() == AppView::Home {
                    model.refresh_home_stats(true, cx);
                }
            });
        })
        .detach();
    }
}

/// One UTC day of token usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DayTokens {
    pub(crate) date: NaiveDate,
    pub(crate) input: i64,
    pub(crate) output: i64,
    /// At least `input + output`; cache and reasoning are already inside those two.
    pub(crate) total: i64,
    /// Some records of the day were partial or unavailable.
    pub(crate) incomplete: bool,
}

/// The last `WINDOW_DAYS` UTC days ending `today`, oldest first; days without records are zero.
pub(crate) fn token_days(summary: Option<&UsageSummary>, today: NaiveDate) -> Vec<DayTokens> {
    (0..WINDOW_DAYS)
        .rev()
        .map(|offset| {
            let date = today - chrono::Duration::days(offset);
            let key = date.to_string();
            let totals = summary
                .and_then(|summary| summary.series.iter().find(|bucket| bucket.period == key))
                .map(|bucket| &bucket.totals);
            let input = totals.and_then(|t| t.input_tokens).unwrap_or(0).max(0);
            let output = totals.and_then(|t| t.output_tokens).unwrap_or(0).max(0);
            let total = totals
                .and_then(|t| t.total_tokens)
                .unwrap_or(0)
                .max(input + output);
            DayTokens {
                date,
                input,
                output,
                total,
                incomplete: totals
                    .is_some_and(|t| t.partial_records > 0 || t.unavailable_records > 0),
            }
        })
        .collect()
}

/// A model's slice of the week's tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelShare {
    pub(crate) label: String,
    pub(crate) tokens: i64,
    /// The summed tail beyond `MODEL_SLICES`.
    pub(crate) other: bool,
}

/// Tokens per model (the same model over API and CLI counts once), largest first, with the
/// tail folded into "Other".
pub(crate) fn model_shares(summary: Option<&UsageSummary>) -> Vec<ModelShare> {
    let mut merged: Vec<(String, i64)> = Vec::new();
    for usage in summary.map(|s| s.models.as_slice()).unwrap_or_default() {
        let tokens = usage.totals.total_tokens.unwrap_or_else(|| {
            usage.totals.input_tokens.unwrap_or(0) + usage.totals.output_tokens.unwrap_or(0)
        });
        if tokens <= 0 {
            continue;
        }
        let label = if usage.model_id.trim().is_empty() {
            usage.provider_id.clone()
        } else {
            usage.model_id.clone()
        };
        match merged.iter_mut().find(|(existing, _)| *existing == label) {
            Some((_, sum)) => *sum += tokens,
            None => merged.push((label, tokens)),
        }
    }
    merged.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut shares: Vec<ModelShare> = merged
        .iter()
        .take(MODEL_SLICES)
        .map(|(label, tokens)| ModelShare {
            label: label.clone(),
            tokens: *tokens,
            other: false,
        })
        .collect();
    let rest: i64 = merged
        .iter()
        .skip(MODEL_SLICES)
        .map(|(_, tokens)| tokens)
        .sum();
    if rest > 0 {
        shares.push(ModelShare {
            label: "Other".into(),
            tokens: rest,
            other: true,
        });
    }
    shares
}

/// How long one agent's runs took over the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentTiming {
    pub(crate) role: AgentRole,
    pub(crate) name: &'static str,
    pub(crate) runs: usize,
    pub(crate) failed: usize,
    pub(crate) median_secs: u64,
    pub(crate) longest_secs: u64,
}

/// Finished runs per canonical agent that started inside the window, in pipeline order; agents
/// without such a run are left out. The snapshot holds each agent's last 20 runs.
pub(crate) fn agent_timings(
    snapshot: Option<&TerminalsSnapshot>,
    now: DateTime<Utc>,
) -> Vec<AgentTiming> {
    let since = now - chrono::Duration::days(WINDOW_DAYS);
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    snapshot
        .agents
        .iter()
        .filter_map(|agent| {
            let finished: Vec<(&TerminalRun, u64)> = agent
                .recent_runs
                .iter()
                .filter(|run| run.status.is_terminal())
                .filter_map(|run| {
                    let started = parse_utc(run.started_at.as_deref()?)?;
                    let finished = parse_utc(run.finished_at.as_deref()?)?;
                    (started >= since).then(|| (run, seconds_between(started, finished)))
                })
                .collect();
            if finished.is_empty() {
                return None;
            }
            let mut durations: Vec<u64> = finished.iter().map(|(_, secs)| *secs).collect();
            durations.sort_unstable();
            let role = agent.config.agent_role;
            Some(AgentTiming {
                role,
                name: hvc_core::terminals::canonical_agent(role).name,
                runs: finished.len(),
                failed: finished
                    .iter()
                    .filter(|(run, _)| {
                        matches!(
                            run.status,
                            TerminalRunStatus::Failed | TerminalRunStatus::Interrupted
                        )
                    })
                    .count(),
                median_secs: median(&durations),
                longest_secs: durations.last().copied().unwrap_or(0),
            })
        })
        .collect()
}

/// A pulsation's wall-clock duration: to its last update once finished, to `now` otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PulsationTiming {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) status: PulsationStatus,
    pub(crate) secs: u64,
    pub(crate) finished: bool,
}

/// The newest `PULSATION_BARS` pulsations that started (idle ones have no duration yet),
/// oldest first.
pub(crate) fn pulsation_timings(
    pulsations: &[Pulsation],
    now: DateTime<Utc>,
) -> Vec<PulsationTiming> {
    let mut started: Vec<(&Pulsation, DateTime<Utc>)> = pulsations
        .iter()
        .filter(|pulsation| pulsation.status != PulsationStatus::Idle)
        .filter_map(|pulsation| Some((pulsation, parse_utc(&pulsation.created_at)?)))
        .collect();
    started.sort_by_key(|(_, created)| *created);
    let skip = started.len().saturating_sub(PULSATION_BARS);
    started
        .into_iter()
        .skip(skip)
        .map(|(pulsation, created)| {
            let finished = matches!(
                pulsation.status,
                PulsationStatus::Done | PulsationStatus::Failed
            );
            let end = if finished {
                parse_utc(&pulsation.updated_at).unwrap_or(now)
            } else {
                now
            };
            PulsationTiming {
                id: pulsation.id.clone(),
                name: pulsation.feature_name.trim().to_string(),
                status: pulsation.status,
                secs: seconds_between(created, end),
                finished,
            }
        })
        .collect()
}

/// Median wall-clock duration of the finished pulsations among `timings`.
pub(crate) fn typical_pulsation_secs(timings: &[PulsationTiming]) -> Option<u64> {
    let mut finished: Vec<u64> = timings
        .iter()
        .filter(|timing| timing.status == PulsationStatus::Done)
        .map(|timing| timing.secs)
        .collect();
    if finished.is_empty() {
        return None;
    }
    finished.sort_unstable();
    Some(median(&finished))
}

/// Time each agent has spent on one pulsation so far (a running run counts until `now`), in the
/// order the agents first started.
pub(crate) fn stage_times(
    snapshot: Option<&TerminalsSnapshot>,
    pulsation_id: &str,
    now: DateTime<Utc>,
) -> Vec<(AgentRole, u64)> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let mut stages: Vec<(AgentRole, DateTime<Utc>, u64)> = Vec::new();
    for agent in &snapshot.agents {
        for run in agent
            .recent_runs
            .iter()
            .filter(|run| run.pulsation_id.as_deref() == Some(pulsation_id))
        {
            let Some(started) = run.started_at.as_deref().and_then(parse_utc) else {
                continue;
            };
            let end = if run.status.is_terminal() {
                run.finished_at
                    .as_deref()
                    .and_then(parse_utc)
                    .unwrap_or(started)
            } else {
                now
            };
            let secs = seconds_between(started, end);
            let role = agent.config.agent_role;
            match stages.iter_mut().find(|(existing, ..)| *existing == role) {
                Some((_, first, total)) => {
                    *first = (*first).min(started);
                    *total += secs;
                }
                None => stages.push((role, started, secs)),
            }
        }
    }
    stages.sort_by_key(|(_, first, _)| *first);
    stages
        .into_iter()
        .map(|(role, _, secs)| (role, secs))
        .collect()
}

/// "45s", "3m 12s", "1h 04m", "2d 3h".
pub(crate) fn format_secs(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3_600..86_400 => format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3_600),
    }
}

/// Ledger and runtime timestamps: RFC 3339, or SQLite's `YYYY-MM-DD HH:MM:SS` in UTC.
pub(crate) fn parse_utc(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

fn seconds_between(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    (end - start).num_seconds().max(0) as u64
}

fn median(sorted: &[u64]) -> u64 {
    match sorted.len() {
        0 => 0,
        len if len % 2 == 1 => sorted[len / 2],
        len => (sorted[len / 2 - 1] + sorted[len / 2]) / 2,
    }
}

#[cfg(test)]
#[path = "home_stats_tests.rs"]
mod tests;
