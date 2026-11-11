//! The on-demand proposal (ADR-034 M5, decision D6): one model call per
//! request, with the sanitized note and the sanitized line diffs of the
//! note's open signals, and one corrective call when the answer is refused.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{TransactionBehavior, params};
use tokio_util::sync::CancellationToken;

use super::contract::{
    self, AnchorV1, ChangeKind, CodeChangeV1, NoteAnchor, ReconciliationInputV1,
};
use super::store::{self, LinkedFile};
use super::{DocDriftOverview, ExpectedCode, project_root};
use crate::feature_forge::RequestError;
use crate::feature_forge::service::{id, invalid, json, now};
use crate::llm::AgentRole;
use crate::orchestration::composition::anchors::partition_file;
use crate::orchestration::composition::line_diff::{self, LineDiff};
use crate::orchestration::composition::review_boundary::{
    MAX_DIFF_CHARS_PER_FILE, MAX_DIFF_CHARS_TOTAL,
};
use crate::persistence_service::{
    PersistenceService, PersistenceServiceError as Error, StructuredRequest,
};
use crate::{AppEvent, AppEventBus};

const ROUTING_STAGE: &str = "Reconciliation";
const MAX_CALLS: u32 = 2;

/// A documentation reconciliation changed; the UI reloads the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocDriftEvent {
    pub project_id: String,
}

type Key = (String, String);

pub struct DocReconcilerRuntime {
    persistence: PersistenceService,
    events: Arc<AppEventBus>,
    runtime: tokio::runtime::Handle,
    active: Mutex<HashMap<Key, CancellationToken>>,
    failures: Mutex<HashMap<Key, String>>,
    closing: AtomicBool,
    pub(crate) gateway_override: Option<Arc<dyn crate::llm::LlmGateway>>,
}

/// Everything one request sends, and what the proposal is checked against.
struct Prepared {
    original: String,
    note_sha256: String,
    anchors: Vec<NoteAnchor>,
    changes: Vec<CodeChangeV1>,
    expected: Vec<ExpectedCode>,
    prompt: String,
}

impl DocReconcilerRuntime {
    pub fn new(
        persistence: PersistenceService,
        events: Arc<AppEventBus>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            persistence,
            events,
            runtime,
            active: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
            gateway_override: None,
        }
    }

    /// The persisted overview, plus the requests in flight and the last
    /// failure of each note.
    pub fn overview(&self, project: &str) -> Result<DocDriftOverview, Error> {
        let mut overview = self.persistence.doc_drift_overview(project)?;
        overview.generating = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|(p, _)| p == project)
            .map(|(_, note)| note.clone())
            .collect();
        overview.generating.sort();
        overview.failures = self
            .failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((p, _), _)| p == project)
            .map(|((_, note), message)| (note.clone(), message.clone()))
            .collect();
        overview.failures.sort();
        Ok(overview)
    }

    /// Starts a proposal for one note with open signals. Only the
    /// developer's request reaches the model (decision D6).
    pub fn propose(self: &Arc<Self>, project: &str, note: &str) -> Result<(), Error> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if self.closing.load(Ordering::SeqCst) {
            return Err(invalid("Documentation reconciliation is shutting down"));
        }
        let key = (project.to_owned(), note.to_owned());
        if active.contains_key(&key) {
            return Err(invalid(
                "A proposal for this note is already being prepared",
            ));
        }
        {
            let db = self.persistence.connection()?;
            let open: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM doc_drift_signals WHERE project_id=?1 AND note_path=?2 AND state='open')",
                params![project, note],
                |r| r.get(0),
            )?;
            if !open {
                return Err(invalid("This note has no open change to reconcile"));
            }
            let proposed: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM doc_reconciliation_proposals WHERE project_id=?1 AND note_path=?2 AND state IN ('pending','edited'))",
                params![project, note],
                |r| r.get(0),
            )?;
            if proposed {
                return Err(invalid("This note already has a proposal to review"));
            }
        }
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        let token = CancellationToken::new();
        active.insert(key.clone(), token.clone());
        let this = self.clone();
        self.runtime.spawn(async move {
            if let Err(message) = this.run(&key.0, &key.1, &token).await {
                this.failures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key.clone(), message);
            }
            this.active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
            // ADR-037 M2: the run committed; its item precedes the broadcast.
            if let Ok(db) = this.persistence.connection() {
                use crate::exchange::log::{LogRecordKind, PROJECT_SUBJECT, StreamKind, StreamRef};
                crate::exchange::log::record_after_commit(
                    &db,
                    StreamRef::new(&key.0, StreamKind::Docs, PROJECT_SUBJECT),
                    LogRecordKind::DocProposalChanged,
                    ikore_contracts::exchange::HostComponent::DocsState,
                    None,
                    format!("doc_proposal:run:{}", uuid::Uuid::new_v4()),
                );
            }
            this.events.publish(AppEvent::DocDrift(DocDriftEvent {
                project_id: key.0.clone(),
            }));
        });
        Ok(())
    }

    pub fn cancel_all(&self) {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        self.closing.store(true, Ordering::SeqCst);
        for token in active.values() {
            token.cancel();
        }
    }

    pub async fn wait_for_idle(&self) {
        while !self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn prepare(&self, project: &str, note: &str) -> Result<Prepared, String> {
        let db = self.persistence.connection().map_err(|e| e.to_string())?;
        let root = project_root(&db, project).map_err(|e| e.to_string())?;
        let repository =
            crate::repository_context::RepositoryContext::new(&root).map_err(|e| e.to_string())?;
        let original = crate::workspace_service::read_context_document(&root, note)
            .map_err(|_| "The note cannot be read".to_string())?;
        let sanitization =
            crate::sanitization::SanitizationStore::new(self.persistence.database_path());
        let policy = sanitization
            .policy_for_project(project)
            .map_err(|_| "The sanitization policy is unavailable".to_string())?;
        let redaction = sanitization
            .sanitize_source_with_policy(&policy, note, &original)
            .map_err(|_| "The note could not be sanitized".to_string())?;
        let rel =
            crate::orchestration::composition::RelPath::new(note).map_err(|e| e.to_string())?;
        let partition = partition_file(&rel, &original, contract::MAX_ANCHORS);
        let anchors = partition
            .intervals
            .iter()
            .enumerate()
            .map(|(index, &(start, end))| {
                let mut view = String::new();
                let mut cursor = start;
                let mut protected = false;
                for span in &redaction.spans {
                    if span.end_byte <= start || span.start_byte >= end {
                        continue;
                    }
                    protected = true;
                    let from = span.start_byte.max(start);
                    if from > cursor {
                        view.push_str(&original[cursor..from]);
                    }
                    view.push_str(&span.marker);
                    cursor = cursor.max(span.end_byte.min(end));
                }
                if cursor < end {
                    view.push_str(&original[cursor..end]);
                }
                NoteAnchor {
                    handle: format!("anchor_{}", index + 1),
                    start,
                    end,
                    view,
                    protected,
                }
            })
            .collect::<Vec<_>>();

        let signals = db
            .prepare("SELECT code_path,base_sha256 FROM doc_drift_signals WHERE project_id=?1 AND note_path=?2 AND state='open' ORDER BY code_path")
            .and_then(|mut statement| {
                statement
                    .query_map(params![project, note], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(|e| e.to_string())?;
        if signals.is_empty() {
            return Err("This note has no open change to reconcile".into());
        }
        let sanitize = |path: &str, text: &str| {
            sanitization
                .sanitize_source_with_policy(&policy, path, text)
                .map(|view| view.text)
                .map_err(|_| format!("{path} could not be sanitized"))
        };
        let mut budget = MAX_DIFF_CHARS_TOTAL;
        let mut changes = Vec::new();
        let mut expected = Vec::new();
        for (path, base) in signals {
            let file = store::read_linked(&root, &repository, &path);
            expected.push(ExpectedCode {
                path: path.clone(),
                sha256: file.sha256().map(str::to_owned),
            });
            let mut change = CodeChangeV1 {
                path: path.clone(),
                change: ChangeKind::Modified,
                diff: None,
                current: None,
                gap: None,
            };
            match &file {
                LinkedFile::Absent | LinkedFile::Excluded => change.change = ChangeKind::Removed,
                LinkedFile::Present { text: None, .. } => {
                    change.gap = Some("content_unavailable".into());
                }
                LinkedFile::Present {
                    text: Some(current),
                    ..
                } => {
                    let base_text = match base.as_deref() {
                        Some(sha) => store::blob(&db, project, sha).map_err(|e| e.to_string())?,
                        None => Some(String::new()),
                    };
                    let current_view = sanitize(&path, current)?;
                    let limit = MAX_DIFF_CHARS_PER_FILE.min(budget);
                    match base_text {
                        Some(base_text) => {
                            let base_view = sanitize(&path, &base_text)?;
                            match line_diff::unified(&base_view, &current_view, limit) {
                                LineDiff::Identical => continue,
                                LineDiff::Unified { text, truncated } => {
                                    budget = budget.saturating_sub(text.chars().count());
                                    if truncated {
                                        change.gap = Some("diff_truncated".into());
                                    }
                                    change.diff = Some(text);
                                }
                                LineDiff::TooLarge => change.gap = Some("too_large".into()),
                            }
                        }
                        None => {
                            change.gap = Some("diff_unavailable".into());
                            if current_view.chars().count() <= limit {
                                budget = budget.saturating_sub(current_view.chars().count());
                                change.current = Some(current_view);
                            } else {
                                change.gap = Some("too_large".into());
                            }
                        }
                    }
                }
            }
            changes.push(change);
        }
        let input = ReconciliationInputV1 {
            note_path: note,
            anchors: anchors
                .iter()
                .map(|anchor| AnchorV1 {
                    anchor_ref: &anchor.handle,
                    protected: anchor.protected,
                    text: &anchor.view,
                })
                .collect(),
            changes: &changes,
        };
        let prompt = json(&input).map_err(|e| e.to_string())?;
        Ok(Prepared {
            note_sha256: store::sha256_hex(original.as_bytes()),
            original,
            anchors,
            changes,
            expected,
            prompt,
        })
    }

    async fn run(
        &self,
        project: &str,
        note: &str,
        token: &CancellationToken,
    ) -> Result<(), String> {
        let prepared = self.prepare(project, note)?;
        let mut rejection: Option<String> = None;
        let mut calls = 0;
        let validated = loop {
            calls += 1;
            let answer = self
                .persistence
                .execute_structured_request(
                    StructuredRequest {
                        project,
                        job_chain: format!("reconcile:{note}"),
                        role: AgentRole::DocumentationReconciler,
                        stage: ROUTING_STAGE,
                        system_prompt: contract::system_prompt(rejection.as_deref()),
                        prompt: prepared.prompt.clone(),
                        schema_name: contract::SCHEMA_NAME.into(),
                        schema: contract::reply_schema(),
                        role_output_floor: true,
                        context_too_large: "The note and its changes exceed the context budget; reconcile fewer files at a time or increase the configured limit",
                    },
                    token,
                    self.gateway_override.as_deref(),
                )
                .await;
            let reason = match answer {
                Ok(raw) => {
                    match contract::validate_reply(&raw, &prepared.original, &prepared.anchors) {
                        Ok(validated) => break validated,
                        Err(reason) => reason,
                    }
                }
                Err(RequestError::Unreadable(detail)) => detail,
                Err(error) => {
                    return Err(crate::feature_forge::TurnError::from(error)
                        .into_failure()
                        .technical);
                }
            };
            if calls >= MAX_CALLS || token.is_cancelled() {
                return Err(format!("The proposal was refused twice: {reason}"));
            }
            rejection = Some(reason);
        };
        if token.is_cancelled() {
            return Err("The proposal was cancelled".into());
        }
        let mut db = self.persistence.connection().map_err(|e| e.to_string())?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let root = project_root(&tx, project).map_err(|e| e.to_string())?;
        if store::note_sha256(&root, note).as_deref() != Some(prepared.note_sha256.as_str()) {
            return Err("The note changed while the proposal was prepared; ask again".into());
        }
        let at = now();
        let preview = super::preview_diff(&prepared.original, &validated.proposed);
        let request_sha256 = store::sha256_hex(prepared.prompt.as_bytes());
        tx.execute(
            "INSERT INTO doc_reconciliation_proposals(id,project_id,note_path,state,expected_note_sha256,expected_code_json,request_sha256,summary_json,edits_json,unresolved_json,code_diffs_json,proposed_content,preview_diff,created_at,updated_at) VALUES(?1,?2,?3,'pending',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",
            params![
                id(),
                project,
                note,
                prepared.note_sha256,
                to_json(&prepared.expected)?,
                request_sha256,
                to_json(&validated.reply.change_summary)?,
                to_json(&validated.reply.edits)?,
                to_json(&validated.reply.unresolved)?,
                to_json(&prepared.changes)?,
                validated.proposed,
                preview,
                at
            ],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE doc_drift_signals SET state='proposed',updated_at=?3 WHERE project_id=?1 AND note_path=?2 AND state='open'",
            params![project, note, at],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}
