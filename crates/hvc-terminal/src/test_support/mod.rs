//! Native test doubles. Production policy and migration integration are tested by the host.

use std::{
    any::Any,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use hvc_contracts::{PulsationStage, TokenCounts, UsageContext};
use rusqlite::{Connection, params};

use crate::{
    host::{
        ContextRedaction, InputProof, PreparedModelInput, RedactionResult, RedactionSpan,
        SnapshotPreparation, TerminalHost, TerminalUsageRecorder,
    },
    terminals::{CliBackend, SafeCliWorkspace, TerminalError, TerminalRun},
    usage::CliUsageTracker,
};

pub(crate) struct TestDatabase {
    path: PathBuf,
}
pub(crate) struct TestProject {
    pub id: String,
}
pub(crate) struct TestPulse {
    pub id: String,
    pub stage: PulsationStage,
}

impl TestDatabase {
    pub(crate) fn initialize(directory: &Path) -> rusqlite::Result<Self> {
        let path = directory.join("native-terminal-test.db");
        Connection::open(&path)?.execute_batch(include_str!("schema.sql"))?;
        Ok(Self { path })
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn insert_project(
        &self,
        workspace: impl AsRef<Path>,
        name: Option<String>,
    ) -> rusqlite::Result<TestProject> {
        let id = uuid::Uuid::new_v4().to_string();
        Connection::open(&self.path)?.execute(
            "INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES(?1,?2,?3,?4,?4)",
            params![id, name.unwrap_or_else(|| "Native fixture".into()), workspace.as_ref().to_string_lossy(), "2026-10-23T00:00:00Z"],
        )?;
        Ok(TestProject { id })
    }

    pub(crate) fn insert_pulsation(
        &self,
        project: String,
        name: String,
    ) -> rusqlite::Result<TestPulse> {
        let id = uuid::Uuid::new_v4().to_string();
        let db = Connection::open(&self.path)?;
        db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at) VALUES(?1,?2,?3,'Intake','idle',?4,?4)", params![id,project,name,"2026-10-23T00:00:00Z"])?;
        db.execute("INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at) VALUES(?1,?2,'run_stage::Intake','pending',?3,?3)", params![uuid::Uuid::new_v4().to_string(),id,"2026-10-23T00:00:00Z"])?;
        Ok(TestPulse {
            id,
            stage: PulsationStage::Intake,
        })
    }
}

#[derive(Debug)]
struct FixtureProof {
    system: String,
    user: String,
    original_bytes: usize,
}
impl InputProof for FixtureProof {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn system_prompt(&self) -> &str {
        &self.system
    }
    fn user_prompt(&self) -> &str {
        &self.user
    }
    fn policy_identity(&self) -> &str {
        "native-fixture-policy"
    }
    fn redaction_count(&self) -> u64 {
        0
    }
    fn categories(&self) -> &[String] {
        &[]
    }
    fn original_bytes(&self) -> usize {
        self.original_bytes
    }
}

/// Only fixture strings are recognized. This deliberately does not implement host policy.
#[derive(Debug)]
pub(crate) struct FakeHost;

pub(crate) fn host() -> Arc<dyn TerminalHost> {
    Arc::new(FakeHost)
}

pub(crate) fn fixture_proof(system: &str, user: &str) -> Result<PreparedModelInput, TerminalError> {
    FakeHost.prepare_input(system, user, None)
}

fn fixture_projection(text: &str) -> Result<ContextRedaction, String> {
    let regex = regex::Regex::new(r"(?s)-----BEGIN PRIVATE KEY-----.*?-----END PRIVATE KEY-----|(?:ghp_|sk-proj-)[A-Za-z0-9_]+|synthetic[-_][A-Za-z0-9_-]+|fixture-secret-token|arbitrary phrase|nested phrase")
        .map_err(|_| "fixture_pattern".to_string())?;
    let mut spans = Vec::new();
    let mut projected = String::new();
    let mut cursor = 0;
    for found in regex.find_iter(text) {
        let marker = format!("<REDACTED_SECRET:FIXTURE_{}>", spans.len() + 1);
        projected.push_str(&text[cursor..found.start()]);
        projected.push_str(&marker);
        projected.extend(found.as_str().chars().filter(|c| matches!(c, '\r' | '\n')));
        spans.push(RedactionSpan {
            start_byte: found.start(),
            end_byte: found.end(),
            detector_id: "fixture".into(),
            detector_version: 1,
            category: "fixture".into(),
            marker,
        });
        cursor = found.end();
    }
    projected.push_str(&text[cursor..]);
    Ok(ContextRedaction {
        text: projected,
        spans,
        applied_exceptions: Vec::new(),
        authorized_spans: Vec::new(),
    })
}

fn fixture_json(value: &mut serde_json::Value) -> Result<u64, String> {
    let mut count = 0;
    match value {
        serde_json::Value::String(text) => {
            if let Ok(mut nested) = serde_json::from_str::<serde_json::Value>(text)
                && matches!(
                    nested,
                    serde_json::Value::Object(_) | serde_json::Value::Array(_)
                )
            {
                count += fixture_json(&mut nested)?;
                *text = nested.to_string();
            } else {
                let projection = fixture_projection(text)?;
                count += projection.spans.len() as u64;
                *text = projection.text;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                count += fixture_json(item)?;
            }
        }
        serde_json::Value::Object(items) => {
            let original = std::mem::take(items);
            for (key, mut value) in original {
                let projection = fixture_projection(&key)?;
                count += projection.spans.len() as u64 + fixture_json(&mut value)?;
                items.insert(projection.text, value);
            }
        }
        _ => {}
    }
    Ok(count)
}

impl TerminalHost for FakeHost {
    fn redact_context(&self, text: &str) -> Result<ContextRedaction, String> {
        fixture_projection(text)
    }
    fn redact_secrets(&self, text: &str) -> Result<RedactionResult, String> {
        Ok(fixture_projection(text)?.result())
    }
    fn sanitize_model_text_counted(&self, text: &str) -> Result<RedactionResult, String> {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) {
            let count = fixture_json(&mut value)?;
            Ok(RedactionResult {
                text: value.to_string(),
                count,
                categories: if count == 0 {
                    vec![]
                } else {
                    vec!["fixture".into()]
                },
            })
        } else {
            self.redact_secrets(text)
        }
    }
    fn prepare_input(
        &self,
        system: &str,
        user: &str,
        candidate: Option<PreparedModelInput>,
    ) -> Result<PreparedModelInput, TerminalError> {
        if let Some(proof) = candidate {
            if proof.as_any().downcast_ref::<FixtureProof>().is_none()
                || proof.system_prompt() != system
                || proof.user_prompt() != user
            {
                return Err(TerminalError::Redaction("fixture_proof_mismatch".into()));
            }
            return Ok(proof);
        }
        Ok(Arc::new(FixtureProof {
            system: fixture_projection(system)
                .map_err(TerminalError::Redaction)?
                .text,
            user: fixture_projection(user)
                .map_err(TerminalError::Redaction)?
                .text,
            original_bytes: system.len() + user.len(),
        }))
    }
    fn record_prepared_input(
        &self,
        _: &Path,
        _: &str,
        _: Option<&str>,
        _: &str,
        _: &PreparedModelInput,
    ) -> Result<(), TerminalError> {
        Ok(())
    }
    fn prepare_workspace(
        &self,
        _: &Path,
        _: &Path,
        _: &TerminalRun,
    ) -> Result<SafeCliWorkspace, TerminalError> {
        Err(TerminalError::Security(
            "native fixture has no repository capability".into(),
        ))
    }
    fn record_snapshot(
        &self,
        _: &Path,
        _: &TerminalRun,
        _: &SnapshotPreparation,
        _: u64,
    ) -> Result<(), TerminalError> {
        Err(TerminalError::Security(
            "native fixture has no snapshot receipt capability".into(),
        ))
    }
    fn create_usage_recorder(
        &self,
        path: &Path,
        context: UsageContext,
        backend: CliBackend,
    ) -> rusqlite::Result<Arc<dyn TerminalUsageRecorder>> {
        Ok(Arc::new(FixtureUsage {
            path: path.to_path_buf(),
            context,
            backend,
            tracker: Mutex::new(CliUsageTracker::default()),
        }))
    }
}

struct FixtureUsage {
    path: PathBuf,
    context: UsageContext,
    backend: CliBackend,
    tracker: Mutex<CliUsageTracker>,
}
impl FixtureUsage {
    fn persist(&self, partial: bool) -> rusqlite::Result<()> {
        let tracker = self.tracker.lock().unwrap();
        let counts = tracker
            .live_partial()
            .map_or_else(|| tracker.totals(), |m| m.counts);
        Connection::open(&self.path)?.execute("INSERT INTO token_usage_records(source_kind,source_id,unit_key,project_id,transport,provider_id,input_tokens,output_tokens,total_tokens,quality,started_at,updated_at) VALUES(?1,?2,'fixture-unit',?3,'cli',?4,?5,?6,?7,?8,?9,?9) ON CONFLICT(source_kind,source_id,unit_key) DO UPDATE SET input_tokens=excluded.input_tokens,output_tokens=excluded.output_tokens,total_tokens=excluded.total_tokens,quality=excluded.quality", params![self.context.source_kind,self.context.source_id,self.context.project_id,self.context.provider_id,counts.input_tokens,counts.output_tokens,counts.total_tokens,if partial {"partial"}else{"complete"},self.context.started_at])?;
        Ok(())
    }
}
impl TerminalUsageRecorder for FixtureUsage {
    fn observe_line(&self, line: &[u8]) -> rusqlite::Result<()> {
        if let Ok(value) = serde_json::from_slice(line) {
            self.tracker.lock().unwrap().observe(self.backend, &value);
        }
        self.persist(false)
    }
    fn totals(&self) -> TokenCounts {
        self.tracker.lock().unwrap().totals()
    }
    fn output_tokens_so_far(&self) -> u64 {
        self.tracker.lock().unwrap().output_tokens_so_far()
    }
    fn record_live_partial(&self) -> rusqlite::Result<()> {
        self.persist(true)
    }
    fn mark_partial(&self) -> rusqlite::Result<()> {
        self.tracker.lock().unwrap().mark_incomplete();
        self.persist(true)
    }
}
