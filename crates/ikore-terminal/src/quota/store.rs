//! Quota history (`quota_snapshots`, V48) and the quota switches kept in `runtime_settings`.
//! Rows carry only validated numbers and labels: no token, account id, e-mail or reply body.

use std::path::PathBuf;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use super::model::{QuotaSnapshot, QuotaSource, QuotaWindow};
use crate::terminals::CliBackend;

/// At most one periodic sample per backend in this interval (D5).
pub const SAMPLE_INTERVAL: Duration = Duration::minutes(5);
pub const RETENTION: Duration = Duration::days(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleKind {
    Sample,
    RunStart,
    RunEnd,
}

impl SampleKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Sample => "sample",
            Self::RunStart => "run_start",
            Self::RunEnd => "run_end",
        }
    }
}

/// Used-percent change of one window over one CLI run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunQuotaDelta {
    pub backend: CliBackend,
    pub scope: Option<String>,
    pub window_minutes: Option<u32>,
    pub start_percent: f64,
    pub end_percent: f64,
    /// Another run of the same CLI overlapped this one, so the change is shared.
    pub approximate: bool,
}

impl RunQuotaDelta {
    pub fn delta(&self) -> f64 {
        ((self.end_percent - self.start_percent) * 10.0).round() / 10.0
    }
}

#[derive(Debug, Clone)]
pub struct QuotaStore {
    db_path: PathBuf,
}

pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.to_utc())
}

impl QuotaStore {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }

    pub fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

    fn connection(&self) -> rusqlite::Result<Connection> {
        let connection = Connection::open(&self.db_path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    /// Stores every window of `snapshot`. A periodic sample is skipped when the backend already
    /// has one inside `SAMPLE_INTERVAL`; run-boundary rows are always written. Returns the
    /// number of rows written.
    pub fn record(
        &self,
        snapshot: &QuotaSnapshot,
        kind: SampleKind,
        run: Option<(&str, Option<&str>)>,
    ) -> rusqlite::Result<usize> {
        if snapshot.windows.is_empty() {
            return Ok(0);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if kind == SampleKind::Sample {
            let recent: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM quota_snapshots
                 WHERE backend=?1 AND kind='sample' AND observed_at>?2)",
                params![
                    snapshot.backend.as_str(),
                    timestamp(snapshot.observed_at - SAMPLE_INTERVAL)
                ],
                |row| row.get(0),
            )?;
            if recent {
                return Ok(0);
            }
        }
        let (run_id, pulsation_id) = match run {
            Some((run_id, pulsation_id)) => (Some(run_id), pulsation_id),
            None => (None, None),
        };
        let mut written = 0;
        for window in &snapshot.windows {
            written += transaction.execute(
                "INSERT INTO quota_snapshots
                 (backend, scope, window_minutes, used_percent, resets_at, source, plan,
                  observed_at, kind, run_id, pulsation_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    snapshot.backend.as_str(),
                    window.scope,
                    window.window_minutes,
                    window.used_percent,
                    window.resets_at.map(timestamp),
                    snapshot.source.as_str(),
                    snapshot.plan,
                    timestamp(snapshot.observed_at),
                    kind.as_str(),
                    run_id,
                    pulsation_id,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(written)
    }

    /// The most recent observation of `backend`, rebuilt from its rows. Used after a restart;
    /// the caller decides whether it is still current.
    pub fn latest(&self, backend: CliBackend) -> rusqlite::Result<Option<QuotaSnapshot>> {
        let connection = self.connection()?;
        let Some(observed_at) = connection
            .query_row(
                "SELECT MAX(observed_at) FROM quota_snapshots WHERE backend=?1",
                [backend.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT scope, window_minutes, used_percent, resets_at, source, plan
             FROM quota_snapshots WHERE backend=?1 AND observed_at=?2 ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![backend.as_str(), observed_at], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<u32>>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let Some(observed_at) = parse_timestamp(&observed_at) else {
            return Ok(None);
        };
        let mut windows = Vec::new();
        let mut source = None;
        let mut plan = None;
        for (scope, minutes, used, resets_at, row_source, row_plan) in rows {
            let resets_at = match resets_at.as_deref().map(parse_timestamp) {
                Some(None) => continue,
                Some(Some(value)) => Some(value),
                None => None,
            };
            let Ok(window) = QuotaWindow::new(scope.as_deref(), used, minutes, resets_at) else {
                continue;
            };
            // Keep only the windows of one source, should two have landed at the same instant.
            let row_source = QuotaSource::parse(&row_source);
            if source.is_none() {
                source = row_source;
                plan = row_plan;
            } else if source != row_source {
                continue;
            }
            windows.push(window);
        }
        Ok(source.map(|source| {
            QuotaSnapshot::new(backend, plan.as_deref(), windows, source, observed_at)
        }))
    }

    /// `(observed_at, used_percent)` of one window since `since`, oldest first.
    pub fn samples(
        &self,
        backend: CliBackend,
        window: &QuotaWindow,
        since: DateTime<Utc>,
    ) -> rusqlite::Result<Vec<(DateTime<Utc>, f64)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT observed_at, used_percent FROM quota_snapshots
             WHERE backend=?1 AND scope IS ?2 AND window_minutes IS ?3 AND observed_at>=?4
             ORDER BY observed_at",
        )?;
        let rows = statement
            .query_map(
                params![
                    backend.as_str(),
                    window.scope,
                    window.window_minutes,
                    timestamp(since)
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(at, used)| Some((parse_timestamp(&at)?, used)))
            .collect())
    }

    /// Per-window change between a run's start and end snapshots. Windows that reset during the
    /// run, or are missing at either end, are left out rather than guessed.
    pub fn run_deltas(&self, run_id: &str) -> rusqlite::Result<Vec<RunQuotaDelta>> {
        Self::run_deltas_with(&self.connection()?, run_id)
    }

    /// `run_deltas` for many runs over one connection; runs without attribution are omitted.
    pub fn run_deltas_many<'a>(
        &self,
        run_ids: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<std::collections::HashMap<String, Vec<RunQuotaDelta>>> {
        let connection = self.connection()?;
        let mut deltas = std::collections::HashMap::new();
        for run_id in run_ids {
            let found = Self::run_deltas_with(&connection, run_id)?;
            if !found.is_empty() {
                deltas.insert(run_id.to_string(), found);
            }
        }
        Ok(deltas)
    }

    fn run_deltas_with(
        connection: &Connection,
        run_id: &str,
    ) -> rusqlite::Result<Vec<RunQuotaDelta>> {
        let mut statement = connection.prepare_cached(
            "SELECT backend, scope, window_minutes, used_percent, resets_at, kind, observed_at
             FROM quota_snapshots WHERE run_id=?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map([run_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<u32>>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        for row in rows {
            match row.5.as_str() {
                "run_start" => starts.push(row),
                "run_end" => ends.push(row),
                _ => {}
            }
        }
        let (Some(first), Some(last)) = (starts.first(), ends.last()) else {
            return Ok(Vec::new());
        };
        let Some(backend) = CliBackend::parse(&first.0) else {
            return Ok(Vec::new());
        };
        let (started_at, ended_at) = (first.6.clone(), last.6.clone());
        let approximate: bool = connection.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM quota_snapshots other
               WHERE other.backend=?1 AND other.run_id IS NOT NULL AND other.run_id<>?2
                 AND other.kind='run_start' AND other.observed_at<=?4
                 AND NOT EXISTS(
                   SELECT 1 FROM quota_snapshots finished
                   WHERE finished.run_id=other.run_id AND finished.kind='run_end'
                     AND finished.observed_at<?3))",
            params![first.0, run_id, started_at, ended_at],
            |row| row.get(0),
        )?;
        let mut deltas = Vec::new();
        for start in &starts {
            let Some(end) = ends
                .iter()
                .rev()
                .find(|end| end.1 == start.1 && end.2 == start.2)
            else {
                continue;
            };
            if start.4 != end.4 {
                continue;
            }
            deltas.push(RunQuotaDelta {
                backend,
                scope: start.1.clone(),
                window_minutes: start.2,
                start_percent: start.3,
                end_percent: end.3,
                approximate,
            });
        }
        Ok(deltas)
    }

    pub fn has_run_start(&self, run_id: &str) -> rusqlite::Result<bool> {
        self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM quota_snapshots WHERE run_id=?1 AND kind='run_start')",
            [run_id],
            |row| row.get(0),
        )
    }

    /// Deletes rows older than the retention period. Returns the number removed.
    pub fn prune(&self, now: DateTime<Utc>) -> rusqlite::Result<usize> {
        self.connection()?.execute(
            "DELETE FROM quota_snapshots WHERE observed_at<?1",
            [timestamp(now - RETENTION)],
        )
    }

    pub fn setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.connection()?
            .query_row(
                "SELECT value FROM runtime_settings WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.connection()?.execute(
            "INSERT INTO runtime_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![key, value, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}
