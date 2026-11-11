//! Documentation reconciliation, code -> note (ADR-034 M5).
//!
//! The host keeps one baseline per explicit link note -> code file and raises
//! a signal when a linked file's hash leaves it. Detection is local and only
//! hashes. A proposal to update the note is generated only when the developer
//! asks for one (decision D6), by an executor that has no tools and may only
//! edit that note. Publishing checks the note and code hashes it was made
//! against; any divergence invalidates it. The interface never attributes a
//! change to anyone nor states that it is a defect.

pub(crate) mod contract;
pub mod links;
mod runtime;
mod store;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub use contract::{AnchorEditV1, ChangeKind, CodeChangeV1};
pub use links::LinkKind;
pub use runtime::{DocDriftEvent, DocReconcilerRuntime};

use crate::feature_forge::service::{invalid, now};
use crate::persistence_service::{PersistenceService, PersistenceServiceError as Error};
use crate::repository_context::RepositoryContext;

/// Base bytes kept per file, and per project, for the proposal's line diff.
pub const MAX_BLOB_BYTES: usize = 256 * 1024;
pub const MAX_PROJECT_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// Links compared by the scan at project open; the rest is `partial_scan`.
pub const MAX_SCAN_LINKS: usize = 2_000;
pub const MAX_REASON_CHARS: usize = 400;

/// The fixed indicator text: a count, never an author or a defect.
pub fn indicator_text(notes: usize) -> String {
    format!("{notes} document(s) may need an update after code changes")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftOrigin {
    Watcher,
    StartupScan,
    Pulsation,
}

impl DriftOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watcher => "watcher",
            Self::StartupScan => "startup_scan",
            Self::Pulsation => "pulsation",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "startup_scan" => Self::StartupScan,
            "pulsation" => Self::Pulsation,
            _ => Self::Watcher,
        }
    }

    /// How the interface names the trigger; it says when the change was
    /// seen, not who made it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Watcher => "seen while the app was open",
            Self::StartupScan => "found when the project was opened",
            Self::Pulsation => "published by a pulsation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    Open,
    Proposed,
    Resolved,
    Ignored,
}

impl SignalState {
    fn parse(value: &str) -> Self {
        match value {
            "proposed" => Self::Proposed,
            "resolved" => Self::Resolved,
            "ignored" => Self::Ignored,
            _ => Self::Open,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftSignal {
    pub id: String,
    pub note_path: String,
    pub code_path: String,
    pub base_sha256: Option<String>,
    /// `None` when the file was removed.
    pub current_sha256: Option<String>,
    pub origin: DriftOrigin,
    pub pulsation_id: Option<String>,
    pub state: SignalState,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Pending,
    Edited,
    Published,
    Rejected,
    Invalidated,
}

impl ProposalState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Edited => "edited",
            Self::Published => "published",
            Self::Rejected => "rejected",
            Self::Invalidated => "invalidated",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "edited" => Self::Edited,
            "published" => Self::Published,
            "rejected" => Self::Rejected,
            "invalidated" => Self::Invalidated,
            _ => Self::Pending,
        }
    }

    pub const fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Edited)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedCode {
    pub path: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconciliationProposal {
    pub id: String,
    pub note_path: String,
    pub state: ProposalState,
    pub expected_note_sha256: String,
    pub expected_code: Vec<ExpectedCode>,
    pub change_summary: Vec<String>,
    pub edits: Vec<AnchorEditV1>,
    pub unresolved: Vec<String>,
    pub code_changes: Vec<CodeChangeV1>,
    pub proposed_content: String,
    /// Unified diff of the note, current -> proposed.
    pub preview_diff: String,
    pub reason: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftedNote {
    pub note_path: String,
    pub signals: Vec<DriftSignal>,
    pub proposal: Option<ReconciliationProposal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilencedLink {
    pub note_path: String,
    pub code_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub partial: bool,
    pub scanned: usize,
    pub total: usize,
    pub scanned_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocDriftOverview {
    /// Notes with an active signal or an open proposal, by path.
    pub notes: Vec<DriftedNote>,
    pub tracked_links: usize,
    pub unlinked_notes: usize,
    pub silenced: Vec<SilencedLink>,
    pub scan: Option<ScanSummary>,
    /// Notes whose proposal is being generated right now.
    pub generating: Vec<String>,
    /// The last proposal request that failed, by note.
    pub failures: Vec<(String, String)>,
}

impl DocDriftOverview {
    /// How many notes the indicator counts.
    pub fn drifted(&self) -> usize {
        self.notes
            .iter()
            .filter(|note| !note.signals.is_empty())
            .count()
    }
}

/// The developer's decisions on a proposal or a note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconciliationAction {
    Approve {
        proposal_id: String,
    },
    Edit {
        proposal_id: String,
        content: String,
    },
    Reject {
        proposal_id: String,
    },
    /// Advances the note's baselines without changing it.
    Ignore {
        note_path: String,
        reason: Option<String>,
    },
    Silence {
        note_path: String,
        code_path: String,
    },
    Unsilence {
        note_path: String,
        code_path: String,
    },
}

/// The outcome of an approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Published,
    /// The note or a code file changed since the proposal.
    Invalidated(String),
}

pub(crate) fn project_root(db: &Connection, project: &str) -> Result<PathBuf, Error> {
    let root: String = db
        .query_row(
            "SELECT workspace_path FROM projects WHERE id=?1 AND is_archived=0",
            [project],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| invalid("Project not found"))?;
    let root = PathBuf::from(root);
    Ok(root.canonicalize().unwrap_or(root))
}

fn repository(root: &Path) -> Result<RepositoryContext, Error> {
    RepositoryContext::new(root).map_err(|error| invalid(error.to_string()))
}

fn reason(value: Option<String>) -> Result<Option<String>, Error> {
    let value = value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if value
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_REASON_CHARS)
    {
        return Err(invalid(format!(
            "The reason is limited to {MAX_REASON_CHARS} characters"
        )));
    }
    Ok(value)
}

impl PersistenceService {
    /// Derives the links from the Vault index and brings the baselines in
    /// line with them. Every file is read before the short write
    /// transaction. Returns the notes without any usable link.
    fn sync_doc_links(
        &self,
        db: &mut Connection,
        project: &str,
        root: &Path,
    ) -> Result<usize, Error> {
        let index = self
            .document_cache
            .with(root, |cache| {
                crate::workspace_service::get_workspace_vault_index(
                    root,
                    cache,
                    crate::workspace_service::GetWorkspaceVaultIndexRequest {
                        force_rebuild: false,
                    },
                )
            })
            .map_err(|error| invalid(format!("The Vault index is unavailable: {error:?}")))?;
        let repository = repository(root)?;
        let existing = store::existing_pairs(db, project)?;
        let known = existing
            .iter()
            .map(|(_, code)| code.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let derived = links::derive_links(
            index.notes.iter().map(|note| {
                (
                    note.relative_path.as_str(),
                    note.source_files.as_slice(),
                    note.test_files.as_slice(),
                )
            }),
            |path| {
                if known.contains(path) {
                    links::LinkCheck::Present
                } else {
                    store::check_path(root, &repository, path)
                }
            },
        );
        let prefetched = store::prefetch_new(root, &repository, &derived.links, &existing);
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::sync_baselines(&tx, project, &derived.links, &prefetched)?;
        tx.commit()?;
        Ok(derived.unlinked_notes)
    }

    /// A workspace change to a linked file: a hash comparison only. Most
    /// events touch no linked file and are answered with one read, without
    /// the write lock the pipeline also needs. A note's own changes (its
    /// links, a rename or a deletion) are applied by the next overview or
    /// scan, which already re-derive every link from the Vault index; doing it
    /// here would rebuild that index on each brownfield note publication.
    pub(crate) fn record_doc_drift_change(
        &self,
        read: &Connection,
        project: &str,
        root: &Path,
        relative_path: &str,
    ) -> Result<(), Error> {
        let path = relative_path.replace('\\', "/");
        if !store::is_tracked(read, project, &path)? {
            return Ok(());
        }
        let file = store::read_linked(root, &repository(root)?, &path);
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::observe_file(&tx, project, &path, &file, DriftOrigin::Watcher, None)?;
        tx.commit()?;
        Ok(())
    }

    /// The scan at project open, once per call: links, finished pulsations,
    /// then every tracked path up to [`MAX_SCAN_LINKS`], read before the
    /// write transaction.
    pub fn scan_doc_drift(&self, project: &str) -> Result<(), Error> {
        let mut db = self.connection()?;
        let root = project_root(&db, project)?;
        self.sync_doc_links(&mut db, project, &root)?;
        let repository = repository(&root)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::process_finished_pulsations(&tx, project, &root, &repository)?;
        tx.commit()?;
        let paths = store::tracked_paths(&db, project)?;
        let files = paths
            .iter()
            .take(MAX_SCAN_LINKS)
            .map(|path| (path.clone(), store::read_linked(&root, &repository, path)))
            .collect::<Vec<_>>();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::record_scan(&tx, project, &files, paths.len())?;
        tx.commit()?;
        Ok(())
    }

    /// What the indicator, the Vault list and the comparison show. Brings the
    /// links and the finished pulsations up to date first.
    pub fn doc_drift_overview(&self, project: &str) -> Result<DocDriftOverview, Error> {
        let mut db = self.connection()?;
        let root = project_root(&db, project)?;
        let unlinked_notes = self.sync_doc_links(&mut db, project, &root)?;
        let repository = repository(&root)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        store::process_finished_pulsations(&tx, project, &root, &repository)?;
        tx.commit()?;
        let signals = db
            .prepare("SELECT id,note_path,code_path,base_sha256,current_sha256,origin,pulsation_id,state,updated_at FROM doc_drift_signals WHERE project_id=?1 AND state IN ('open','proposed') ORDER BY note_path,code_path")?
            .query_map([project], |r| {
                Ok(DriftSignal {
                    id: r.get(0)?,
                    note_path: r.get(1)?,
                    code_path: r.get(2)?,
                    base_sha256: r.get(3)?,
                    current_sha256: r.get(4)?,
                    origin: DriftOrigin::parse(&r.get::<_, String>(5)?),
                    pulsation_id: r.get(6)?,
                    state: SignalState::parse(&r.get::<_, String>(7)?),
                    updated_at: r.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut notes: Vec<DriftedNote> = Vec::new();
        for signal in signals {
            match notes.last_mut() {
                Some(note) if note.note_path == signal.note_path => note.signals.push(signal),
                _ => notes.push(DriftedNote {
                    note_path: signal.note_path.clone(),
                    signals: vec![signal],
                    proposal: None,
                }),
            }
        }
        for proposal in open_proposals(&db, project)? {
            match notes
                .iter_mut()
                .find(|note| note.note_path == proposal.note_path)
            {
                Some(note) => note.proposal = Some(proposal),
                None => notes.push(DriftedNote {
                    note_path: proposal.note_path.clone(),
                    signals: Vec::new(),
                    proposal: Some(proposal),
                }),
            }
        }
        notes.sort_by(|a, b| a.note_path.cmp(&b.note_path));
        let tracked_links: usize = db.query_row(
            "SELECT COUNT(*) FROM doc_reconciliation_baselines WHERE project_id=?1 AND state='tracking'",
            [project],
            |r| r.get::<_, i64>(0),
        )? as usize;
        let silenced = db
            .prepare("SELECT note_path,code_path FROM doc_reconciliation_baselines WHERE project_id=?1 AND state='silenced' ORDER BY note_path,code_path")?
            .query_map([project], |r| {
                Ok(SilencedLink {
                    note_path: r.get(0)?,
                    code_path: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let scan = db
            .query_row(
                "SELECT state,scanned,total,scanned_at FROM doc_reconciliation_scans WHERE project_id=?1",
                [project],
                |r| {
                    Ok(ScanSummary {
                        partial: r.get::<_, String>(0)? == "partial_scan",
                        scanned: r.get::<_, i64>(1)? as usize,
                        total: r.get::<_, i64>(2)? as usize,
                        scanned_at: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(DocDriftOverview {
            notes,
            tracked_links,
            unlinked_notes,
            silenced,
            scan,
            generating: Vec::new(),
            failures: Vec::new(),
        })
    }

    /// A note renamed through the editor keeps its reconciliation rows.
    pub(crate) fn rename_doc_drift_note(
        &self,
        root: &Path,
        from: &str,
        to: &str,
    ) -> Result<(), Error> {
        let db = self.connection()?;
        let root = root.canonicalize()?;
        let projects = db
            .prepare("SELECT id,workspace_path FROM projects WHERE is_archived=0")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        for (project, path) in projects {
            if Path::new(&path).canonicalize().ok().as_deref() == Some(root.as_path()) {
                store::rename_note(&db, &project, from, to)?;
            }
        }
        Ok(())
    }

    /// Applies one of the developer's decisions.
    pub fn reconcile_doc_drift(
        &self,
        project: &str,
        action: ReconciliationAction,
    ) -> Result<Option<ApprovalOutcome>, Error> {
        let mut db = self.connection()?;
        let root = project_root(&db, project)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let repository = repository(&root)?;
        let at = now();
        let outcome = match action {
            ReconciliationAction::Approve { proposal_id } => {
                let proposal = open_proposal(&tx, project, &proposal_id)?;
                let outcome = approve(&tx, project, &root, &repository, &proposal)?;
                Some(outcome)
            }
            ReconciliationAction::Edit {
                proposal_id,
                content,
            } => {
                let proposal = open_proposal(&tx, project, &proposal_id)?;
                let current =
                    crate::workspace_service::read_context_document(&root, &proposal.note_path)
                        .map_err(|_| invalid("The note cannot be read"))?;
                contract::validate_developer_content(&current, &content).map_err(invalid)?;
                let preview = preview_diff(&current, &content);
                tx.execute(
                    "UPDATE doc_reconciliation_proposals SET state='edited',proposed_content=?3,preview_diff=?4,updated_at=?5 WHERE project_id=?1 AND id=?2",
                    params![project, proposal_id, content, preview, at],
                )?;
                None
            }
            ReconciliationAction::Reject { proposal_id } => {
                let proposal = open_proposal(&tx, project, &proposal_id)?;
                tx.execute(
                    "UPDATE doc_reconciliation_proposals SET state='rejected',updated_at=?3 WHERE project_id=?1 AND id=?2",
                    params![project, proposal.id, at],
                )?;
                tx.execute(
                    "UPDATE doc_drift_signals SET state='open',updated_at=?3 WHERE project_id=?1 AND note_path=?2 AND state='proposed'",
                    params![project, proposal.note_path, at],
                )?;
                None
            }
            ReconciliationAction::Ignore {
                note_path,
                reason: why,
            } => {
                let why = reason(why)?;
                let codes = tx
                    .prepare("SELECT code_path FROM doc_drift_signals WHERE project_id=?1 AND note_path=?2 AND state IN ('open','proposed')")?
                    .query_map(params![project, note_path], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                if codes.is_empty() {
                    return Err(invalid("This note has no change to ignore"));
                }
                for code in &codes {
                    let file = store::read_linked(&root, &repository, code);
                    store::advance_baseline(
                        &tx,
                        project,
                        &root,
                        &note_path,
                        code,
                        &file,
                        why.as_deref(),
                    )?;
                }
                tx.execute(
                    "UPDATE doc_drift_signals SET state='ignored',updated_at=?3 WHERE project_id=?1 AND note_path=?2 AND state IN ('open','proposed')",
                    params![project, note_path, at],
                )?;
                tx.execute(
                    "UPDATE doc_reconciliation_proposals SET state='rejected',reason=COALESCE(?3,reason),updated_at=?4 WHERE project_id=?1 AND note_path=?2 AND state IN ('pending','edited')",
                    params![project, note_path, why, at],
                )?;
                store::collect_blobs(&tx, project)?;
                None
            }
            ReconciliationAction::Silence {
                note_path,
                code_path,
            } => {
                let changed = tx.execute(
                    "UPDATE doc_reconciliation_baselines SET state='silenced',updated_at=?4 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND state='tracking'",
                    params![project, note_path, code_path, at],
                )?;
                if changed != 1 {
                    return Err(invalid("This link is not tracked"));
                }
                tx.execute(
                    "UPDATE doc_drift_signals SET state='ignored',updated_at=?4 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND state IN ('open','proposed')",
                    params![project, note_path, code_path, at],
                )?;
                None
            }
            ReconciliationAction::Unsilence {
                note_path,
                code_path,
            } => {
                let changed = tx.execute(
                    "UPDATE doc_reconciliation_baselines SET state='tracking',updated_at=?4 WHERE project_id=?1 AND note_path=?2 AND code_path=?3 AND state='silenced'",
                    params![project, note_path, code_path, at],
                )?;
                if changed != 1 {
                    return Err(invalid("This link is not silenced"));
                }
                // Tracking starts again from the file as it is now.
                let file = store::read_linked(&root, &repository, &code_path);
                store::advance_baseline(&tx, project, &root, &note_path, &code_path, &file, None)?;
                store::collect_blobs(&tx, project)?;
                None
            }
        };
        tx.commit()?;
        Ok(outcome)
    }
}

const PROPOSAL_COLUMNS: &str = "id,note_path,state,expected_note_sha256,expected_code_json,summary_json,edits_json,unresolved_json,code_diffs_json,proposed_content,preview_diff,reason,updated_at";

fn parse<T: serde::de::DeserializeOwned>(index: usize, text: String) -> rusqlite::Result<T> {
    serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn map_proposal(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReconciliationProposal> {
    Ok(ReconciliationProposal {
        id: r.get(0)?,
        note_path: r.get(1)?,
        state: ProposalState::parse(&r.get::<_, String>(2)?),
        expected_note_sha256: r.get(3)?,
        expected_code: parse(4, r.get(4)?)?,
        change_summary: parse(5, r.get(5)?)?,
        edits: parse(6, r.get(6)?)?,
        unresolved: parse(7, r.get(7)?)?,
        code_changes: parse(8, r.get(8)?)?,
        proposed_content: r.get(9)?,
        preview_diff: r.get(10)?,
        reason: r.get(11)?,
        updated_at: r.get(12)?,
    })
}

fn open_proposals(db: &Connection, project: &str) -> Result<Vec<ReconciliationProposal>, Error> {
    Ok(db
        .prepare(&format!(
            "SELECT {PROPOSAL_COLUMNS} FROM doc_reconciliation_proposals WHERE project_id=?1 AND state IN ('pending','edited') ORDER BY note_path"
        ))?
        .query_map([project], map_proposal)?
        .collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn proposal(
    db: &Connection,
    project: &str,
    id: &str,
) -> Result<Option<ReconciliationProposal>, Error> {
    Ok(db
        .query_row(
            &format!("SELECT {PROPOSAL_COLUMNS} FROM doc_reconciliation_proposals WHERE project_id=?1 AND id=?2"),
            params![project, id],
            map_proposal,
        )
        .optional()?)
}

fn open_proposal(
    db: &Connection,
    project: &str,
    id: &str,
) -> Result<ReconciliationProposal, Error> {
    proposal(db, project, id)?
        .filter(|proposal| proposal.state.is_open())
        .ok_or_else(|| invalid("This proposal is no longer open; refresh the list"))
}

pub(crate) fn preview_diff(current: &str, proposed: &str) -> String {
    use crate::orchestration::composition::line_diff::{self, LineDiff};
    match line_diff::unified(current, proposed, 32_000) {
        LineDiff::Identical => String::new(),
        LineDiff::Unified { text, truncated } => {
            if truncated {
                format!("{text}\n[diff truncated]")
            } else {
                text
            }
        }
        LineDiff::TooLarge => "[the change is too large to preview; compare the full text]".into(),
    }
}

fn invalidate(
    db: &Connection,
    project: &str,
    proposal: &ReconciliationProposal,
    why: &str,
) -> Result<ApprovalOutcome, Error> {
    db.execute(
        "UPDATE doc_reconciliation_proposals SET state='invalidated',reason=?3,updated_at=?4 WHERE project_id=?1 AND id=?2",
        params![project, proposal.id, why, now()],
    )?;
    db.execute(
        "UPDATE doc_drift_signals SET state='open',updated_at=?3 WHERE project_id=?1 AND note_path=?2 AND state='proposed'",
        params![project, proposal.note_path, now()],
    )?;
    Ok(ApprovalOutcome::Invalidated(why.into()))
}

/// Publishes a proposal only on the bytes it was made against, and never
/// while a pulsation in flight has the note as a source or a destination.
fn approve(
    db: &Connection,
    project: &str,
    root: &Path,
    repository: &RepositoryContext,
    proposal: &ReconciliationProposal,
) -> Result<ApprovalOutcome, Error> {
    let read = crate::workspace_service::read_workspace_file(
        root,
        crate::workspace_service::ReadWorkspaceFileRequest {
            relative_path: proposal.note_path.clone(),
        },
    );
    let Ok(read) = read else {
        return invalidate(
            db,
            project,
            proposal,
            "changed since proposal: the note can no longer be read",
        );
    };
    if store::sha256_hex(read.content.as_bytes()) != proposal.expected_note_sha256 {
        return invalidate(
            db,
            project,
            proposal,
            "changed since proposal: the note was edited",
        );
    }
    for expected in &proposal.expected_code {
        let file = store::read_linked(root, repository, &expected.path);
        if file.sha256() != expected.sha256.as_deref() {
            return invalidate(
                db,
                project,
                proposal,
                &format!("changed since proposal: {} changed again", expected.path),
            );
        }
    }
    let in_use: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM artifact_publications o JOIN pulsations p ON p.id=o.pulsation_id WHERE p.project_id=?1 AND p.status NOT IN ('done','failed') AND o.relative_path=?2 AND o.state IN ('pending','applying'))
            OR EXISTS(SELECT 1 FROM context_retrieval_items i JOIN context_retrieval_runs r ON r.id=i.retrieval_run_id JOIN pulsations p ON p.id=r.pulsation_id WHERE p.project_id=?1 AND p.status NOT IN ('done','failed') AND i.relative_path=?2 AND i.delivered=1)",
        params![project, proposal.note_path],
        |r| r.get(0),
    )?;
    if in_use {
        return Err(invalid(
            "note_in_use_by_pulsation: a pulsation in progress uses this note; approve again when it finishes",
        ));
    }
    let written = crate::workspace_service::write_workspace_file_atomic(
        root,
        crate::workspace_service::WriteWorkspaceFileAtomicRequest {
            relative_path: proposal.note_path.clone(),
            content: proposal.proposed_content.clone(),
            expected_revision: read.revision,
        },
    );
    if written.is_err() {
        return invalidate(
            db,
            project,
            proposal,
            "changed since proposal: the note changed while it was written",
        );
    }
    let at = now();
    for expected in &proposal.expected_code {
        let file = store::read_linked(root, repository, &expected.path);
        store::advance_baseline(
            db,
            project,
            root,
            &proposal.note_path,
            &expected.path,
            &file,
            None,
        )?;
    }
    db.execute(
        "UPDATE doc_drift_signals SET state='resolved',updated_at=?3 WHERE project_id=?1 AND note_path=?2 AND state IN ('open','proposed')",
        params![project, proposal.note_path, at],
    )?;
    db.execute(
        "UPDATE doc_reconciliation_proposals SET state='published',updated_at=?3 WHERE project_id=?1 AND id=?2",
        params![project, proposal.id, at],
    )?;
    store::collect_blobs(db, project)?;
    Ok(ApprovalOutcome::Published)
}
