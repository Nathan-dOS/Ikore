//! Documentation reconciliation in the UI (ADR-034 M5): the status-bar
//! indicator, the Vault list of notes whose linked code changed, and the
//! comparison where the developer approves, edits, ignores or silences.
//!
//! Detection runs in the core and only hashes; a proposal reaches the model
//! only from [`DocDriftAction::Propose`] (decision D6).

use std::collections::HashSet;
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Task};
use ikore_core::doc_reconciliation::{
    ApprovalOutcome, DocDriftOverview, DriftedNote, ReconciliationAction,
};

use crate::AppModel;
use crate::text_input::NativeTextInput;

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(900);

pub(crate) struct DocDriftUi {
    pub project_id: Option<String>,
    pub overview: DocDriftOverview,
    /// The note whose comparison is open in the Vault.
    pub open_note: Option<String>,
    pub editing: bool,
    pub editor: Entity<NativeTextInput>,
    pub reason: Entity<NativeTextInput>,
    pub busy: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// Projects already scanned in this session (the scan at project open).
    scanned: HashSet<String>,
    generation: u64,
    refresh_again: bool,
    refresh_task: Option<Task<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DocDriftAction {
    Open(String),
    Close,
    Propose(String),
    Approve(String),
    StartEdit,
    CancelEdit,
    SaveEdit(String),
    Reject(String),
    Ignore(String),
    Silence(String, String),
    Unsilence(String, String),
}

enum Operation {
    Refresh,
    Propose(String),
    Act(ReconciliationAction),
}

impl DocDriftUi {
    pub fn new(cx: &mut Context<AppModel>) -> Self {
        Self {
            project_id: None,
            overview: DocDriftOverview::default(),
            open_note: None,
            editing: false,
            editor: cx.new(|cx| {
                NativeTextInput::new("The note as it will be published", cx)
                    .multiline()
                    .max_rows(24)
            }),
            reason: cx.new(|cx| NativeTextInput::new("Why the note stays as it is (optional)", cx)),
            busy: false,
            error: None,
            notice: None,
            scanned: HashSet::new(),
            generation: 0,
            refresh_again: false,
            refresh_task: None,
        }
    }

    /// The note whose comparison is open, if it is still listed.
    pub fn open(&self) -> Option<&DriftedNote> {
        let note = self.open_note.as_ref()?;
        self.overview.notes.iter().find(|n| &n.note_path == note)
    }

    pub fn generating(&self, note: &str) -> bool {
        self.overview.generating.iter().any(|n| n == note)
    }

    pub fn failure(&self, note: &str) -> Option<&str> {
        self.overview
            .failures
            .iter()
            .find(|(n, _)| n == note)
            .map(|(_, message)| message.as_str())
    }
}

impl AppModel {
    pub(crate) fn refresh_doc_drift(&mut self, cx: &mut Context<Self>) {
        self.run_doc_drift(Operation::Refresh, cx);
    }

    /// Coalesces bursts of workspace events into one refresh.
    pub(crate) fn schedule_doc_drift_refresh(&mut self, cx: &mut Context<Self>) {
        if self.doc_drift.project_id.is_none() {
            return;
        }
        let timer = cx.background_executor().timer(REFRESH_DEBOUNCE);
        self.doc_drift.refresh_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| model.refresh_doc_drift(cx));
        }));
    }

    pub(crate) fn doc_drift_action(&mut self, action: DocDriftAction, cx: &mut Context<Self>) {
        match action {
            DocDriftAction::Open(note) => {
                self.doc_drift.open_note = Some(note);
                self.doc_drift.editing = false;
                self.doc_drift.error = None;
                self.doc_drift.notice = None;
                cx.notify();
            }
            DocDriftAction::Close => {
                self.doc_drift.open_note = None;
                self.doc_drift.editing = false;
                cx.notify();
            }
            DocDriftAction::Propose(note) => self.run_doc_drift(Operation::Propose(note), cx),
            DocDriftAction::Approve(id) => self.run_doc_drift(
                Operation::Act(ReconciliationAction::Approve { proposal_id: id }),
                cx,
            ),
            DocDriftAction::StartEdit => {
                let Some(proposal) = self.doc_drift.open().and_then(|n| n.proposal.clone()) else {
                    return;
                };
                self.doc_drift.editor.update(cx, |input, cx| {
                    input.set_value(proposal.proposed_content, cx)
                });
                self.doc_drift.editing = true;
                cx.notify();
            }
            DocDriftAction::CancelEdit => {
                self.doc_drift.editing = false;
                cx.notify();
            }
            DocDriftAction::SaveEdit(id) => {
                let content = self.doc_drift.editor.read(cx).value().to_owned();
                self.doc_drift.editing = false;
                self.run_doc_drift(
                    Operation::Act(ReconciliationAction::Edit {
                        proposal_id: id,
                        content,
                    }),
                    cx,
                );
            }
            DocDriftAction::Reject(id) => self.run_doc_drift(
                Operation::Act(ReconciliationAction::Reject { proposal_id: id }),
                cx,
            ),
            DocDriftAction::Ignore(note) => {
                let reason = self.doc_drift.reason.read(cx).value().trim().to_owned();
                self.run_doc_drift(
                    Operation::Act(ReconciliationAction::Ignore {
                        note_path: note,
                        reason: (!reason.is_empty()).then_some(reason),
                    }),
                    cx,
                );
            }
            DocDriftAction::Silence(note, code) => self.run_doc_drift(
                Operation::Act(ReconciliationAction::Silence {
                    note_path: note,
                    code_path: code,
                }),
                cx,
            ),
            DocDriftAction::Unsilence(note, code) => self.run_doc_drift(
                Operation::Act(ReconciliationAction::Unsilence {
                    note_path: note,
                    code_path: code,
                }),
                cx,
            ),
        }
    }

    fn run_doc_drift(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        let Some(project) = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
        else {
            return;
        };
        if self.doc_drift.project_id.as_ref() != Some(&project) {
            self.doc_drift.project_id = Some(project.clone());
            self.doc_drift.overview = DocDriftOverview::default();
            self.doc_drift.open_note = None;
            self.doc_drift.editing = false;
            self.doc_drift.generation += 1;
        }
        let background = matches!(operation, Operation::Refresh);
        if self.doc_drift.busy {
            if background {
                self.doc_drift.refresh_again = true;
            }
            return;
        }
        self.doc_drift.busy = true;
        if !background {
            self.doc_drift.error = None;
            self.doc_drift.notice = None;
        }
        let scan = self.doc_drift.scanned.insert(project.clone());
        let generation = self.doc_drift.generation;
        let runtime = core.runtime_handle().clone();
        let current = project.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || -> Result<_, String> {
                    let reconciler = core.doc_reconciler();
                    let mut notice = None;
                    let outcome: Result<(), String> = (|| {
                        if scan {
                            core.persistence()
                                .scan_doc_drift(&project)
                                .map_err(|e| e.to_string())?;
                        }
                        match operation {
                            Operation::Refresh => {}
                            Operation::Propose(note) => {
                                reconciler.propose(&project, &note).map_err(|e| e.to_string())?;
                                notice = Some("Preparing a proposal for this note.".to_owned());
                            }
                            Operation::Act(action) => {
                                let ignored = matches!(action, ReconciliationAction::Ignore { .. });
                                match core
                                    .persistence()
                                    .reconcile_doc_drift(&project, action)
                                    .map_err(|e| e.to_string())?
                                {
                                    Some(ApprovalOutcome::Published) => {
                                        notice = Some("The note was updated.".to_owned());
                                    }
                                    Some(ApprovalOutcome::Invalidated(reason)) => {
                                        return Err(format!(
                                            "The proposal was set aside ({reason}). Ask for a new one."
                                        ));
                                    }
                                    None if ignored => {
                                        notice = Some("The note stays as it is; later changes will show again.".to_owned());
                                    }
                                    None => {}
                                }
                            }
                        }
                        Ok(())
                    })();
                    let overview = reconciler.overview(&project).map_err(|e| e.to_string())?;
                    Ok((overview, outcome.err(), notice))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = this.update(cx, |model, cx| {
                model.doc_drift.busy = false;
                if model.doc_drift.generation != generation
                    || model.doc_drift.project_id.as_ref() != Some(&current)
                {
                    return;
                }
                match result {
                    Ok((overview, error, notice)) => {
                        model.doc_drift.overview = overview;
                        if !background || error.is_some() {
                            model.doc_drift.error = error;
                        }
                        if notice.is_some() {
                            model.doc_drift.notice = notice;
                        }
                    }
                    Err(error) => model.doc_drift.error = Some(error),
                }
                if std::mem::take(&mut model.doc_drift.refresh_again) {
                    model.refresh_doc_drift(cx);
                }
                cx.notify();
            });
        })
        .detach();
        if !background {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn the_comparison_opens_edits_and_closes_without_reaching_the_core(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.doc_drift.overview = crate::test_support::drift_overview(true);
            model.doc_drift_action(
                DocDriftAction::Open("knowledge/obsidian/features/api.md".into()),
                cx,
            );
            let note = model.doc_drift.open().expect("the listed note opens");
            assert_eq!(note.signals.len(), 1);

            // Editing starts from the proposed text.
            model.doc_drift_action(DocDriftAction::StartEdit, cx);
            assert!(model.doc_drift.editing);
            assert_eq!(
                model.doc_drift.editor.read(cx).value(),
                "# API\nThe API returns MessagePack.\n"
            );
            model.doc_drift_action(DocDriftAction::CancelEdit, cx);
            assert!(!model.doc_drift.editing);

            // A note that is no longer listed has no comparison.
            model.doc_drift.open_note = Some("knowledge/obsidian/features/gone.md".into());
            assert!(model.doc_drift.open().is_none());
            model.doc_drift_action(DocDriftAction::Close, cx);
            assert!(model.doc_drift.open_note.is_none());

            // Without a proposal there is nothing to edit.
            model.doc_drift.overview = crate::test_support::drift_overview(false);
            model.doc_drift_action(
                DocDriftAction::Open("knowledge/obsidian/features/api.md".into()),
                cx,
            );
            model.doc_drift_action(DocDriftAction::StartEdit, cx);
            assert!(!model.doc_drift.editing);
        });
    }

    #[test]
    fn the_indicator_counts_notes_with_open_changes_only() {
        let mut overview = crate::test_support::drift_overview(true);
        assert_eq!(overview.drifted(), 1);
        overview.notes[0].signals.clear();
        assert_eq!(
            overview.drifted(),
            0,
            "a proposal alone is not a pending change"
        );
    }
}
