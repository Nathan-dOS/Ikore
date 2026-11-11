//! Canvas lifecycle and commands. Blocking persistence never runs in paint or pointer handlers.
pub(crate) mod grid;
pub(crate) mod state;

#[cfg(test)]
#[path = "canvas_authored_navigation_tests.rs"]
mod authored_navigation_tests;

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding, KeyDownEvent,
    Pixels, Point, Subscription, Task, Window, actions,
};
use ikore_core::companion_drawings::{
    CanvasBackgroundMode, CanvasConnectorCurve, CanvasElementKindV1, CanvasSemanticColor,
    CanvasStrokeRendering, CanvasStrokeStyle, CanvasStrokeWidth, geometry,
    service::{CompanionDrawing, CompanionDrawingSummary},
};

use crate::{
    AppModel, native_editor::NativeEditor, split_pane::SplitPaneState, text_input::NativeTextInput,
};
pub(crate) use state::{EditorState, Tool};

actions!(
    canvas_label,
    [ApplyLabel, NewlineLabel, CancelLabel, SaveLabel]
);

pub(crate) fn register_canvas_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", ApplyLabel, Some("CanvasLabel")),
        KeyBinding::new("ctrl-enter", ApplyLabel, Some("CanvasLabel")),
        KeyBinding::new("shift-enter", NewlineLabel, Some("CanvasLabel")),
        KeyBinding::new("escape", CancelLabel, Some("CanvasLabel")),
        KeyBinding::new("ctrl-s", SaveLabel, Some("CanvasLabel")),
    ]);
}

pub(crate) struct CanvasUi {
    pub project_id: Option<String>,
    pub drawings: Vec<CompanionDrawingSummary>,
    pub active: Option<CompanionDrawing>,
    pub editor: EditorState,
    pub search: Entity<NativeTextInput>,
    pub title: Entity<NativeTextInput>,
    pub label: Entity<NativeEditor>,
    pub editing_label: Option<String>,
    pub renaming: bool,
    pub confirm_delete: bool,
    pub list_collapsed: bool,
    pub split: Entity<SplitPaneState>,
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    pub focus: FocusHandle,
    pub busy: bool,
    pub saving: bool,
    pub syncing: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub redaction_notice: bool,
    pub show_explanation: bool,
    draft_title: Option<String>,
    generation: u64,
    library_epoch: u64,
    autosave_generation: u64,
    operation_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    sync_task: Option<Task<()>>,
    autosave_task: Option<Task<()>>,
    viewport_task: Option<Task<()>>,
    pending_action: Option<CanvasAction>,
    sync_requested: bool,
    focus_observed: bool,
    label_focus_generation: u64,
    subscriptions: Vec<Subscription>,
}

impl CanvasUi {
    pub fn new(cx: &mut Context<AppModel>) -> Self {
        let search = cx.new(|cx| NativeTextInput::new("Search drawings", cx));
        let title = cx.new(|cx| NativeTextInput::new("Drawing title", cx));
        let label = cx.new(NativeEditor::new);
        let subscriptions = vec![
            cx.observe(&search, |_, _, cx| cx.notify()),
            cx.observe(&title, |_, _, cx| cx.notify()),
            cx.observe(&label, |_, _, cx| cx.notify()),
        ];
        Self {
            project_id: None,
            drawings: Vec::new(),
            active: None,
            editor: EditorState::default(),
            search,
            title,
            label,
            editing_label: None,
            renaming: false,
            confirm_delete: false,
            list_collapsed: false,
            split: cx.new(|_| SplitPaneState::new(240., 180., 400., 300.)),
            bounds: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
            busy: false,
            saving: false,
            syncing: false,
            error: None,
            notice: None,
            redaction_notice: false,
            show_explanation: false,
            draft_title: None,
            generation: 0,
            library_epoch: 0,
            autosave_generation: 0,
            operation_task: None,
            save_task: None,
            sync_task: None,
            autosave_task: None,
            viewport_task: None,
            pending_action: None,
            subscriptions,
            sync_requested: false,
            focus_observed: false,
            label_focus_generation: 0,
        }
    }

    pub fn status(&self) -> &'static str {
        if self.saving {
            "Saving"
        } else if self.error.is_some() && self.editor.dirty {
            "Save failed"
        } else if self.editor.dirty || self.editing_label.is_some() {
            "Unsaved"
        } else {
            self.active
                .as_ref()
                .map_or("No drawing", |d| d.sync_status.label())
        }
    }

    pub fn has_pending(&self) -> bool {
        self.editor.dirty
            || self.saving
            || self.busy
            || self.editing_label.is_some()
            || self.renaming
    }

    fn set_active(&mut self, drawing: Option<CompanionDrawing>, cx: &mut Context<AppModel>) {
        self.editing_label = None;
        self.label_focus_generation = self.label_focus_generation.saturating_add(1);
        self.renaming = false;
        self.confirm_delete = false;
        self.redaction_notice = false;
        self.draft_title = None;
        self.autosave_generation = self.autosave_generation.saturating_add(1);
        self.autosave_task.take();
        self.editor.load(
            drawing
                .as_ref()
                .map_or_else(Default::default, |d| d.scene.clone()),
            drawing
                .as_ref()
                .map_or_else(Default::default, |d| d.viewport),
        );
        self.title.update(cx, |input, cx| {
            input.set_value(drawing.as_ref().map_or("", |d| d.title.as_str()), cx)
        });
        self.active = drawing;
    }

    /// A completed write acknowledges its captured revision, never a newer gesture or input.
    fn acknowledge_save(
        &mut self,
        drawing: CompanionDrawing,
        captured_change: u64,
        cx: &mut Context<AppModel>,
    ) {
        self.library_epoch = self.library_epoch.saturating_add(1);
        if self.editor.change == captured_change && !self.editor.gesturing() {
            let title = self
                .draft_title
                .as_deref()
                .or_else(|| self.active.as_ref().map(|active| active.title.as_str()));
            let title_redacted = title.is_some_and(|before| {
                before != drawing.title
                    && ikore_core::secret_redactor::introduces_redaction_marker(
                        before,
                        &drawing.title,
                    )
            });
            let label_redacted = drawing.scene.elements.iter().any(|saved| {
                self.editor.scene.elements.iter().any(|before| {
                    before.id == saved.id
                        && before.label != saved.label
                        && ikore_core::secret_redactor::introduces_redaction_marker(
                            &before.label,
                            &saved.label,
                        )
                })
            });
            self.redaction_notice = title_redacted || label_redacted;
            self.editor.scene = drawing.scene.clone();
            self.editor.refresh_diagnostics();
            self.editor.dirty = false;
            self.draft_title = None;
            // A title input opened during this write is still owned by the developer.
            if !self.renaming {
                self.title
                    .update(cx, |input, cx| input.set_value(drawing.title.clone(), cx));
            }
        }
        if let Some(item) = self.drawings.iter_mut().find(|d| d.id == drawing.id) {
            *item = drawing.summary();
        }
        self.active = Some(drawing);
    }

    fn apply_sync_snapshot(&mut self, captured_epoch: u64, drawings: Vec<CompanionDrawingSummary>) {
        if let Some(active) = &mut self.active
            && let Some(summary) = drawings.iter().find(|drawing| drawing.id == active.id)
            && summary.revision == active.revision
        {
            active.sync_status = summary.sync_status;
        }
        // Sync status is advisory metadata. Only a canonical document-list load removes
        // rows; a sync callback cannot erase a newly created drawing from the sidebar.
        for current in &mut self.drawings {
            if let Some(summary) = drawings
                .iter()
                .find(|drawing| drawing.id == current.id && drawing.revision >= current.revision)
            {
                *current = summary.clone();
            }
        }
        if captured_epoch == self.library_epoch {
            for summary in drawings {
                if !self.drawings.iter().any(|current| current.id == summary.id) {
                    self.drawings.push(summary);
                }
            }
        }
    }
}

#[derive(Clone)]
pub(crate) enum CanvasAction {
    New,
    Open(String),
    Refresh,
    BeginRename,
    Rename,
    Duplicate,
    Delete,
    ConfirmDelete,
    Save,
    RetryFiles,
    ExportCopy,
    Discard,
    Tool(Tool),
    Undo,
    Redo,
    Remove,
    Color(CanvasSemanticColor),
    Width(CanvasStrokeWidth),
    Stroke(CanvasStrokeStyle),
    FontSize(f32),
    Rendering(CanvasStrokeRendering),
    Curve(CanvasConnectorCurve),
    Fill,
    Round,
    Bidirectional,
    Background,
    Grid,
    Lock,
    Zoom(f32),
    Fit,
    ToggleList,
    ToggleExplanation,
}

fn resolve_canvas_selection(
    requested: Option<&str>,
    previous: Option<String>,
    drawings: &[CompanionDrawingSummary],
) -> Result<Option<String>, String> {
    if let Some(requested) = requested {
        return drawings
            .iter()
            .find(|drawing| drawing.id == requested)
            .map(|drawing| Some(drawing.id.clone()))
            .ok_or_else(|| "The requested drawing is no longer available.".into());
    }
    Ok(previous
        .filter(|id| drawings.iter().any(|drawing| &drawing.id == id))
        .or_else(|| drawings.first().map(|drawing| drawing.id.clone())))
}

impl AppModel {
    pub(crate) fn canvas_choice_id(&self, field: &str) -> String {
        format!(
            "canvas-semantic-{field}:{}:{}",
            self.canvas.active.as_ref().map_or("", |d| d.id.as_str()),
            self.canvas.editor.selected.as_deref().unwrap_or("new")
        )
    }

    pub(crate) fn canvas_choices(&self, id: &str) -> Vec<crate::select_menu::Choice> {
        use crate::select_menu::Choice;
        use ikore_core::companion_drawings::semantics::CanvasSemanticKindV1 as Semantic;
        use state::semantic_editing::{semantic_kinds, semantic_name};
        let Some(field) = id
            .strip_prefix("canvas-semantic-")
            .and_then(|s| s.split(':').next())
        else {
            return vec![];
        };
        if id != self.canvas_choice_id(field) || self.canvas.busy || self.canvas_project_switching {
            return vec![];
        }
        let editor = &self.canvas.editor;
        let Some(document) = &editor.scene.document else {
            return vec![];
        };
        let selected = editor.selected_semantic();
        if matches!(field, "create" | "type") {
            let current = match (field, selected) {
                ("type", Some(selected)) => selected.kind,
                ("type", None) => return vec![],
                _ => editor.creation_kind,
            };
            return semantic_kinds(document.profile)
                .iter()
                .enumerate()
                .map(|(index, kind)| {
                    Choice::new(
                        index,
                        semantic_name(*kind),
                        *kind == current
                            && (field == "type"
                                || (editor.selected.is_none()
                                    && editor.tool
                                        == state::semantic_editing::semantic_tool(*kind))),
                    )
                })
                .collect();
        }
        let current = match field {
            "participant" => selected
                .and_then(|e| e.participant_id.as_deref())
                .or_else(|| {
                    selected
                        .is_none()
                        .then_some(editor.participant_id.as_deref())
                        .flatten()
                }),
            "lane" => selected.and_then(|e| e.lane_id.as_deref()).or_else(|| {
                selected
                    .is_none()
                    .then_some(editor.lane_id.as_deref())
                    .flatten()
            }),
            "source" => selected.and_then(|e| e.source_id.as_deref()).or_else(|| {
                selected
                    .is_none()
                    .then_some(editor.semantic_source.as_deref())
                    .flatten()
            }),
            "target" => selected.and_then(|e| e.target_id.as_deref()).or_else(|| {
                selected
                    .is_none()
                    .then_some(editor.semantic_target.as_deref())
                    .flatten()
            }),
            _ => return vec![],
        };
        let mut values = if matches!(field, "participant" | "lane") {
            vec![Choice::new("", "None", current.is_none())]
        } else {
            vec![]
        };
        let participant = selected
            .and_then(|e| e.participant_id.as_deref())
            .or(editor.participant_id.as_deref());
        for entry in &document.elements {
            if match field {
                "participant" => entry.kind != Semantic::Participant,
                "lane" => {
                    entry.kind != Semantic::Lane || entry.participant_id.as_deref() != participant
                }
                _ => matches!(
                    entry.kind,
                    Semantic::Relationship
                        | Semantic::SequenceFlow
                        | Semantic::MessageFlow
                        | Semantic::Association
                ),
            } {
                continue;
            }
            let label = editor
                .scene
                .elements
                .iter()
                .find(|e| e.id == entry.element_id)
                .map(|e| e.label.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(semantic_name(entry.kind));
            values.push(Choice::new(
                &entry.element_id,
                format!(
                    "{label} · {}",
                    &entry.element_id[..entry.element_id.len().min(8)]
                ),
                current == Some(entry.element_id.as_str()),
            ));
        }
        values
    }

    pub(crate) fn apply_canvas_choice(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        use state::semantic_editing::semantic_kinds;
        let Some(field) = id
            .strip_prefix("canvas-semantic-")
            .and_then(|s| s.split(':').next())
        else {
            return;
        };
        if id != self.canvas_choice_id(field) || self.canvas.busy || self.canvas_project_switching {
            return;
        }
        self.commit_canvas_label(cx);
        if self.canvas.editing_label.is_some() {
            return;
        }
        let editor = &mut self.canvas.editor;
        let changed = match field {
            "create" | "type" => {
                let Some(kind) = editor
                    .scene
                    .document
                    .as_ref()
                    .and_then(|d| {
                        value
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| semantic_kinds(d.profile).get(i))
                    })
                    .copied()
                else {
                    return;
                };
                if field == "create" {
                    editor.set_creation_kind(kind);
                    false
                } else {
                    editor.set_selected_kind(kind)
                }
            }
            "participant" | "lane" => {
                let selected = editor.selected_semantic();
                let participant = selected
                    .map(|e| e.participant_id.clone())
                    .unwrap_or_else(|| editor.participant_id.clone());
                let chosen = (!value.is_empty()).then(|| value.to_owned());
                if field == "participant" {
                    editor.set_membership(chosen, None)
                } else {
                    editor.set_membership(participant, chosen)
                }
            }
            "source" | "target" => editor.set_endpoint(field == "source", value),
            _ => false,
        };
        if changed && self.canvas.editor.dirty {
            self.schedule_canvas_save(cx);
        }
        cx.notify();
    }

    /// Only an explicit developer action opens a published explanation.
    pub(crate) fn open_authored_canvas(
        &mut self,
        project_id: String,
        drawing_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.canvas_project(cx).as_deref() != Some(project_id.as_str())
            || self.canvas_project_switching
        {
            self.set_status_message("Return to the drawing's project before opening Canvas.");
            cx.notify();
            return;
        }
        if self
            .canvas
            .project_id
            .as_deref()
            .is_some_and(|owner| owner != project_id)
            && self.canvas.has_pending()
        {
            self.set_status_message("Save the current drawing before opening another project.");
            cx.notify();
            return;
        }
        self.select_view(crate::AppView::Canvas, cx);
        self.canvas.pending_action = Some(CanvasAction::Open(drawing_id));
        self.resume_canvas_pending_action(cx);
    }

    fn resume_canvas_pending_action(&mut self, cx: &mut Context<Self>) {
        if self.canvas.busy || self.canvas.saving || self.canvas.error.is_some() {
            return;
        }
        if let Some(action) = self.canvas.pending_action.take() {
            self.canvas_action(action, cx);
        }
    }
    pub(crate) fn observe_canvas_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.canvas.focus_observed {
            return;
        }
        self.canvas.focus_observed = true;
        let focus = self.canvas.focus.clone();
        let label = self.canvas.label.read(cx).focus_handle(cx);
        let surface_subscription = cx.on_blur(&focus, window, |model, _, cx| {
            model.canvas.editor.cancel();
            cx.notify();
        });
        let label_subscription =
            cx.on_blur(&label, window, |model, _, cx| model.commit_canvas_label(cx));
        self.canvas
            .subscriptions
            .extend([surface_subscription, label_subscription]);
    }

    fn canvas_project(&self, cx: &Context<Self>) -> Option<String> {
        self.workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
    }

    pub(crate) fn canvas_has_pending(&self) -> bool {
        self.canvas.has_pending()
    }

    /// The caller keeps its requested navigation until `complete_canvas_transition` is notified.
    pub(crate) fn canvas_can_leave(&mut self, cx: &mut Context<Self>) -> bool {
        self.commit_canvas_label(cx);
        if self.canvas.renaming {
            self.commit_canvas_title(cx);
        }
        if self.canvas.editor.gesturing() {
            self.canvas.editor.cancel();
        }
        if self.canvas.editor.dirty {
            self.flush_canvas(cx);
        }
        !self.canvas.has_pending()
    }

    pub(crate) fn discard_canvas_changes(&mut self, cx: &mut Context<Self>) {
        if self.canvas.saving || self.canvas.busy {
            return;
        }
        let active = self.canvas.active.clone();
        let id = active.as_ref().map(|d| d.id.clone());
        self.canvas.set_active(active, cx);
        self.canvas.error = None;
        self.canvas.pending_action = None;
        if let Some(id) = id {
            self.run_canvas_operation(CanvasAction::Open(id), cx);
        } else {
            self.complete_canvas_transition(cx);
        }
        cx.notify();
    }

    pub(crate) fn refresh_canvas(&mut self, cx: &mut Context<Self>) {
        if self.canvas_project_switching {
            return;
        }
        let project = self.canvas_project(cx);
        if project != self.canvas.project_id {
            // A dirty editor is owned by its original project until its commit completes.
            if self.canvas.has_pending() {
                return;
            }
            self.canvas.generation = self.canvas.generation.saturating_add(1);
            self.canvas.sync_task.take();
            self.canvas.syncing = false;
            self.canvas.sync_requested = false;
            self.canvas.project_id = project.clone();
            self.canvas.drawings.clear();
            self.canvas.pending_action = None;
            self.canvas.set_active(None, cx);
        }
        if project.is_some() && !self.canvas.busy && !self.canvas.saving {
            self.run_canvas_operation(CanvasAction::Refresh, cx);
        }
    }

    pub(crate) fn canvas_action(&mut self, action: CanvasAction, cx: &mut Context<Self>) {
        if self.canvas_project_switching {
            return;
        }
        match action {
            CanvasAction::Refresh => self.refresh_canvas(cx),
            CanvasAction::Save => {
                self.commit_canvas_label(cx);
                if self.canvas.editing_label.is_some() {
                    return;
                }
                if self.canvas.renaming {
                    self.commit_canvas_title(cx);
                }
                self.flush_canvas(cx);
            }
            CanvasAction::Discard => self.discard_canvas_changes(cx),
            CanvasAction::New
            | CanvasAction::Open(_)
            | CanvasAction::Duplicate
            | CanvasAction::ExportCopy
            | CanvasAction::ConfirmDelete => {
                if self.canvas.busy {
                    return;
                }
                self.commit_canvas_label(cx);
                if self.canvas.editing_label.is_some() {
                    return;
                }
                if self.canvas.renaming {
                    self.commit_canvas_title(cx);
                }
                if self.canvas.saving || self.canvas.editor.dirty {
                    self.canvas.pending_action = Some(action);
                    self.flush_canvas(cx);
                } else {
                    self.run_canvas_operation(action, cx);
                }
            }
            CanvasAction::RetryFiles => {
                if let Some(project) = self.canvas.project_id.clone() {
                    self.sync_canvas_files(project, cx);
                }
            }
            CanvasAction::BeginRename => {
                if self.canvas.active.is_some() {
                    self.canvas.renaming = true;
                    cx.notify();
                }
            }
            CanvasAction::Rename => {
                self.commit_canvas_title(cx);
                self.flush_canvas(cx);
            }
            CanvasAction::Delete => {
                self.canvas.confirm_delete = !self.canvas.confirm_delete;
                cx.notify();
            }
            CanvasAction::Tool(tool) => {
                self.commit_canvas_label(cx);
                if self.canvas.editing_label.is_some() {
                    return;
                }
                self.canvas.editor.cancel();
                self.canvas.editor.tool = tool;
                cx.notify();
            }
            CanvasAction::Zoom(factor) => {
                let bounds = self.canvas.bounds.get();
                let centre = [
                    f32::from(bounds.size.width) / 2.,
                    f32::from(bounds.size.height) / 2.,
                ];
                let zoom = self.canvas.editor.viewport.zoom * factor;
                geometry::zoom_at(&mut self.canvas.editor.viewport, centre, zoom);
                self.save_canvas_viewport(cx);
                cx.notify();
            }
            CanvasAction::Fit => {
                self.fit_canvas(cx);
            }
            CanvasAction::ToggleList => {
                self.canvas.list_collapsed = !self.canvas.list_collapsed;
                cx.notify();
            }
            CanvasAction::ToggleExplanation => {
                self.canvas.show_explanation = !self.canvas.show_explanation;
                cx.notify();
            }
            CanvasAction::Lock => {
                self.canvas.editor.locked = !self.canvas.editor.locked;
                cx.notify();
            }
            other => {
                if self.canvas.active.is_none() || self.canvas.busy {
                    return;
                }
                self.commit_canvas_label(cx);
                if self.canvas.editing_label.is_some() {
                    return;
                }
                let changed = match other {
                    CanvasAction::Undo => self.canvas.editor.undo(),
                    CanvasAction::Redo => self.canvas.editor.redo(),
                    CanvasAction::Remove => self.canvas.editor.delete_selection(),
                    CanvasAction::Background => self.canvas.editor.mutate(|scene, _| {
                        scene.background_mode = match scene.background_mode {
                            CanvasBackgroundMode::Dark => CanvasBackgroundMode::Light,
                            CanvasBackgroundMode::Light => CanvasBackgroundMode::Dark,
                        }
                    }),
                    CanvasAction::Grid => self
                        .canvas
                        .editor
                        .mutate(|scene, _| scene.show_grid = !scene.show_grid),
                    CanvasAction::Color(color) => {
                        self.canvas.editor.style.color = color;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                            {
                                element.stroke_color = color;
                            }
                        })
                    }
                    CanvasAction::Width(width) => {
                        self.canvas.editor.style.width = width;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                            {
                                element.stroke_width = width;
                            }
                        })
                    }
                    CanvasAction::Stroke(stroke) => {
                        self.canvas.editor.style.stroke = stroke;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                            {
                                element.stroke_style = stroke;
                            }
                        })
                    }
                    CanvasAction::Rendering(rendering) => {
                        self.canvas.editor.style.rendering = rendering;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                            {
                                element.stroke_rendering = rendering;
                            }
                        })
                    }
                    CanvasAction::Curve(curve) => {
                        self.canvas.editor.style.curve = curve;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                                && element.shape.is_connector()
                            {
                                element.connector_curve = curve;
                            }
                        })
                    }
                    CanvasAction::FontSize(size) => {
                        if !size.is_finite() || !(8.0..=96.0).contains(&size) {
                            return;
                        }
                        self.canvas.editor.style.font_size = size;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                            {
                                element.label_font_size = Some(size);
                                if let CanvasElementKindV1::Text { font_size } = &mut element.shape
                                {
                                    *font_size = size;
                                    let bounds = geometry::label_bounds(element);
                                    element.width = bounds.width.max(16.);
                                    element.height = bounds.height.max(24.);
                                }
                            }
                        })
                    }
                    CanvasAction::Fill => {
                        self.canvas.editor.style.fill = !self.canvas.editor.style.fill;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                            {
                                element.fill_enabled = !element.fill_enabled;
                            }
                        })
                    }
                    CanvasAction::Round => {
                        self.canvas.editor.style.round = !self.canvas.editor.style.round;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                                && let CanvasElementKindV1::Rectangle { round } = &mut element.shape
                            {
                                *round = !*round;
                            }
                        })
                    }
                    CanvasAction::Bidirectional => {
                        self.canvas.editor.style.bidirectional =
                            !self.canvas.editor.style.bidirectional;
                        self.canvas.editor.mutate(|scene, id| {
                            if let Some(element) = scene
                                .elements
                                .iter_mut()
                                .find(|e| Some(&e.id) == id.as_ref())
                                && let CanvasElementKindV1::Arrow { bidirectional, .. } =
                                    &mut element.shape
                            {
                                *bidirectional = !*bidirectional;
                            }
                        })
                    }
                    _ => false,
                };
                if changed {
                    self.schedule_canvas_save(cx);
                }
                cx.notify();
            }
        }
    }

    fn commit_canvas_title(&mut self, cx: &mut Context<Self>) {
        if !self.canvas.renaming {
            return;
        }
        self.canvas.renaming = false;
        let title = self.canvas.title.read(cx).value().trim().to_owned();
        if let Some(active) = &self.canvas.active
            && self.canvas.draft_title.as_deref().unwrap_or(&active.title) != title
        {
            self.canvas.draft_title = (active.title != title).then_some(title);
            self.canvas.editor.dirty = true;
            self.canvas.editor.change = self.canvas.editor.change.saturating_add(1);
        }
    }

    fn run_canvas_operation(&mut self, action: CanvasAction, cx: &mut Context<Self>) {
        let Some(core) = self.core.clone() else {
            return;
        };
        let Some(project) = self.canvas.project_id.clone() else {
            return;
        };
        if self.canvas.busy || self.canvas.saving {
            return;
        }
        let active = self.canvas.active.clone();
        let selected = active.as_ref().map(|d| d.id.clone());
        let generation = self.canvas.generation;
        let replace_document = !matches!(&action, CanvasAction::Refresh);
        self.canvas.busy = true;
        self.canvas.error = None;
        let runtime = core.runtime_handle().clone();
        self.canvas.operation_task = Some(cx.spawn(async move |this, cx| {
            let request_project = project.clone();
            let result = runtime
                .spawn_blocking(move || -> Result<_, String> {
                    let service = core.drawings();
                    let mut selection = selected;
                    let requested = match &action {
                        CanvasAction::Open(id) => Some(id.clone()),
                        _ => None,
                    };
                    match action {
                        CanvasAction::New => {
                            selection = Some(
                                service
                                    .create(&request_project, "Untitled drawing")
                                    .map_err(|e| e.to_string())?
                                    .id,
                            )
                        }
                        CanvasAction::Open(id) => selection = Some(id),
                        CanvasAction::Duplicate | CanvasAction::ExportCopy => {
                            if let Some(active) = active {
                                selection = Some(
                                    service
                                        .duplicate(&request_project, &active.id)
                                        .map_err(|e| e.to_string())?
                                        .id,
                                );
                            }
                        }
                        CanvasAction::ConfirmDelete => {
                            if let Some(active) = active {
                                service
                                    .delete(&request_project, &active.id, active.revision)
                                    .map_err(|e| e.to_string())?;
                                selection = None;
                            }
                        }
                        _ => {}
                    }
                    let drawings = service
                        .list_summaries(&request_project)
                        .map_err(|e| e.to_string())?;
                    let selected =
                        resolve_canvas_selection(requested.as_deref(), selection, &drawings)?;
                    let drawing = selected
                        .map(|id| {
                            service
                                .get(&request_project, &id)
                                .map_err(|e| e.to_string())
                        })
                        .transpose()?;
                    Ok((drawings, drawing))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = this.update(cx, |model, cx| {
                model.finish_canvas_operation(generation, &project, replace_document, result, cx);
            });
        }));
        cx.notify();
    }

    fn finish_canvas_operation(
        &mut self,
        generation: u64,
        project: &str,
        replace_document: bool,
        result: Result<(Vec<CompanionDrawingSummary>, Option<CompanionDrawing>), String>,
        cx: &mut Context<Self>,
    ) {
        if self.canvas.generation != generation
            || self.canvas.project_id.as_deref() != Some(project)
        {
            return;
        }
        self.canvas.busy = false;
        self.canvas.operation_task.take();
        match result {
            Ok((drawings, drawing)) => {
                self.canvas.library_epoch = self.canvas.library_epoch.saturating_add(1);
                let same =
                    self.canvas.active.as_ref().map(|d| &d.id) == drawing.as_ref().map(|d| &d.id);
                self.canvas.drawings = drawings;
                if !same || self.canvas.active.is_none() || replace_document {
                    self.canvas.set_active(drawing, cx);
                } else if !self.canvas.editor.dirty
                    && self.canvas.editing_label.is_none()
                    && let Some(drawing) = drawing
                {
                    if self
                        .canvas
                        .active
                        .as_ref()
                        .is_some_and(|active| active.revision != drawing.revision)
                    {
                        let viewport = self.canvas.editor.viewport;
                        self.canvas.editor.load(drawing.scene.clone(), viewport);
                    }
                    self.canvas.active = Some(drawing);
                }
                self.sync_canvas_files(project.to_owned(), cx);
                // An autosave timer may have elapsed while this refresh owned the backend.
                // Retain the dirty scene and arm a fresh timer once the operation releases it.
                if self.canvas.editor.dirty {
                    self.schedule_canvas_save(cx);
                }
                self.resume_canvas_pending_action(cx);
            }
            Err(error) => {
                self.canvas.error = Some(error);
                self.canvas_close_requested = false;
            }
        }
        self.complete_canvas_transition(cx);
        cx.notify();
    }

    fn schedule_canvas_save(&mut self, cx: &mut Context<Self>) {
        self.canvas.autosave_generation = self.canvas.autosave_generation.saturating_add(1);
        let generation = self.canvas.autosave_generation;
        self.canvas.autosave_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            let _ = this.update(cx, |model, cx| {
                if model.canvas.autosave_generation == generation {
                    model.flush_canvas(cx);
                }
            });
        }));
    }

    pub(crate) fn flush_canvas(&mut self, cx: &mut Context<Self>) {
        if self.canvas.saving || self.canvas.busy || !self.canvas.editor.dirty {
            return;
        }
        if self.canvas.editor.gesturing() {
            self.schedule_canvas_save(cx);
            return;
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        let Some(mut active) = self.canvas.active.clone() else {
            return;
        };
        if let Some(title) = &self.canvas.draft_title {
            active.title = title.clone();
        }
        self.canvas.editor.cancel();
        self.canvas.autosave_generation = self.canvas.autosave_generation.saturating_add(1);
        self.canvas.autosave_task.take();
        let generation = self.canvas.generation;
        let change = self.canvas.editor.change;
        let scene = self.canvas.editor.scene.clone();
        let viewport = self.canvas.editor.viewport;
        self.canvas.saving = true;
        self.canvas.error = None;
        let runtime = core.runtime_handle().clone();
        self.canvas.save_task = Some(cx.spawn(async move |this, cx| {
            let request = active.clone();
            let result = runtime
                .spawn_blocking(move || {
                    core.drawings()
                        .save(
                            &request.project_id,
                            &request.id,
                            request.revision,
                            &request.title,
                            &scene,
                            &viewport,
                        )
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = this.update(cx, |model, cx| {
                if model.canvas.generation != generation
                    || model.canvas.project_id.as_ref() != Some(&active.project_id)
                    || model.canvas.active.as_ref().map(|d| &d.id) != Some(&active.id)
                {
                    return;
                }
                model.canvas.saving = false;
                model.canvas.save_task.take();
                match result {
                    Ok(drawing) => {
                        let project = drawing.project_id.clone();
                        model.canvas.acknowledge_save(drawing, change, cx);
                        model.sync_canvas_files(project, cx);
                        if model.canvas.editor.dirty {
                            model.flush_canvas(cx);
                        } else if let Some(action) = model.canvas.pending_action.take() {
                            model.run_canvas_operation(action, cx);
                        }
                        model.complete_canvas_transition(cx);
                    }
                    Err(error) => {
                        model.canvas.error = Some(error);
                        model.canvas_close_requested = false;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn sync_canvas_files(&mut self, project: String, cx: &mut Context<Self>) {
        if self.canvas.syncing {
            self.canvas.sync_requested = true;
            return;
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        self.canvas.syncing = true;
        let generation = self.canvas.generation;
        let library_epoch = self.canvas.library_epoch;
        let runtime = core.runtime_handle().clone();
        self.canvas.sync_task=Some(cx.spawn(async move |this,cx| {
            let request_project=project.clone();
            let result=runtime.spawn_blocking(move || -> Result<_,String> {
                let service=core.drawings();
                let report=service.sync_project(&request_project).map_err(|e|e.to_string())?;
                let drawings=service.list_summaries(&request_project).map_err(|e|e.to_string())?;
                Ok((report,drawings))
            }).await.map_err(|e|e.to_string()).and_then(|r|r);
            let _=this.update(cx,|model,cx| {
                if model.canvas.generation!=generation || model.canvas.project_id.as_ref()!=Some(&project) { return; }
                model.canvas.syncing=false; model.canvas.sync_task.take();
                let sync_again=std::mem::take(&mut model.canvas.sync_requested);
                match result {
                    Ok((report,drawings))=>{
                        model.canvas.apply_sync_snapshot(library_epoch,drawings);
                        if report.conflicts>0 { model.canvas.notice=Some("A drawing file changed outside HVC. Preserve it or create an export copy.".into()); }
                    }
                    Err(_)=>model.canvas.notice=Some("Saved in the app. Drawing files are pending; retry when the workspace is available.".into()),
                }
                if sync_again { model.sync_canvas_files(project.clone(),cx); }
                cx.notify();
            });
        }));
    }

    fn save_canvas_viewport(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.canvas.active.clone() else {
            return;
        };
        let Some(core) = self.core.clone() else {
            return;
        };
        let viewport = self.canvas.editor.viewport;
        let runtime = core.runtime_handle().clone();
        self.canvas.viewport_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = runtime
                .spawn_blocking(move || {
                    core.drawings()
                        .save_viewport(&active.project_id, &active.id, &viewport)
                })
                .await;
        }));
    }

    pub(crate) fn fit_canvas(&mut self, cx: &mut Context<Self>) {
        let bounds = geometry::scene_bounds(&self.canvas.editor.scene);
        let panel = self.canvas.bounds.get();
        let w = f32::from(panel.size.width).max(200.);
        let h = f32::from(panel.size.height).max(200.);
        let zoom = ((w - 80.) / bounds.width.max(1.))
            .min((h - 80.) / bounds.height.max(1.))
            .clamp(0.1, 4.);
        self.canvas.editor.viewport.zoom = zoom;
        self.canvas.editor.viewport.pan_x = w / 2. - (bounds.x + bounds.width / 2.) * zoom;
        self.canvas.editor.viewport.pan_y = h / 2. - (bounds.y + bounds.height / 2.) * zoom;
        self.save_canvas_viewport(cx);
        cx.notify();
    }

    fn canvas_screen_point(&self, position: Point<Pixels>) -> [f32; 2] {
        let bounds = self.canvas.bounds.get();
        [
            f32::from(position.x - bounds.left()),
            f32::from(position.y - bounds.top()),
        ]
    }

    pub(crate) fn canvas_press(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.canvas_project_switching || self.canvas.busy || self.canvas.active.is_none() {
            return;
        }
        self.commit_canvas_label(cx);
        if self.canvas.editing_label.is_some() {
            self.canvas.label.read(cx).focus_handle(cx).focus(window);
            cx.stop_propagation();
            return;
        }
        self.canvas.focus.focus(window);
        let point = self.canvas_screen_point(event.position);
        if event.click_count == 2 && self.canvas.editor.tool == Tool::Select {
            let world = geometry::screen_to_world(&self.canvas.editor.viewport, point);
            if let Some(id) = geometry::hit_test(
                &self.canvas.editor.scene,
                world,
                8. / self.canvas.editor.viewport.zoom,
            ) {
                self.start_canvas_label(id, window, cx);
                return;
            }
        }
        self.canvas
            .editor
            .press(point, event.button == gpui::MouseButton::Middle);
        cx.notify();
    }

    pub(crate) fn canvas_drag(&mut self, event: &gpui::MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.canvas.editor.gesturing() || self.canvas_project_switching {
            return;
        }
        if event.pressed_button.is_none() {
            self.canvas.editor.cancel();
            cx.notify();
            return;
        }
        let point = self.canvas_screen_point(event.position);
        self.canvas.editor.drag(point);
        cx.notify();
    }

    pub(crate) fn canvas_release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.canvas.editor.gesturing() {
            return;
        }
        let (changed, text) = self.canvas.editor.release();
        if changed {
            self.schedule_canvas_save(cx);
        } else {
            self.save_canvas_viewport(cx);
        }
        if let Some(id) = text {
            self.start_canvas_label(id, window, cx);
        }
        cx.notify();
    }

    pub(crate) fn canvas_scroll(
        &mut self,
        event: &gpui::ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.canvas_project_switching || self.canvas.active.is_none() {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        if event.modifiers.control {
            let point = self.canvas_screen_point(event.position);
            let zoom = self.canvas.editor.viewport.zoom * (f32::from(delta.y) * 0.002).exp();
            geometry::zoom_at(&mut self.canvas.editor.viewport, point, zoom);
        } else {
            self.canvas.editor.viewport.pan_x += f32::from(delta.x);
            self.canvas.editor.viewport.pan_y += f32::from(delta.y);
        }
        self.save_canvas_viewport(cx);
        cx.notify();
        cx.stop_propagation();
    }

    pub(crate) fn start_canvas_label(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.canvas.editing_label.is_some() {
            self.commit_canvas_label(cx);
            if self.canvas.editing_label.is_some() {
                self.canvas.label.read(cx).focus_handle(cx).focus(window);
                return;
            }
        }
        if let Some(element) = self
            .canvas
            .editor
            .scene
            .elements
            .iter()
            .find(|e| e.id == id)
        {
            let text = element.label.clone();
            self.canvas.editor.selected = Some(id.clone());
            self.canvas.editing_label = Some(id.clone());
            self.canvas.label_focus_generation =
                self.canvas.label_focus_generation.saturating_add(1);
            let focus_generation = self.canvas.label_focus_generation;
            let generation = self.canvas.generation;
            let focused_before_mount = window.focused(cx);
            self.canvas.label.update(cx, |input, cx| {
                input.reset_plain_text(text, cx);
            });
            let model = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                let _ = model.update(cx, |model, cx| {
                    if model.canvas.generation != generation
                        || model.canvas.label_focus_generation != focus_generation
                        || model.canvas.editing_label.as_ref() != Some(&id)
                        || model.active_view() != crate::AppView::Canvas
                        || model.settings_modal.open
                        || model.canvas_project_switching
                        || window.focused(cx) != focused_before_mount
                    {
                        return;
                    }
                    // Finish the current pointer dispatch before choosing the editor focus.
                    // The next paint installs its native input handler with that focus.
                    model.canvas.label.read(cx).focus_handle(cx).focus(window);
                    cx.notify();
                });
            });
            cx.notify();
        }
    }

    pub(crate) fn commit_canvas_label(&mut self, cx: &mut Context<Self>) {
        if self.canvas.label.read(cx).has_marked_text() {
            return;
        }
        let Some(id) = self.canvas.editing_label.take() else {
            return;
        };
        let text = self.canvas.label.read(cx).buffer.text().to_owned();
        if text.chars().count() > ikore_core::companion_drawings::MAX_LABEL_CHARS {
            self.canvas.editing_label = Some(id);
            self.canvas.error = Some("A label can contain at most 4000 characters.".into());
            return;
        }
        let changed = self.canvas.editor.mutate(|scene, _| {
            if let Some(element) = scene.elements.iter_mut().find(|e| e.id == id) {
                element.label = text;
                if matches!(element.shape, CanvasElementKindV1::Text { .. }) {
                    let bounds = geometry::label_bounds(element);
                    element.width = bounds.width.max(16.);
                    element.height = bounds.height.max(24.);
                }
            }
        });
        if changed {
            self.schedule_canvas_save(cx);
        }
        cx.notify();
    }

    pub(crate) fn apply_canvas_label(
        &mut self,
        _: &ApplyLabel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.canvas.label.read(cx).has_marked_text() {
            cx.stop_propagation();
            return;
        }
        self.commit_canvas_label(cx);
        if self.canvas.editing_label.is_none() {
            self.canvas.focus.focus(window);
        }
        cx.stop_propagation();
    }
    pub(crate) fn newline_canvas_label(
        &mut self,
        _: &NewlineLabel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.canvas
            .label
            .update(cx, |label, cx| label.insert_newline(cx));
        cx.stop_propagation();
    }
    pub(crate) fn cancel_canvas_label(
        &mut self,
        _: &CancelLabel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.canvas.editing_label = None;
        self.canvas.label_focus_generation = self.canvas.label_focus_generation.saturating_add(1);
        self.canvas.focus.focus(window);
        cx.stop_propagation();
        cx.notify();
    }
    pub(crate) fn save_canvas_label(
        &mut self,
        _: &SaveLabel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_canvas_label(cx);
        self.flush_canvas(cx);
        if self.canvas.editing_label.is_none() {
            self.canvas.focus.focus(window);
        }
        cx.stop_propagation();
    }

    pub(crate) fn canvas_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.canvas.focus.is_focused(window) || self.canvas_project_switching {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let action = if modifiers.control {
            match key {
                "s" => Some(CanvasAction::Save),
                "z" if modifiers.shift => Some(CanvasAction::Redo),
                "z" => Some(CanvasAction::Undo),
                "y" => Some(CanvasAction::Redo),
                _ => None,
            }
        } else if modifiers.alt || modifiers.platform {
            None
        } else {
            match key {
                "v" => Some(CanvasAction::Tool(Tool::Select)),
                "h" => Some(CanvasAction::Tool(Tool::Hand)),
                "r" => Some(CanvasAction::Tool(Tool::Rectangle)),
                "d" => Some(CanvasAction::Tool(Tool::Diamond)),
                "o" => Some(CanvasAction::Tool(Tool::Ellipse)),
                "a" => Some(CanvasAction::Tool(Tool::Arrow)),
                "l" => Some(CanvasAction::Tool(Tool::Line)),
                "t" => Some(CanvasAction::Tool(Tool::Text)),
                "e" => Some(CanvasAction::Tool(Tool::Eraser)),
                "q" => Some(CanvasAction::Lock),
                "delete" | "backspace" => Some(CanvasAction::Remove),
                "space" => {
                    self.canvas.editor.space_pan = true;
                    None
                }
                "escape" => {
                    self.canvas.editor.cancel();
                    self.canvas.confirm_delete = false;
                    None
                }
                "enter" => {
                    if let Some(id) = self.canvas.editor.selected.clone() {
                        self.start_canvas_label(id, window, cx);
                    }
                    None
                }
                _ => None,
            }
        };
        if let Some(action) = action {
            self.canvas_action(action, cx);
            cx.stop_propagation();
        } else if matches!(key, "space" | "escape" | "enter") {
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(crate) fn canvas_key_up(&mut self, event: &gpui::KeyUpEvent, cx: &mut Context<Self>) {
        if event.keystroke.key == "space" {
            self.canvas.editor.space_pan = false;
            cx.notify();
        }
    }
}

#[cfg(test)]
mod async_regression_tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use ikore_core::companion_drawings::{CanvasElementV1, CanvasSceneV1, DrawingSyncStatus};

    fn document() -> CompanionDrawing {
        CompanionDrawing {
            id: "drawing-callback-fixture".into(),
            project_id: "layout-project".into(),
            title: "Saved title".into(),
            scene: CanvasSceneV1::default(),
            viewport: Default::default(),
            revision: 1,
            basename: "callback-fixture".into(),
            created_at: String::new(),
            updated_at: String::new(),
            sync_status: DrawingSyncStatus::Synced,
        }
    }

    fn install_label_window(model: &Entity<AppModel>, visual: &mut VisualTestContext) -> String {
        let id = model.update(visual, |model, cx| {
            model.select_view(crate::AppView::Canvas, cx);
            let mut saved = document();
            let mut shape = CanvasElementV1::new(
                CanvasElementKindV1::Rectangle { round: false },
                50.,
                50.,
                160.,
                90.,
            );
            shape.label = "Initial label".into();
            let id = shape.id.clone();
            saved.scene.elements.push(shape);
            model.canvas.project_id = Some(saved.project_id.clone());
            model.canvas.set_active(Some(saved), cx);
            cx.notify();
            id
        });
        visual.run_until_parked();
        id
    }

    #[gpui::test]
    fn canvas_label_editor_receives_focus_after_mount_without_an_extra_click(
        cx: &mut TestAppContext,
    ) {
        let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
        visual.update(|_, cx| crate::native_editor::register_editor_keybindings(cx));
        let id = install_label_window(&model, visual);
        visual.update(|window, cx| {
            model.update(cx, |model, cx| {
                model.canvas.focus.focus(window);
                model.start_canvas_label(id, window, cx);
                assert!(
                    !model
                        .canvas
                        .label
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window)
                );
            })
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            assert!(
                model
                    .read(cx)
                    .canvas
                    .label
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
        });
        visual.simulate_keystrokes("ctrl-a");
        visual.simulate_input("Selected immediately");
        assert_eq!(
            model.read_with(visual, |model, cx| model
                .canvas
                .label
                .read(cx)
                .buffer
                .text()
                .to_owned()),
            "Selected immediately"
        );
    }

    #[gpui::test]
    fn canvas_cancelled_label_focus_request_does_not_steal_focus_after_dispatch(
        cx: &mut TestAppContext,
    ) {
        let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
        let id = install_label_window(&model, visual);
        visual.update(|window, cx| {
            model.update(cx, |model, cx| {
                model.canvas.focus.focus(window);
                model.start_canvas_label(id, window, cx);
                model.cancel_canvas_label(&CancelLabel, window, cx);
                model.canvas.search.read(cx).focus_handle(cx).focus(window);
            })
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            let model = model.read(cx);
            assert!(
                model
                    .canvas
                    .search
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            assert!(
                !model
                    .canvas
                    .label
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            assert!(model.canvas.editing_label.is_none());
        });
    }

    #[gpui::test]
    fn canvas_save_callback_keeps_a_gesture_started_after_dispatch(cx: &mut TestAppContext) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let mut saved = document();
            model.canvas.set_active(Some(saved.clone()), cx);
            model.canvas.editor.mutate(|scene, _| {
                scene.elements.push(CanvasElementV1::new(
                    CanvasElementKindV1::Rectangle { round: false },
                    0.,
                    0.,
                    100.,
                    100.,
                ));
            });
            saved.scene = model.canvas.editor.scene.clone();
            saved.revision = 2;
            let captured_change = model.canvas.editor.change;
            model.canvas.editor.tool = Tool::Rectangle;
            model.canvas.editor.press([200., 20.], false);
            model.canvas.editor.drag([320., 140.]);
            let preview = model.canvas.editor.scene.clone();
            assert_eq!(preview.elements.len(), 2);
            assert_eq!(model.canvas.editor.change, captured_change);

            model.canvas.acknowledge_save(saved, captured_change, cx);
            assert_eq!(model.canvas.editor.scene, preview);
            assert!(model.canvas.editor.gesturing());
            assert!(model.canvas.editor.dirty);
            assert_eq!(model.canvas.active.as_ref().unwrap().revision, 2);
            assert!(model.canvas.editor.release().0);
            assert_eq!(model.canvas.editor.scene.elements.len(), 2);
        });
    }

    #[gpui::test]
    fn canvas_save_callback_does_not_replace_an_unconfirmed_rename(cx: &mut TestAppContext) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let mut saved = document();
            model.canvas.set_active(Some(saved.clone()), cx);
            model
                .canvas
                .editor
                .mutate(|scene, _| scene.show_grid = false);
            saved.scene = model.canvas.editor.scene.clone();
            saved.revision = 2;
            let captured_change = model.canvas.editor.change;
            model.canvas.renaming = true;
            model.canvas.title.update(cx, |input, cx| {
                input.set_value("Title typed while the write was running", cx);
            });

            model.canvas.acknowledge_save(saved, captured_change, cx);
            assert_eq!(
                model.canvas.title.read(cx).value(),
                "Title typed while the write was running"
            );
            assert!(model.canvas.renaming);
            assert!(model.canvas.has_pending());
            assert!(!model.canvas.editor.dirty);
            model.commit_canvas_title(cx);
            assert!(model.canvas.editor.dirty);
            assert_eq!(
                model.canvas.draft_title.as_deref(),
                Some("Title typed while the write was running")
            );
        });
    }

    #[gpui::test]
    fn canvas_refresh_completion_rearms_a_dirty_autosave(cx: &mut TestAppContext) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let saved = document();
            model.canvas.project_id = Some(saved.project_id.clone());
            model.canvas.set_active(Some(saved.clone()), cx);
            model
                .canvas
                .editor
                .mutate(|scene, _| scene.show_grid = false);
            let draft = model.canvas.editor.scene.clone();
            model.canvas.busy = true;
            // Simulate the original timer already having elapsed during the refresh.
            model.canvas.autosave_task = None;

            model.finish_canvas_operation(
                model.canvas.generation,
                &saved.project_id,
                false,
                Ok((vec![saved.summary()], Some(saved.clone()))),
                cx,
            );
            assert!(!model.canvas.busy);
            assert_eq!(model.canvas.editor.scene, draft);
            assert!(model.canvas.editor.dirty);
            assert!(model.canvas.autosave_task.is_some());
        });
    }

    #[gpui::test]
    fn canvas_sync_callback_does_not_mark_a_newer_revision_as_saved(cx: &mut TestAppContext) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let mut older = document();
            older.sync_status = DrawingSyncStatus::Synced;
            let mut current = older.clone();
            current.revision = 2;
            current.title = "Current revision".into();
            current.sync_status = DrawingSyncStatus::Pending;
            model.canvas.set_active(Some(current.clone()), cx);
            model.canvas.drawings = vec![current.summary()];

            model
                .canvas
                .apply_sync_snapshot(model.canvas.library_epoch, vec![older.summary()]);
            let active = model.canvas.active.as_ref().unwrap();
            assert_eq!(active.revision, 2);
            assert_eq!(active.sync_status, DrawingSyncStatus::Pending);
            assert_eq!(model.canvas.drawings[0].revision, 2);
            assert_eq!(model.canvas.drawings[0].title, "Current revision");

            current.sync_status = DrawingSyncStatus::Synced;
            model
                .canvas
                .apply_sync_snapshot(model.canvas.library_epoch, vec![current.summary()]);
            assert_eq!(
                model.canvas.active.as_ref().unwrap().sync_status,
                DrawingSyncStatus::Synced
            );
        });
    }

    #[gpui::test]
    fn canvas_old_sync_snapshot_preserves_new_rows_and_does_not_resurrect_deleted_rows(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let existing = document();
            model.canvas.set_active(Some(existing.clone()), cx);
            let mut created = existing.clone();
            created.id = "created-after-sync-started".into();
            let mut deleted = existing.clone();
            deleted.id = "deleted-after-sync-started".into();
            let captured_epoch = model.canvas.library_epoch;
            model.canvas.library_epoch += 1;
            model.canvas.drawings = vec![existing.summary(), created.summary()];

            model
                .canvas
                .apply_sync_snapshot(captured_epoch, vec![existing.summary(), deleted.summary()]);
            assert_eq!(model.canvas.drawings.len(), 2);
            assert!(
                model
                    .canvas
                    .drawings
                    .iter()
                    .any(|drawing| drawing.id == created.id)
            );
            assert!(
                !model
                    .canvas
                    .drawings
                    .iter()
                    .any(|drawing| drawing.id == deleted.id)
            );
        });
    }

    #[gpui::test]
    fn canvas_save_reordering_elements_does_not_create_a_protection_notice(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let mut saved = document();
            model.canvas.set_active(Some(saved.clone()), cx);
            let mut core = CanvasElementV1::new(
                CanvasElementKindV1::Rectangle { round: false },
                0.,
                0.,
                100.,
                100.,
            );
            core.label = "Core".into();
            core.z_index = 1;
            let mut storage =
                CanvasElementV1::new(CanvasElementKindV1::Ellipse, 200., 0., 100., 100.);
            storage.label = "Storage".into();
            storage.z_index = 2;
            model
                .canvas
                .editor
                .mutate(|scene, _| scene.elements = vec![storage.clone(), core.clone()]);
            saved.scene = model.canvas.editor.scene.clone();
            saved.scene.elements.reverse();
            saved.revision = 2;
            assert_ne!(model.canvas.editor.scene, saved.scene);

            model
                .canvas
                .acknowledge_save(saved, model.canvas.editor.change, cx);
            assert!(!model.canvas.redaction_notice);
            assert!(model.canvas.notice.is_none());
            assert!(!model.canvas.editor.dirty);
        });
    }

    #[gpui::test]
    fn canvas_save_reports_real_title_or_label_redaction_without_file_actions(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(crate::layout_tests::fixture);
        for redact_title in [false, true] {
            model.update(cx, |model, cx| {
                let mut saved = document();
                model.canvas.set_active(Some(saved.clone()), cx);
                let mut shape = CanvasElementV1::new(
                    CanvasElementKindV1::Rectangle { round: false },
                    0.,
                    0.,
                    100.,
                    100.,
                );
                shape.label = if redact_title {
                    "Core"
                } else {
                    "TOKEN=fixture-only-sensitive-value"
                }
                .into();
                model
                    .canvas
                    .editor
                    .mutate(|scene, _| scene.elements.push(shape));
                saved.scene =
                    ikore_core::companion_drawings::normalize_scene(&model.canvas.editor.scene)
                        .expect("fixture scene is valid");
                if redact_title {
                    model.canvas.draft_title = Some("TOKEN=fixture-only-sensitive-title".into());
                    saved.title = ikore_core::companion_drawings::sanitize_title(
                        model.canvas.draft_title.as_deref().unwrap(),
                    )
                    .expect("fixture title is valid");
                }
                saved.revision = 2;

                model
                    .canvas
                    .acknowledge_save(saved, model.canvas.editor.change, cx);
                assert!(model.canvas.redaction_notice);
                assert!(model.canvas.notice.is_none());
                assert!(!model.canvas.editor.dirty);
            });
        }
    }
}
