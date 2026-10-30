//! Application capabilities supplied to the terminal subsystem.

use crate::{
    repository_context::RepositoryContext,
    sanitization::{SanitizationPolicySnapshot, SanitizationStore},
    secret_redactor::{self, PreparedModelInput, SANITIZATION_POLICY_VERSION},
    usage::{TokenCounts, UsageContext, cli::CliUsageRecorder},
};
use hvc_terminal::{
    host::{
        ContextRedaction, InputProof, PreparedModelInput as OpaqueInput, RedactionResult,
        SnapshotFile, SnapshotPreparation, TerminalHost, TerminalUsageRecorder,
    },
    terminals::{CliBackend, TerminalError, TerminalRun},
};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use std::{
    any::Any,
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub(crate) struct CoreTerminalHost;

impl InputProof for PreparedModelInput {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn system_prompt(&self) -> &str {
        self.system_prompt()
    }
    fn user_prompt(&self) -> &str {
        self.user_prompt()
    }
    fn policy_identity(&self) -> &str {
        self.policy_identity()
    }
    fn redaction_count(&self) -> u64 {
        self.redaction_count()
    }
    fn categories(&self) -> &[String] {
        self.categories()
    }
    fn original_bytes(&self) -> usize {
        self.original_bytes()
    }
}

impl TerminalUsageRecorder for CliUsageRecorder {
    fn observe_line(&self, line: &[u8]) -> rusqlite::Result<()> {
        self.observe_line(line)
    }
    fn totals(&self) -> TokenCounts {
        self.totals()
    }
    fn output_tokens_so_far(&self) -> u64 {
        self.output_tokens_so_far()
    }
    fn record_live_partial(&self) -> rusqlite::Result<()> {
        self.record_live_partial()
    }
    fn mark_partial(&self) -> rusqlite::Result<()> {
        self.mark_partial()
    }
}

impl TerminalHost for CoreTerminalHost {
    fn record_run_status(
        &self,
        transaction: &rusqlite::Connection,
        project_id: &str,
        run_id: &str,
        status: hvc_terminal::terminals::TerminalRunStatus,
    ) -> rusqlite::Result<()> {
        crate::exchange::log::record_terminal_run_status(
            transaction,
            project_id,
            run_id,
            status.as_str(),
        )
        .map(|_| ())
        .map_err(|error| match error {
            crate::exchange::log::ExchangeError::Storage(error) => error,
            other => rusqlite::Error::ToSqlConversionFailure(Box::new(other)),
        })
    }
    fn redact_context(&self, text: &str) -> Result<ContextRedaction, String> {
        secret_redactor::redact_context(text)
    }
    fn redact_secrets(&self, text: &str) -> Result<RedactionResult, String> {
        secret_redactor::redact_secrets(text)
    }
    fn sanitize_model_text_counted(&self, text: &str) -> Result<RedactionResult, String> {
        crate::llm::adapter::sanitize_model_text_counted(text)
            .map_err(|_| "CLI JSON sanitization failed.".to_owned())
    }
    fn prepare_input(
        &self,
        system: &str,
        user: &str,
        candidate: Option<OpaqueInput>,
    ) -> Result<OpaqueInput, TerminalError> {
        let concrete = match candidate.as_ref() {
            Some(candidate) => {
                let proof = candidate
                    .as_any()
                    .downcast_ref::<PreparedModelInput>()
                    .ok_or_else(|| {
                        TerminalError::Redaction("invalid_prepared_input_issuer".into())
                    })?;
                proof
                    .verify_matches(system, user)
                    .map_err(TerminalError::Redaction)?;
                proof.clone()
            }
            None => PreparedModelInput::new(system, user).map_err(TerminalError::Redaction)?,
        };
        // Discard the candidate object: only getters on the validated concrete type are trusted.
        Ok(Arc::new(concrete))
    }
    fn record_prepared_input(
        &self,
        db_path: &Path,
        project: &str,
        pulse: Option<&str>,
        call_id: &str,
        candidate: &OpaqueInput,
    ) -> Result<(), TerminalError> {
        let proof = candidate
            .as_any()
            .downcast_ref::<PreparedModelInput>()
            .ok_or_else(|| TerminalError::Redaction("invalid_prepared_input_issuer".into()))?;
        proof.verify().map_err(TerminalError::Redaction)?;
        SanitizationStore::new(db_path)
            .record_input(
                project,
                pulse,
                call_id,
                proof.policy_identity(),
                proof.redaction_count(),
                proof.categories(),
                proof.original_bytes(),
                0,
            )
            .map_err(TerminalError::Redaction)
    }
    fn prepare_snapshot(
        &self,
        db_path: &Path,
        workspace: &Path,
        run: &TerminalRun,
        max_files: usize,
        max_bytes: u64,
    ) -> Result<SnapshotPreparation, TerminalError> {
        let store = SanitizationStore::new(db_path);
        let policy = store
            .policy_for_project(&run.project_id)
            .map_err(TerminalError::Redaction)?;
        validate_cli_input_policy(db_path, run, &policy)?;
        let context = RepositoryContext::new(workspace)
            .map_err(|error| TerminalError::Security(error.to_string()))?;
        let sources = context
            .safe_snapshot_files(max_files, max_bytes)
            .map_err(|error| TerminalError::Security(error.to_string()))?;
        let mut snapshot = SnapshotPreparation {
            policy_identity: policy.identity().to_owned(),
            files: Vec::with_capacity(sources.len()),
            redaction_count: 0,
            categories: Vec::new(),
            applied_exceptions: Vec::new(),
            original_bytes: 0,
        };
        let mut categories = HashSet::new();
        for source in sources {
            if !secret_redactor::redact_context(&source.relative_path)
                .map_err(TerminalError::Redaction)?
                .spans
                .is_empty()
            {
                return Err(TerminalError::Security(
                    "A snapshot filename contained sensitive content".into(),
                ));
            }
            let projected = store
                .sanitize_source_with_policy(&policy, &source.relative_path, &source.content)
                .map_err(TerminalError::Redaction)?;
            snapshot.original_bytes = snapshot.original_bytes.saturating_add(source.content.len());
            snapshot.redaction_count = snapshot
                .redaction_count
                .saturating_add(projected.spans.len() as u64);
            categories.extend(projected.spans.iter().map(|span| span.category.clone()));
            snapshot
                .applied_exceptions
                .extend(projected.applied_exceptions);
            snapshot.files.push(SnapshotFile {
                relative_path: PathBuf::from(source.relative_path),
                original_sha256: Sha256::digest(source.content.as_bytes()).into(),
                projected_content: projected.text,
            });
        }
        snapshot.categories = categories.into_iter().collect();
        snapshot.categories.sort();
        Ok(snapshot)
    }
    fn record_snapshot(
        &self,
        db_path: &Path,
        run: &TerminalRun,
        snapshot: &SnapshotPreparation,
        duration_ms: u64,
    ) -> Result<(), TerminalError> {
        let store = SanitizationStore::new(db_path);
        let call_id = format!("cli-snapshot:{}", run.id);
        store
            .record_input(
                &run.project_id,
                run.pulsation_id.as_deref(),
                &call_id,
                &snapshot.policy_identity,
                snapshot.redaction_count,
                &snapshot.categories,
                snapshot.original_bytes,
                duration_ms,
            )
            .map_err(TerminalError::Redaction)?;
        store
            .record_exception_uses(&call_id, &snapshot.applied_exceptions)
            .map_err(TerminalError::Redaction)
    }
    fn create_usage_recorder(
        &self,
        db_path: &Path,
        context: UsageContext,
        backend: CliBackend,
    ) -> rusqlite::Result<Arc<dyn TerminalUsageRecorder>> {
        Ok(Arc::new(CliUsageRecorder::new(db_path, context, backend)?))
    }
}

fn validate_cli_input_policy(
    db_path: &Path,
    run: &TerminalRun,
    policy: &SanitizationPolicySnapshot,
) -> Result<(), TerminalError> {
    let receipt: Option<String> = Connection::open(db_path)?.query_row(
        "SELECT policy_identity FROM secret_sanitization_receipts WHERE project_id=?1 AND call_id=?2",
        params![run.project_id, run.llm_execution_id.as_deref().unwrap_or(&run.id)],
        |row| row.get(0),
    ).optional()?;
    match receipt.as_deref() {
        Some(identity)
            if identity == policy.identity() || identity == SANITIZATION_POLICY_VERSION =>
        {
            Ok(())
        }
        _ => Err(TerminalError::Redaction(
            "sanitization_policy_changed_before_cli_launch".into(),
        )),
    }
}

#[cfg(test)]
#[path = "terminals_host_tests.rs"]
mod tests;
