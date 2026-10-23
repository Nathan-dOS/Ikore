//! Notes view state (ADR-029): the capture box, search and filters, the
//! selected note and its Scribe conversation.
//!
//! Loads and changes follow the Forge's pattern (`forge.rs`): the work runs on
//! the blocking pool, a generation guard drops stale results, and one snapshot
//! is applied on the UI thread. The Scribe answers later; its `AppEvent::Notes`
//! triggers the refresh that shows the result.
use crate::{
    AppModel, AppView,
    activity::ActivityKind,
    flow::FlowTarget,
    session::HeaderNoticeKind,
    text_input::{NativeTextInput, Submit},
};
use gpui::{AppContext, Context, Entity, Subscription};
use hvc_core::companion_notes::*;
use std::collections::{BTreeSet, HashMap, HashSet};

pub(crate) struct NotesUi {
    pub snapshot: NotesSnapshot,
    pub selected: Option<String>,
    /// The selected note's Scribe turns, oldest first.
    pub history: Vec<NoteTurn>,
    pub capture: Entity<NativeTextInput>,
    pub search: Entity<NativeTextInput>,
    pub refine: Entity<NativeTextInput>,
    _subscriptions: Vec<Subscription>,
    /// A note shows when it carries any of these tags; empty shows all.
    pub tag_filter: BTreeSet<NoteTag>,
    pub source_filter: Option<NoteSource>,
    pub raw_open: bool,
    /// Failed turns whose technical detail is open.
    pub expanded_failures: HashSet<String>,
    /// The note whose Delete was clicked once and waits for the confirmation.
    pub confirm_delete: Option<String>,
    pub busy: bool,
    refreshing: bool,
    refresh_pending: bool,
    pub generation: u64,
    pub project_id: Option<String>,
    pub error: Option<String>,
}

impl NotesUi {
    pub fn new(cx: &mut Context<AppModel>) -> Self {
        let capture = cx.new(|cx| {
            NativeTextInput::new("Drop a thought; the Notes Scribe organizes it", cx)
                .multiline()
                .max_rows(6)
        });
        let search =
            cx.new(|cx| NativeTextInput::new("Search notes", cx).leading_inset(gpui::px(22.0)));
        let refine = cx.new(|cx| {
            NativeTextInput::new("Ask the Scribe to change this note", cx)
                .multiline()
                .max_rows(6)
        });
        let subscriptions = vec![
            cx.subscribe(&capture, |model, input, _: &Submit, cx| {
                if input == model.notes.capture {
                    model.notes_action(NotesAction::Capture, cx);
                }
            }),
            cx.subscribe(&refine, |model, input, _: &Submit, cx| {
                if input == model.notes.refine {
                    model.notes_action(NotesAction::Refine, cx);
                }
            }),
            cx.observe(&capture, |_, _, cx| cx.notify()),
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&refine, |_, _, cx| cx.notify()),
        ];
        Self {
            snapshot: NotesSnapshot::default(),
            selected: None,
            history: Vec::new(),
            capture,
            search,
            refine,
            _subscriptions: subscriptions,
            tag_filter: BTreeSet::new(),
            source_filter: None,
            raw_open: false,
            expanded_failures: HashSet::new(),
            confirm_delete: None,
            busy: false,
            refreshing: false,
            refresh_pending: false,
            generation: 0,
            project_id: None,
            error: None,
        }
    }

    pub fn note(&self) -> Option<&CompanionNote> {
        self.selected
            .as_ref()
            .and_then(|id| self.snapshot.notes.iter().find(|note| &note.id == id))
    }

    /// The turn the Scribe is running on the selected note.
    pub fn running_turn(&self) -> Option<&String> {
        self.selected
            .as_ref()
            .and_then(|id| self.snapshot.running.get(id))
    }

    pub fn has_voice_notes(&self) -> bool {
        self.snapshot
            .notes
            .iter()
            .any(|note| note.source_type == NoteSource::Voice)
    }

    fn capture_operation(
        &self,
        project: Option<&str>,
        cx: &Context<AppModel>,
    ) -> Option<Operation> {
        let text = self.capture.read(cx).value();
        if self.busy || project != self.project_id.as_deref() || text.trim().is_empty() {
            return None;
        }
        Some(Operation::Capture(text.into()))
    }

    fn refine_operation(&self, project: Option<&str>, cx: &Context<AppModel>) -> Option<Operation> {
        let note = self.note()?;
        let message = self.refine.read(cx).value();
        if self.busy
            || project != self.project_id.as_deref()
            || note.status != NoteStatus::Ready
            || self.running_turn().is_some()
            || message.trim().is_empty()
        {
            return None;
        }
        Some(Operation::Refine(
            note.id.clone(),
            note.revision,
            message.into(),
        ))
    }

    /// Clears a composer only if it still holds what was sent: the developer
    /// may already be typing the next thought.
    fn clear_if_unchanged(input: &Entity<NativeTextInput>, sent: &str, cx: &mut Context<AppModel>) {
        if input.read(cx).value() == sent {
            input.update(cx, |input, cx| input.clear(cx));
        }
    }

    fn begin_operation(&mut self, operation: &Operation) -> bool {
        let background = matches!(operation, Operation::Refresh);
        if self.busy || (background && self.refreshing) {
            if background {
                self.refresh_pending = true;
            }
            return false;
        }
        self.refreshing = background;
        self.busy = !background;
        self.refresh_pending = false;
        self.generation += 1;
        true
    }

    fn finish_operation(&mut self) -> bool {
        self.busy = false;
        self.refreshing = false;
        std::mem::take(&mut self.refresh_pending)
    }
}

/// The notes the list shows: every search term somewhere in the note's text,
/// any of the selected tags, and the selected source.
pub(crate) fn visible_notes<'a>(
    notes: &'a [CompanionNote],
    query: &str,
    tags: &BTreeSet<NoteTag>,
    source: Option<NoteSource>,
) -> Vec<&'a CompanionNote> {
    let terms = query
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    notes
        .iter()
        .filter(|note| source.is_none_or(|source| note.source_type == source))
        .filter(|note| tags.is_empty() || note.tags.iter().any(|tag| tags.contains(tag)))
        .filter(|note| {
            if terms.is_empty() {
                return true;
            }
            let text = [
                note.title.as_deref(),
                note.summary.as_deref(),
                note.content_markdown.as_deref(),
                Some(note.raw_input.as_str()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
            terms.iter().all(|term| text.contains(term))
        })
        .collect()
}

#[derive(Clone)]
pub(crate) enum NotesAction {
    Refresh,
    Capture,
    Select(String),
    ToggleTag(NoteTag),
    SetSource(Option<NoteSource>),
    ClearFilters,
    ToggleRaw,
    Refine,
    /// Cancel the Scribe turn with this id.
    Cancel(String),
    RetryStructure,
    ToggleFailureDetail(String),
    /// First click on Delete; `ConfirmDelete` is the second.
    Delete,
    ConfirmDelete,
}

enum Operation {
    Refresh,
    Capture(String),
    Refine(String, u64, String),
    RetryStructure(String, u64),
    Delete(String, u64),
}

impl AppModel {
    pub(crate) fn refresh_notes(&mut self, cx: &mut Context<Self>) {
        self.run_notes(Operation::Refresh, cx);
    }

    fn current_project(&self, cx: &Context<Self>) -> Option<String> {
        self.workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
    }

    pub(crate) fn notes_action(&mut self, action: NotesAction, cx: &mut Context<Self>) {
        let project = self.current_project(cx);
        match action {
            NotesAction::Refresh => self.refresh_notes(cx),
            NotesAction::Capture => {
                if let Some(operation) = self.notes.capture_operation(project.as_deref(), cx) {
                    self.run_notes(operation, cx);
                }
            }
            NotesAction::Refine => {
                if let Some(operation) = self.notes.refine_operation(project.as_deref(), cx) {
                    self.run_notes(operation, cx);
                }
            }
            NotesAction::Select(id) => {
                if self.notes.selected.as_ref() != Some(&id) {
                    self.notes.selected = Some(id);
                    self.notes.history.clear();
                    self.notes.raw_open = false;
                    self.notes.confirm_delete = None;
                    self.refresh_notes(cx);
                }
                cx.notify();
            }
            NotesAction::ToggleTag(tag) => {
                if !self.notes.tag_filter.remove(&tag) {
                    self.notes.tag_filter.insert(tag);
                }
                cx.notify();
            }
            NotesAction::SetSource(source) => {
                self.notes.source_filter = source;
                cx.notify();
            }
            NotesAction::ClearFilters => {
                self.notes.tag_filter.clear();
                self.notes.source_filter = None;
                self.notes.search.update(cx, |input, cx| input.clear(cx));
                cx.notify();
            }
            NotesAction::ToggleRaw => {
                self.notes.raw_open = !self.notes.raw_open;
                cx.notify();
            }
            NotesAction::Cancel(turn) => {
                if let Some(core) = &self.core {
                    core.notes_scribe().cancel(&turn);
                }
                cx.notify();
            }
            NotesAction::RetryStructure => {
                let retry = self.notes.note().filter(|note| {
                    note.status == NoteStatus::Failed && self.notes.running_turn().is_none()
                });
                if let Some(note) = retry {
                    let operation = Operation::RetryStructure(note.id.clone(), note.revision);
                    self.run_notes(operation, cx);
                }
            }
            NotesAction::ToggleFailureDetail(turn) => {
                if !self.notes.expanded_failures.remove(&turn) {
                    self.notes.expanded_failures.insert(turn);
                }
                cx.notify();
            }
            NotesAction::Delete => {
                self.notes.confirm_delete = self.notes.selected.clone();
                cx.notify();
            }
            NotesAction::ConfirmDelete => {
                let armed = self.notes.confirm_delete.take();
                if let Some(note) = self
                    .notes
                    .note()
                    .filter(|note| Some(&note.id) == armed.as_ref())
                {
                    let operation = Operation::Delete(note.id.clone(), note.revision);
                    self.run_notes(operation, cx);
                }
                cx.notify();
            }
        }
    }

    fn run_notes(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        let Some(project) = self.current_project(cx) else {
            return;
        };
        if self.notes.project_id.as_ref() != Some(&project) {
            let generation = self.notes.generation;
            self.notes = NotesUi::new(cx);
            self.notes.generation = generation;
            self.notes.project_id = Some(project.clone());
        }
        let background = matches!(operation, Operation::Refresh);
        if !self.notes.begin_operation(&operation) {
            return;
        }
        if !background {
            self.notes.error = None;
        }
        let generation = self.notes.generation;
        let original_selection = self.notes.selected.clone();
        let selected = original_selection.clone();
        let runtime = core.runtime_handle().clone();
        let current_project = project.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || -> Result<_, String> {
                    let notes = CompanionNotesService::new(core.persistence().clone());
                    let scribe = core.notes_scribe();
                    let key = uuid::Uuid::new_v4().to_string();
                    let mut selected = selected;
                    let mut sent_capture = None;
                    let mut sent_refine = None;
                    let outcome: Result<
                        (),
                        hvc_core::persistence_service::PersistenceServiceError,
                    > = (|| {
                        match operation {
                            Operation::Refresh => {}
                            Operation::Capture(text) => {
                                let note = scribe.capture(CaptureNoteRequest {
                                    project_id: project.clone(),
                                    request_id: key,
                                    raw_input: text.clone(),
                                    source_type: NoteSource::Text,
                                })?;
                                selected = Some(note.id);
                                sent_capture = Some(text);
                            }
                            Operation::Refine(id, revision, message) => {
                                scribe.refine(&project, &id, revision, &key, &message)?;
                                sent_refine = Some(message);
                            }
                            Operation::RetryStructure(id, revision) => {
                                scribe.retry_structure(&project, &id, revision, &key)?;
                            }
                            Operation::Delete(id, revision) => {
                                notes.delete(&project, &id, revision)?;
                                if selected.as_ref() == Some(&id) {
                                    selected = None;
                                }
                            }
                        }
                        Ok(())
                    })();
                    let error = outcome.err().map(|e| e.to_string());
                    let snapshot = notes.snapshot(&project).map_err(|e| e.to_string())?;
                    if selected
                        .as_ref()
                        .is_some_and(|id| !snapshot.notes.iter().any(|note| &note.id == id))
                    {
                        selected = None;
                    }
                    let history = match &selected {
                        Some(id) => notes.history(&project, id).map_err(|e| e.to_string())?,
                        None => Vec::new(),
                    };
                    Ok((
                        snapshot,
                        selected,
                        history,
                        sent_capture,
                        sent_refine,
                        error,
                    ))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = this.update(cx, |model, cx| {
                if model.notes.generation != generation
                    || model.current_project(cx).as_ref() != Some(&current_project)
                {
                    return;
                }
                match result {
                    Ok((snapshot, selected, history, sent_capture, sent_refine, error)) => {
                        let before = std::mem::take(&mut model.notes.snapshot.running);
                        model.notes.snapshot = snapshot;
                        model.report_finished_notes(&before, cx);
                        if model.notes.selected == original_selection {
                            model.notes.selected = selected;
                            model.notes.history = history;
                        }
                        if let Some(text) = sent_capture {
                            NotesUi::clear_if_unchanged(&model.notes.capture, &text, cx);
                        }
                        if let Some(message) = sent_refine {
                            NotesUi::clear_if_unchanged(&model.notes.refine, &message, cx);
                        }
                        if !background || error.is_some() {
                            model.notes.error = error;
                        }
                    }
                    Err(error) => model.notes.error = Some(error),
                }
                if model.notes.finish_operation() {
                    model.refresh_notes(cx);
                }
                cx.notify();
            });
        })
        .detach();
        if !background {
            cx.notify();
        }
    }

    /// One activity line per Scribe turn that finished since the last snapshot,
    /// and a header notice when the developer is not looking at Notes: a capture
    /// is meant to be fire-and-forget.
    fn report_finished_notes(&mut self, before: &HashMap<String, String>, cx: &mut Context<Self>) {
        let finished = before
            .keys()
            .filter(|id| !self.notes.snapshot.running.contains_key(*id))
            .filter_map(|id| self.notes.snapshot.notes.iter().find(|note| &note.id == id))
            .map(|note| (note.status, note.revision, note.display_title()))
            .collect::<Vec<_>>();
        let watching = self.active_view() == AppView::Notes;
        for (status, revision, title) in finished {
            let target = Some(FlowTarget::View(AppView::Notes));
            match status {
                NoteStatus::Ready => {
                    let line = if revision <= 2 {
                        format!("Note ready: {title}")
                    } else {
                        format!("Note updated: {title}")
                    };
                    self.record_activity(ActivityKind::Notes, line.clone(), target, false);
                    if !watching {
                        self.push_header_notice(HeaderNoticeKind::Notice, line, cx);
                    }
                }
                NoteStatus::Failed => {
                    let line = format!("The Notes Scribe could not structure: {title}");
                    self.record_activity(ActivityKind::Notes, line, target, true);
                    if !watching {
                        self.push_header_notice(
                            HeaderNoticeKind::Warning,
                            "A note could not be structured. Its text is saved; open Notes to retry.",
                            cx,
                        );
                    }
                }
                NoteStatus::Structuring => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn note(id: &str, title: &str, tags: &[NoteTag], source: NoteSource) -> CompanionNote {
        CompanionNote {
            id: id.into(),
            project_id: "project".into(),
            revision: 2,
            status: NoteStatus::Ready,
            source_type: source,
            raw_input: format!("raw words for {id}"),
            raw_redactions: 0,
            title: Some(title.into()),
            summary: Some("A summary".into()),
            content_markdown: Some("- A point".into()),
            tags: tags.to_vec(),
            output_language: Some("en".into()),
            created_at: "2026-10-22T12:00:00Z".into(),
            updated_at: "2026-10-22T12:00:00Z".into(),
        }
    }

    #[test]
    fn the_list_matches_every_term_any_selected_tag_and_the_source() {
        let notes = vec![
            note(
                "a",
                "Dark mode contrast",
                &[NoteTag::UiUx],
                NoteSource::Text,
            ),
            note(
                "b",
                "Slow search",
                &[NoteTag::Performance, NoteTag::Bug],
                NoteSource::Text,
            ),
            note("c", "Spoken idea", &[NoteTag::Feature], NoteSource::Voice),
        ];
        let ids = |query: &str, tags: &[NoteTag], source: Option<NoteSource>| {
            visible_notes(&notes, query, &tags.iter().copied().collect(), source)
                .into_iter()
                .map(|note| note.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("", &[], None), ["a", "b", "c"]);
        assert_eq!(ids("  DARK   contrast ", &[], None), ["a"]);
        assert_eq!(ids("dark search", &[], None), Vec::<&str>::new());
        assert_eq!(
            ids("raw words for b", &[], None),
            ["b"],
            "the raw capture is searched"
        );
        assert_eq!(ids("", &[NoteTag::Bug, NoteTag::UiUx], None), ["a", "b"]);
        assert_eq!(ids("", &[], Some(NoteSource::Voice)), ["c"]);
        assert_eq!(ids("slow", &[NoteTag::UiUx], None), Vec::<&str>::new());
    }

    #[gpui::test]
    fn composers_are_admitted_only_for_the_open_project_and_a_free_note(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            let notes = &mut model.notes;
            notes.project_id = Some("project".into());
            for blank in ["", " \n\t"] {
                notes
                    .capture
                    .update(cx, |input, cx| input.set_value(blank, cx));
                assert!(notes.capture_operation(Some("project"), cx).is_none());
            }
            notes
                .capture
                .update(cx, |input, cx| input.set_value("A thought", cx));
            assert!(matches!(
                notes.capture_operation(Some("project"), cx),
                Some(Operation::Capture(text)) if text == "A thought"
            ));
            assert!(notes.capture_operation(Some("other"), cx).is_none());
            notes.busy = true;
            assert!(notes.capture_operation(Some("project"), cx).is_none());
            notes.busy = false;

            notes
                .refine
                .update(cx, |input, cx| input.set_value("Shorter", cx));
            assert!(
                notes.refine_operation(Some("project"), cx).is_none(),
                "nothing selected"
            );
            notes.snapshot.notes = vec![note("a", "Title", &[NoteTag::Bug], NoteSource::Text)];
            notes.selected = Some("a".into());
            assert!(matches!(
                notes.refine_operation(Some("project"), cx),
                Some(Operation::Refine(id, 2, text)) if id == "a" && text == "Shorter"
            ));
            notes.snapshot.running.insert("a".into(), "turn".into());
            assert!(
                notes.refine_operation(Some("project"), cx).is_none(),
                "the Scribe is busy"
            );
            notes.snapshot.running.clear();
            notes.snapshot.notes[0].status = NoteStatus::Failed;
            assert!(
                notes.refine_operation(Some("project"), cx).is_none(),
                "not structured"
            );

            NotesUi::clear_if_unchanged(&notes.refine, "Older", cx);
            assert_eq!(notes.refine.read(cx).value(), "Shorter");
            NotesUi::clear_if_unchanged(&notes.refine, "Shorter", cx);
            assert_eq!(notes.refine.read(cx).value(), "");
        });
    }

    #[gpui::test]
    fn a_finished_turn_is_reported_once_and_noticed_away_from_notes(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.notes.snapshot.notes = vec![
                note(
                    "a",
                    "Dark mode contrast",
                    &[NoteTag::UiUx],
                    NoteSource::Text,
                ),
                CompanionNote {
                    status: NoteStatus::Failed,
                    title: None,
                    ..note("b", "", &[], NoteSource::Text)
                },
            ];
            let before = HashMap::from([
                ("a".to_string(), "t1".to_string()),
                ("b".to_string(), "t2".to_string()),
            ]);
            model.report_finished_notes(&before, cx);
            let lines = model
                .activity
                .recent(10)
                .map(|entry| (entry.summary.to_string(), entry.attention))
                .collect::<Vec<_>>();
            assert!(lines.contains(&("Note ready: Dark mode contrast".to_string(), false)));
            assert!(lines.contains(&(
                "The Notes Scribe could not structure: raw words for b".to_string(),
                true
            )));
            assert!(
                model.current_notice().is_some(),
                "away from Notes, a notice shows"
            );
            model.report_finished_notes(&HashMap::new(), cx);
            assert_eq!(
                model.activity.recent(10).count(),
                lines.len(),
                "nothing new finished"
            );
        });
    }
}
