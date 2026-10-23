//! Versioned JSONL accounting: final snapshots for Claude/Gemini, turns for
//! Codex, uniquely identified step-finish parts for `OpenCode`.
use super::{TokenCounts, UsageContext, UsageMeasurement, record_usage, record_usage_on};
use crate::terminals::{CliBackend, sanitize_terminal_text_bounded};
use rusqlite::params;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

pub use hvc_terminal::usage::CliUsageTracker;

pub struct CliUsageRecorder {
    path: PathBuf,
    context: UsageContext,
    backend: CliBackend,
    tracker: Mutex<CliUsageTracker>,
}

impl CliUsageRecorder {
    pub fn new(path: &Path, context: UsageContext, backend: CliBackend) -> rusqlite::Result<Self> {
        record_usage(path, &context, "unavailable", &UsageMeasurement::default())?;
        Ok(Self {
            path: path.to_owned(),
            context,
            backend,
            tracker: Mutex::new(CliUsageTracker::default()),
        })
    }

    pub fn observe_line(&self, line: &[u8]) -> rusqlite::Result<()> {
        // Only framed and bounded stdout is accepted; stderr is never a source.
        if line.len() > 256 * 1024 {
            return self.mark_incomplete();
        }
        let Ok(value) =
            serde_json::from_str::<Value>(&sanitize_terminal_text_bounded(line, line.len()))
        else {
            if line
                .iter()
                .copied()
                .find(|byte| !byte.is_ascii_whitespace())
                == Some(b'{')
            {
                return self.mark_incomplete();
            }
            return Ok(());
        };
        let (replace, changes) = self
            .tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .observe(self.backend, &value);
        if changes.is_empty() {
            return Ok(());
        }
        let db = super::connection(&self.path, false)?;
        let transaction = db.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM token_usage_records WHERE source_kind=?1 AND source_id=?2 AND (?3 OR unit_key='unavailable')",
            params![self.context.source_kind,self.context.source_id,replace])?;
        for (key, measurement) in changes {
            // Provider identifiers may contain arbitrary text. Persist only a
            // stable digest as the deduplication key, never the raw identifier.
            let key = format!("unit:{:x}", Sha256::digest(key.as_bytes()));
            record_usage_on(&transaction, &self.context, &key, &measurement)?;
        }
        transaction.commit()
    }

    pub fn totals(&self) -> TokenCounts {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .totals()
    }

    /// Output tokens the stream has shown so far; see
    /// [`CliUsageTracker::output_tokens_so_far`].
    pub fn output_tokens_so_far(&self) -> u64 {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output_tokens_so_far()
    }

    /// Records what a Claude stream showed before the host stopped it. Claude
    /// reports usage only in its final `result`, so without this a run ended
    /// early (for spending past its output ceiling, for instance) would keep
    /// the empty `unavailable` placeholder and its cost would not exist in
    /// the ledger. Does nothing when `result` already arrived or the stream
    /// never opened a message.
    pub fn record_live_partial(&self) -> rusqlite::Result<()> {
        let measurement = {
            let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            tracker.live_partial()
        };
        let Some(measurement) = measurement else {
            return Ok(());
        };
        let db = super::connection(&self.path, false)?;
        let transaction = db.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM token_usage_records WHERE source_kind=?1 AND source_id=?2 AND unit_key='unavailable'",
            params![self.context.source_kind, self.context.source_id],
        )?;
        let key = format!("unit:{:x}", Sha256::digest(b"live"));
        record_usage_on(&transaction, &self.context, &key, &measurement)?;
        transaction.commit()
    }

    fn mark_incomplete(&self) -> rusqlite::Result<()> {
        self.tracker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .mark_incomplete();
        self.mark_partial()
    }

    pub fn mark_partial(&self) -> rusqlite::Result<()> {
        super::connection(&self.path,false)?.execute(
            "UPDATE token_usage_records SET quality='partial' WHERE source_kind=?1 AND source_id=?2 AND quality='complete'",
            params![self.context.source_kind,self.context.source_id])?;
        Ok(())
    }
}
