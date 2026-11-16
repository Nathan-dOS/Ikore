//! Local trust configuration and sanitization evidence. No matched values are persisted.
use crate::{
    repository_context::RepositoryContext,
    secret_redactor::{
        self, AuthorizedSpan, ContextRedaction, RedactionSpan, SANITIZATION_POLICY_VERSION,
        text_sha256,
    },
};
use chrono::Utc;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRedactionException {
    pub id: String,
    pub project_id: String,
    pub relative_path: String,
    pub detector_id: String,
    pub detector_version: u32,
    pub original_file_sha256: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub reason: String,
    pub created_at: String,
    #[serde(default)]
    pub active: bool,
}
#[derive(Debug, Clone)]
pub struct SanitizationPolicySnapshot {
    pub identity: String,
    pub project_id: String,
    pub exceptions: Vec<SecretRedactionException>,
}
impl SanitizationPolicySnapshot {
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSecretRedactionException {
    pub project_id: String,
    pub relative_path: String,
    pub detector_id: String,
    pub detector_version: u32,
    pub original_file_sha256: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SanitizationFilePreview {
    pub relative_path: String,
    pub original_file_sha256: String,
    pub spans: Vec<RedactionSpan>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizationSummary {
    pub entries_examined: u64,
    pub input_redaction_count: u64,
    #[serde(default)]
    pub output_redaction_count: u64,
    pub examined_bytes: u64,
    #[serde(default)]
    pub exceptions_applied: u64,
    pub duration_ms: u64,
    pub categories: Vec<String>,
}
#[derive(Debug, Clone)]
pub struct SanitizationStore {
    db_path: PathBuf,
}
fn storage_error(_: impl std::fmt::Display) -> String {
    "sanitization_storage_failed".into()
}
/// Derived display/copy text. Operational artifacts and their hashes stay unchanged.
/// JSON is inspected after decoding; failures expose only a neutral omission notice.
pub fn sanitized_observation(text: &str) -> String {
    crate::llm::adapter::sanitize_model_text(text)
        .unwrap_or_else(|_| crate::secret_redactor::OMITTED_CONTENT.to_owned())
}
impl SanitizationStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            db_path: path.as_ref().to_path_buf(),
        }
    }
    fn connection(&self) -> Result<Connection, String> {
        let db = Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(storage_error)?;
        db.pragma_update(None, "foreign_keys", true)
            .map_err(storage_error)?;
        db.busy_timeout(Duration::from_secs(5))
            .map_err(storage_error)?;
        Ok(db)
    }
    fn exceptions_on(
        db: &Connection,
        project: &str,
    ) -> Result<Vec<SecretRedactionException>, String> {
        let mut query = db.prepare("SELECT id,project_id,relative_path,detector_id,detector_version,original_file_sha256,start_byte,end_byte,reason,created_at FROM project_secret_redaction_exceptions WHERE project_id=?1 ORDER BY id").map_err(storage_error)?;
        query
            .query_map([project], |r| {
                Ok(SecretRedactionException {
                    id: r.get(0)?,
                    project_id: r.get(1)?,
                    relative_path: r.get(2)?,
                    detector_id: r.get(3)?,
                    detector_version: r.get(4)?,
                    original_file_sha256: r.get(5)?,
                    start_byte: r.get(6)?,
                    end_byte: r.get(7)?,
                    reason: r.get(8)?,
                    created_at: r.get(9)?,
                    active: false,
                })
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)
    }
    pub fn policy_on(db: &Connection, project: &str) -> Result<SanitizationPolicySnapshot, String> {
        let exceptions = Self::exceptions_on(db, project)?;
        let identities: Vec<_> = exceptions
            .iter()
            .map(|e| {
                (
                    &e.id,
                    &e.relative_path,
                    &e.detector_id,
                    e.detector_version,
                    &e.original_file_sha256,
                    e.start_byte,
                    e.end_byte,
                )
            })
            .collect();
        let identity = text_sha256(&format!(
            "{SANITIZATION_POLICY_VERSION}:{project}:{}",
            serde_json::to_string(&identities).map_err(storage_error)?
        ));
        Ok(SanitizationPolicySnapshot {
            identity,
            project_id: project.into(),
            exceptions,
        })
    }
    pub fn policy_for_project(&self, project: &str) -> Result<SanitizationPolicySnapshot, String> {
        Self::policy_on(&self.connection()?, project)
    }
    fn repository(&self, project: &str) -> Result<RepositoryContext, String> {
        let root: String = self
            .connection()?
            .query_row(
                "SELECT workspace_path FROM projects WHERE id=?1",
                [project],
                |r| r.get(0),
            )
            .map_err(storage_error)?;
        RepositoryContext::new(Path::new(&root))
            .map_err(|_| "sanitization_source_unavailable".into())
    }
    fn normalized_path(path: &str) -> Result<String, String> {
        if path.is_empty()
            || path.contains('\\')
            || path.contains(':')
            || path.starts_with('/')
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err("invalid_sanitization_path".into());
        }
        Ok(path.into())
    }
    pub fn preview_file(
        &self,
        project: &str,
        path: &str,
    ) -> Result<SanitizationFilePreview, String> {
        let path = Self::normalized_path(path)?;
        let original = self
            .repository(project)?
            .read_original_for_sanitization(&path)
            .map_err(|_| "sanitization_source_unavailable".to_string())?;
        Ok(SanitizationFilePreview {
            relative_path: path,
            original_file_sha256: text_sha256(&original),
            spans: secret_redactor::detect_secret_spans(&original)?,
        })
    }
    pub fn create_exception(
        &self,
        request: &CreateSecretRedactionException,
    ) -> Result<SecretRedactionException, String> {
        if request.reason.trim().is_empty()
            || request.reason.len() > 1000
            || !secret_redactor::redact_context(&request.reason)?
                .spans
                .is_empty()
        {
            return Err("invalid_exception_reason".into());
        }
        let preview = self.preview_file(&request.project_id, &request.relative_path)?;
        if preview.original_file_sha256 != request.original_file_sha256
            || !preview.spans.iter().any(|s| {
                s.start_byte == request.start_byte
                    && s.end_byte == request.end_byte
                    && s.detector_id == request.detector_id
                    && s.detector_version == request.detector_version
            })
        {
            return Err("stale_exception_approval".into());
        }
        let exception = SecretRedactionException {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: request.project_id.clone(),
            relative_path: preview.relative_path,
            detector_id: request.detector_id.clone(),
            detector_version: request.detector_version,
            original_file_sha256: preview.original_file_sha256,
            start_byte: request.start_byte,
            end_byte: request.end_byte,
            reason: request.reason.trim().into(),
            created_at: Utc::now().to_rfc3339(),
            active: true,
        };
        self.connection()?.execute("INSERT INTO project_secret_redaction_exceptions(id,project_id,relative_path,detector_id,detector_version,original_file_sha256,start_byte,end_byte,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![exception.id,exception.project_id,exception.relative_path,exception.detector_id,exception.detector_version,exception.original_file_sha256,exception.start_byte as i64,exception.end_byte as i64,exception.reason,exception.created_at]).map_err(storage_error)?;
        Ok(exception)
    }
    pub fn remove_exception(&self, project: &str, id: &str) -> Result<(), String> {
        self.connection()?
            .execute(
                "DELETE FROM project_secret_redaction_exceptions WHERE project_id=?1 AND id=?2",
                params![project, id],
            )
            .map_err(storage_error)?;
        Ok(())
    }
    pub fn list_exceptions(&self, project: &str) -> Result<Vec<SecretRedactionException>, String> {
        let mut exceptions = Self::exceptions_on(&self.connection()?, project)?;
        for e in &mut exceptions {
            e.active = self.preview_file(project, &e.relative_path).is_ok_and(|p| {
                p.original_file_sha256 == e.original_file_sha256
                    && p.spans.iter().any(|s| {
                        s.detector_id == e.detector_id
                            && s.detector_version == e.detector_version
                            && s.start_byte == e.start_byte
                            && s.end_byte == e.end_byte
                    })
            });
        }
        Ok(exceptions)
    }
    pub fn sanitize_source(
        &self,
        project: &str,
        path: &str,
        original: &str,
    ) -> Result<ContextRedaction, String> {
        self.sanitize_source_with_policy(&self.policy_for_project(project)?, path, original)
    }
    pub fn sanitize_source_with_policy(
        &self,
        policy: &SanitizationPolicySnapshot,
        path: &str,
        original: &str,
    ) -> Result<ContextRedaction, String> {
        Self::sanitize_source_projection(policy, path, original)
    }
    pub fn sanitize_source_projection(
        policy: &SanitizationPolicySnapshot,
        path: &str,
        original: &str,
    ) -> Result<ContextRedaction, String> {
        let path = Self::normalized_path(path)?;
        let mut result = secret_redactor::redact_context(original)?;
        let hash = text_sha256(original);
        let mut allowed = Vec::new();
        result.spans.retain(|span| {
            let matched = policy.exceptions.iter().find(|e| {
                e.project_id == policy.project_id
                    && e.relative_path == path
                    && e.original_file_sha256 == hash
                    && e.detector_id == span.detector_id
                    && e.detector_version == span.detector_version
                    && e.start_byte == span.start_byte
                    && e.end_byte == span.end_byte
            });
            if let Some(e) = matched {
                allowed.push((span.clone(), e.id.clone()));
                false
            } else {
                true
            }
        });
        result.text =
            secret_redactor::render_redacted_range(original, &result.spans, 0, original.len())?;
        for (span, id) in allowed {
            let prefix = secret_redactor::render_redacted_range(
                original,
                &result.spans,
                0,
                span.start_byte,
            )?;
            result.authorized_spans.push(AuthorizedSpan {
                start_byte: prefix.len(),
                end_byte: prefix.len() + span.end_byte - span.start_byte,
                exception_id: id.clone(),
            });
            result.applied_exceptions.push(id);
        }
        Ok(result)
    }
    pub fn record_input(
        &self,
        project: &str,
        pulse: Option<&str>,
        call: &str,
        policy: &str,
        count: u64,
        categories: &[String],
        bytes: usize,
        duration: u64,
    ) -> Result<(), String> {
        self.connection()?.execute("INSERT INTO secret_sanitization_receipts(call_id,project_id,pulsation_id,policy_identity,input_redaction_count,categories_json,examined_bytes,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(call_id) DO NOTHING",params![call,project,pulse,policy,count.min(i64::MAX as u64) as i64,serde_json::to_string(categories).map_err(storage_error)?,bytes as i64,duration.min(i64::MAX as u64) as i64,Utc::now().to_rfc3339()]).map_err(storage_error)?;
        Ok(())
    }
    pub fn record_source_map(
        &self,
        project: &str,
        pulse: &str,
        call: &str,
        source: &str,
        original: &str,
        view: &str,
        policy: &str,
        spans: &[RedactionSpan],
    ) -> Result<(), String> {
        self.record_input(
            project,
            Some(pulse),
            call,
            policy,
            spans.len() as u64,
            &spans
                .iter()
                .map(|s| s.category.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            0,
            0,
        )?;
        self.connection()?.execute("INSERT INTO secret_sanitization_source_maps(call_id,source_ref,original_sha256,view_sha256,policy_identity,spans_json) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(call_id,source_ref) DO NOTHING",params![call,source,original,view,policy,serde_json::to_string(spans).map_err(storage_error)?]).map_err(storage_error)?;
        Ok(())
    }
    pub fn record_publication_guard_on(
        db: &Connection,
        project: &str,
        pulse: &str,
        artifact: &str,
        expected: Option<&str>,
        desired: &str,
        policy: &str,
    ) -> Result<(), String> {
        db.execute("INSERT INTO secret_publication_guards(artifact_id,project_id,pulsation_id,expected_sha256,desired_sha256,policy_identity,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(artifact_id) DO NOTHING",params![artifact,project,pulse,expected,desired,policy,Utc::now().to_rfc3339()]).map_err(storage_error)?;
        Ok(())
    }
    pub fn record_publication_guard(
        &self,
        project: &str,
        pulse: &str,
        artifact: &str,
        expected: Option<&str>,
        desired: &str,
        policy: &str,
    ) -> Result<(), String> {
        Self::record_publication_guard_on(
            &self.connection()?,
            project,
            pulse,
            artifact,
            expected,
            desired,
            policy,
        )
    }
    pub fn assert_publication_safe_on(
        db: &Connection,
        project: &str,
        artifact: &str,
        expected: Option<&str>,
        desired: &str,
    ) -> Result<(), String> {
        let row:Option<(Option<String>,String,String)>=db.query_row("SELECT expected_sha256,desired_sha256,policy_identity FROM secret_publication_guards WHERE artifact_id=?1 AND project_id=?2",params![artifact,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(storage_error)?;
        let Some((base, hash, policy)) = row else {
            return Err("publication_sanitization_evidence_missing".into());
        };
        if base.as_deref() != expected
            || hash != desired
            || policy != Self::policy_on(db, project)?.identity
        {
            return Err("publication_sanitization_evidence_stale".into());
        }
        Ok(())
    }
    pub fn assert_publication_safe(
        &self,
        project: &str,
        artifact: &str,
        expected: Option<&str>,
        desired: &str,
    ) -> Result<(), String> {
        Self::assert_publication_safe_on(&self.connection()?, project, artifact, expected, desired)
    }
    pub fn record_exception_uses(&self, call: &str, ids: &[String]) -> Result<(), String> {
        let count = ids.iter().collect::<BTreeSet<_>>().len();
        self.connection()?
            .execute(
                "UPDATE secret_sanitization_receipts SET exception_count=?2 WHERE call_id=?1",
                params![call, count as i64],
            )
            .map_err(storage_error)?;
        Ok(())
    }
    pub fn summary(
        &self,
        project: &str,
        pulse: Option<&str>,
    ) -> Result<SanitizationSummary, String> {
        Self::summary_on(&self.connection()?, project, pulse)
    }
    pub fn summary_on(
        db: &Connection,
        project: &str,
        pulse: Option<&str>,
    ) -> Result<SanitizationSummary, String> {
        let mut summary=db.query_row("SELECT COUNT(*),COALESCE(SUM(input_redaction_count),0),COALESCE(SUM(examined_bytes),0),COALESCE(SUM(duration_ms),0),COALESCE(SUM(exception_count),0) FROM secret_sanitization_receipts WHERE project_id=?1 AND (?2 IS NULL OR pulsation_id=?2)",params![project,pulse],|r|Ok(SanitizationSummary { entries_examined:r.get(0)?,input_redaction_count:r.get(1)?,output_redaction_count:0,examined_bytes:r.get(2)?,duration_ms:r.get(3)?,exceptions_applied:r.get(4)?,categories:Vec::new() })).map_err(storage_error)?;
        // Read only persisted evidence. A legacy re-scan, export or UI refresh
        // must never manufacture counters from placeholder occurrences.
        summary.output_redaction_count = db.query_row(
            "SELECT COALESCE((SELECT SUM(e.redaction_count) FROM terminal_events e JOIN terminal_runs r ON r.id=e.run_id
             WHERE r.project_id=?1 AND (?2 IS NULL OR r.pulsation_id=?2) AND r.execution_mode='cli_process'
             AND EXISTS(SELECT 1 FROM secret_sanitization_receipts s WHERE s.project_id=r.project_id AND (s.call_id=r.id OR s.call_id=r.llm_execution_id))),0)
             + COALESCE((SELECT SUM(x.output_redaction_count) FROM llm_route_executions x
             WHERE x.project_id=?1 AND (?2 IS NULL OR x.pulsation_id=?2) AND x.status='completed' AND x.visible_output_complete=1
             AND EXISTS(SELECT 1 FROM secret_sanitization_receipts s WHERE s.project_id=x.project_id AND s.call_id=x.id)),0)",
            params![project,pulse], |row| row.get(0),
        ).map_err(storage_error)?;
        summary.categories = Self::category_counts_on(db, project, pulse)?
            .into_keys()
            .collect();
        Ok(summary)
    }

    /// How many sanitized inputs and outputs redacted each category, from the
    /// same persisted evidence as [`Self::summary_on`].
    pub fn category_counts_on(
        db: &Connection,
        project: &str,
        pulse: Option<&str>,
    ) -> Result<BTreeMap<String, u64>, String> {
        let mut stmt=db.prepare(
            "SELECT categories_json FROM secret_sanitization_receipts WHERE project_id=?1 AND (?2 IS NULL OR pulsation_id=?2)
             UNION ALL SELECT e.redaction_categories_json FROM terminal_events e JOIN terminal_runs r ON r.id=e.run_id
             WHERE r.project_id=?1 AND (?2 IS NULL OR r.pulsation_id=?2) AND r.execution_mode='cli_process'
             AND EXISTS(SELECT 1 FROM secret_sanitization_receipts s WHERE s.project_id=r.project_id AND (s.call_id=r.id OR s.call_id=r.llm_execution_id))
             UNION ALL SELECT x.output_redaction_categories_json FROM llm_route_executions x
             WHERE x.project_id=?1 AND (?2 IS NULL OR x.pulsation_id=?2) AND x.status='completed' AND x.visible_output_complete=1
             AND EXISTS(SELECT 1 FROM secret_sanitization_receipts s WHERE s.project_id=x.project_id AND s.call_id=x.id)",
        ).map_err(storage_error)?;
        let rows = stmt
            .query_map(params![project, pulse], |r| r.get::<_, String>(0))
            .map_err(storage_error)?;
        let mut categories = BTreeMap::<String, u64>::new();
        for row in rows {
            let names = serde_json::from_str::<Vec<String>>(&row.map_err(storage_error)?)
                .map_err(storage_error)?;
            for name in names.into_iter().collect::<BTreeSet<_>>() {
                *categories.entry(name).or_default() += 1;
            }
        }
        Ok(categories)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, SanitizationStore, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("repo");
        std::fs::create_dir(&workspace).unwrap();
        let db_path = root.path().join("fixture.db");
        let db = Connection::open(&db_path).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE projects(id TEXT PRIMARY KEY,workspace_path TEXT); CREATE TABLE pulsations(id TEXT PRIMARY KEY,project_id TEXT REFERENCES projects(id) ON DELETE CASCADE); CREATE TABLE artifacts(id TEXT PRIMARY KEY); CREATE TABLE workspace_note_embeddings(id TEXT); CREATE TABLE embedding_sync_state(workspace_path TEXT); CREATE TABLE llm_route_executions(id TEXT PRIMARY KEY,project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,pulsation_id TEXT,status TEXT,visible_output_complete INTEGER); CREATE TABLE terminal_runs(id TEXT PRIMARY KEY,project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,pulsation_id TEXT,llm_execution_id TEXT,execution_mode TEXT); CREATE TABLE terminal_events(id TEXT PRIMARY KEY,run_id TEXT REFERENCES terminal_runs(id) ON DELETE CASCADE,sequence INTEGER,redaction_count INTEGER,redaction_categories_json TEXT);").unwrap();
        db.execute_batch(include_str!("../migrations/V49__secret_sanitization.sql"))
            .unwrap();
        db.execute(
            "INSERT INTO projects VALUES('p',?1)",
            [workspace.to_str().unwrap()],
        )
        .unwrap();
        db.execute("INSERT INTO pulsations VALUES('pulse','p')", [])
            .unwrap();
        (root, SanitizationStore::new(db_path), workspace)
    }
    #[test]
    fn output_summary_uses_persisted_evidence_once_per_event_and_final_execution() {
        let (_root, store, _workspace) = fixture();
        let db = store.connection().unwrap();
        db.execute("INSERT INTO pulsations VALUES('pulse-two','p')", [])
            .unwrap();
        for (call, pulse, count) in [
            ("api", "pulse", 1),
            ("cli", "pulse", 0),
            ("failed", "pulse", 0),
            ("other-pulse", "pulse-two", 0),
        ] {
            store
                .record_input(
                    "p",
                    Some(pulse),
                    call,
                    SANITIZATION_POLICY_VERSION,
                    count,
                    &[],
                    0,
                    0,
                )
                .unwrap();
        }
        db.execute_batch("INSERT INTO llm_route_executions(id,project_id,pulsation_id,status,visible_output_complete) VALUES('api','p','pulse','completed',1),('failed','p','pulse','failed',0),('legacy-api','p','pulse','completed',1),('other-pulse','p','pulse-two','completed',1);
            UPDATE llm_route_executions SET output_redaction_count=99 WHERE id IN ('legacy-api','failed');
            INSERT INTO terminal_runs VALUES('cli','p','pulse',NULL,'cli_process'),('legacy-cli','p','pulse',NULL,'cli_process'),('api-display','p','pulse','api','api_direct');
            INSERT INTO terminal_events VALUES('start','cli',1,1,'[\"known_api_token\"]'),('continuation','cli',2,0,'[]'),('stderr','cli',3,2,'[\"jwt_token\"]'),('legacy','legacy-cli',1,99,'[\"legacy\"]'),('duplicate-api','api-display',1,99,'[\"duplicate\"]');").unwrap();
        let repository = crate::llm::SqliteLlmRepository::new(&store.db_path);
        for _ in 0..2 {
            repository
                .record_output_redaction(
                    "api",
                    2,
                    &["sensitive_variable".into(), "private_key".into()],
                )
                .unwrap();
        }
        repository
            .record_output_redaction("other-pulse", 4, &["private_key".into()])
            .unwrap();
        assert!(
            repository
                .record_output_redaction("failed", 7, &[])
                .is_err()
        );
        let first = store.summary("p", Some("pulse")).unwrap();
        assert_eq!(first.entries_examined, 3);
        assert_eq!(first.input_redaction_count, 1);
        assert_eq!(first.output_redaction_count, 5);
        assert_eq!(
            first.categories,
            [
                "jwt_token",
                "known_api_token",
                "private_key",
                "sensitive_variable"
            ]
        );
        assert_eq!(store.summary("p", Some("pulse")).unwrap(), first);
        assert_eq!(
            store
                .summary("p", Some("pulse-two"))
                .unwrap()
                .output_redaction_count,
            4
        );
        assert_eq!(store.summary("p", None).unwrap().output_redaction_count, 9);
        assert_eq!(
            store
                .summary("unrelated-project", None)
                .unwrap()
                .output_redaction_count,
            0
        );
    }

    #[test]
    fn exception_requires_current_file_detector_occurrence_and_local_reason() {
        let (_root, store, workspace) = fixture();
        let text = "é token = ghp_syntheticabcdefghijklmnop\r\n";
        std::fs::write(workspace.join("fixture.rs"), text).unwrap();
        std::fs::write(workspace.join("copy.rs"), text).unwrap();
        std::fs::write(workspace.join(".env"), text).unwrap();
        assert!(store.preview_file("p", ".env").is_err());
        let preview = store.preview_file("p", "fixture.rs").unwrap();
        let span = &preview.spans[0];
        let mut request = CreateSecretRedactionException {
            project_id: "p".into(),
            relative_path: "fixture.rs".into(),
            detector_id: span.detector_id.clone(),
            detector_version: span.detector_version,
            original_file_sha256: preview.original_file_sha256,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            reason: "Synthetic fixture reviewed locally".into(),
        };
        let before = store.policy_for_project("p").unwrap();
        let approval = store.create_exception(&request).unwrap();
        let policy = store.policy_for_project("p").unwrap();
        assert_ne!(before.identity, policy.identity);
        let result = store
            .sanitize_source_with_policy(&policy, "fixture.rs", text)
            .unwrap();
        assert_eq!(result.text, text);
        assert!(result.spans.is_empty());
        assert_eq!(result.authorized_spans.len(), 1);
        assert_eq!(
            result.applied_exceptions.as_slice(),
            std::slice::from_ref(&approval.id)
        );
        assert!(
            !store
                .sanitize_source_with_policy(&policy, "copy.rs", text)
                .unwrap()
                .spans
                .is_empty()
        );
        assert!(
            !secret_redactor::redact_context(text)
                .unwrap()
                .spans
                .is_empty()
        );
        std::fs::write(workspace.join("fixture.rs"), format!("{text}// changed")).unwrap();
        assert!(!store.list_exceptions("p").unwrap()[0].active);
        assert!(store.create_exception(&request).is_err());
        request.reason = "token=ghp_syntheticabcdefghijklmnop".into();
        assert!(store.create_exception(&request).is_err());
        store.remove_exception("p", &approval.id).unwrap();
        assert_eq!(
            store.policy_for_project("p").unwrap().identity,
            before.identity
        );
    }
    #[test]
    fn evidence_is_durable_scoped_and_deduplicates_retries() {
        let (_root, store, _workspace) = fixture();
        let identity = store.policy_for_project("p").unwrap().identity;
        store
            .record_input(
                "p",
                Some("pulse"),
                "call",
                &identity,
                2,
                &["known_api_token".into()],
                100,
                1,
            )
            .unwrap();
        store
            .record_input("p", Some("pulse"), "call", &identity, 2, &[], 100, 1)
            .unwrap();
        let summary = SanitizationStore::new(&store.db_path)
            .summary("p", Some("pulse"))
            .unwrap();
        assert_eq!(summary.entries_examined, 1);
        assert_eq!(summary.input_redaction_count, 2);
        store
            .connection()
            .unwrap()
            .execute("INSERT INTO artifacts VALUES('artifact')", [])
            .unwrap();
        assert!(
            store
                .assert_publication_safe("p", "artifact", None, "desired")
                .is_err()
        );
        store
            .record_publication_guard(
                "p",
                "pulse",
                "artifact",
                Some("original"),
                "desired",
                &identity,
            )
            .unwrap();
        assert!(
            store
                .assert_publication_safe("p", "artifact", Some("original"), "desired")
                .is_ok()
        );
        assert!(
            store
                .assert_publication_safe("p", "artifact", Some("changed"), "desired")
                .is_err()
        );
        assert!(
            store
                .assert_publication_safe("p", "artifact", Some("original"), "changed")
                .is_err()
        );
        store
            .connection()
            .unwrap()
            .execute("DELETE FROM projects WHERE id='p'", [])
            .unwrap();
        assert_eq!(store.summary("p", None).unwrap().entries_examined, 0);
    }
    #[test]
    fn allowed_coordinates_follow_the_projected_bytes_without_a_value_allowlist() {
        let (_root, store, workspace) = fixture();
        let original = "ghp_firstsyntheticabcdefghijklmnop\nghp_secondsyntheticabcdefghijklmnop";
        std::fs::write(workspace.join("fixture.rs"), original).unwrap();
        let preview = store.preview_file("p", "fixture.rs").unwrap();
        let s = &preview.spans[1];
        store
            .create_exception(&CreateSecretRedactionException {
                project_id: "p".into(),
                relative_path: "fixture.rs".into(),
                detector_id: s.detector_id.clone(),
                detector_version: s.detector_version,
                original_file_sha256: preview.original_file_sha256,
                start_byte: s.start_byte,
                end_byte: s.end_byte,
                reason: "Second occurrence is synthetic".into(),
            })
            .unwrap();
        let result = store.sanitize_source("p", "fixture.rs", original).unwrap();
        let allowed = &result.authorized_spans[0];
        assert_eq!(
            &result.text[allowed.start_byte..allowed.end_byte],
            &original[s.start_byte..s.end_byte]
        );
        let proof = secret_redactor::PreparedModelInput::with_authorized_ranges(
            "",
            &result.text,
            "policy",
            &[],
            std::slice::from_ref(&(allowed.start_byte..allowed.end_byte)),
        )
        .unwrap();
        assert_eq!(proof.user_prompt(), result.text);
    }
}
