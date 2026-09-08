use crate::{
    AppModel, AppView,
    text_input::{NativeTextInput, Submit},
};
use gpui::{AppContext, Context, Entity, Subscription};
use hvc_core::feature_forge::*;
use std::collections::HashSet;

/// Sections of the merged Forge view (Tasks/Backlog + Feature Forge, see the UI flow plan Part 1.1).
/// `Backlog` is UI-only and never persisted through `ForgeNavigation`; the other five map onto the
/// `0..=4` index the core still validates in `FeatureForgeService::save_navigation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForgeSection {
    Backlog,
    Incubation,
    Triage,
    Dependencies,
    Waves,
    Delivery,
}

impl ForgeSection {
    pub(crate) const ALL: [Self; 6] = [
        Self::Backlog,
        Self::Incubation,
        Self::Triage,
        Self::Dependencies,
        Self::Waves,
        Self::Delivery,
    ];

    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Incubation => "incubation",
            Self::Triage => "triage",
            Self::Dependencies => "dependencies",
            Self::Waves => "waves",
            Self::Delivery => "delivery",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Incubation => "Incubation",
            Self::Triage => "Triage",
            Self::Dependencies => "Dependencies",
            Self::Waves => "Waves",
            Self::Delivery => "Delivery",
        }
    }

    /// The core only knows about the five Feature Forge sections; `Backlog` has no equivalent and
    /// never round-trips through `ForgeNavigation`.
    pub(crate) const fn to_core_index(self) -> Option<usize> {
        match self {
            Self::Backlog => None,
            Self::Incubation => Some(0),
            Self::Triage => Some(1),
            Self::Dependencies => Some(2),
            Self::Waves => Some(3),
            Self::Delivery => Some(4),
        }
    }

    pub(crate) const fn from_core_index(index: usize) -> Self {
        match index {
            0 => Self::Incubation,
            1 => Self::Triage,
            2 => Self::Dependencies,
            3 => Self::Waves,
            _ => Self::Delivery,
        }
    }
}

// Pulsations still has a polling backend. Only actual task lifecycle changes
// invalidate Forge; repeated snapshots and token/stage updates do not.
pub(crate) fn task_runtime_changed(
    previous: &[hvc_core::persistence_service::Pulsation],
    next: &[hvc_core::persistence_service::Pulsation],
) -> bool {
    let terminal = |p: &hvc_core::persistence_service::Pulsation| {
        matches!(
            p.status,
            hvc_core::persistence_service::PulsationStatus::Done
                | hvc_core::persistence_service::PulsationStatus::Failed
        )
    };
    next.iter().any(|p| {
        terminal(p)
            && !previous.iter().any(|old| {
                old.id == p.id && old.project_id == p.project_id && old.status == p.status
            })
    })
}

pub(crate) struct DraftInputs {
    pub title: Entity<NativeTextInput>,
    pub description: Entity<NativeTextInput>,
    pub criteria: Entity<NativeTextInput>,
    pub kind: TaskKind,
    pub dependencies: Vec<String>,
}
impl DraftInputs {
    fn new(cx: &mut Context<AppModel>) -> Self {
        Self {
            title: cx.new(|cx| NativeTextInput::new("Task title", cx)),
            description: cx
                .new(|cx| NativeTextInput::new("Current behavior and expected outcome", cx)),
            criteria: cx.new(|cx| NativeTextInput::new("Acceptance criteria separated by ;", cx)),
            kind: TaskKind::Bug,
            dependencies: Vec::new(),
        }
    }
    fn draft(&self, cx: &Context<AppModel>) -> TaskDraft {
        TaskDraft {
            title: self.title.read(cx).value().into(),
            description: self.description.read(cx).value().into(),
            kind: self.kind,
            criteria: self
                .criteria
                .read(cx)
                .value()
                .split(';')
                .filter(|s| !s.trim().is_empty())
                .enumerate()
                .map(|(i, s)| AcceptanceCriterion {
                    id: format!("ac-{}", i + 1),
                    description: s.trim().into(),
                })
                .collect(),
            dependencies: self.dependencies.clone(),
            feature_id: None,
        }
    }
}
pub(crate) struct ForgeUi {
    pub snapshot: ForgeSnapshot,
    pub selected_feature: Option<String>,
    pub selected_tasks: HashSet<String>,
    pub history: Vec<FeatureTurn>,
    pub drafts: Vec<DraftInputs>,
    pub feature_title: Entity<NativeTextInput>,
    pub idea: Entity<NativeTextInput>,
    pub message: Entity<NativeTextInput>,
    _message_submit: Subscription,
    pub batch_title: Entity<NativeTextInput>,
    pub rationale: Entity<NativeTextInput>,
    pub wave_limit: Entity<NativeTextInput>,
    pub filters: Vec<Entity<NativeTextInput>>,
    pub impact: u8,
    pub confidence: u8,
    pub effort: u8,
    pub level: u8,
    pub new_open: bool,
    pub capture_feature: bool,
    pub busy: bool,
    refreshing: bool,
    refresh_pending: bool,
    pending_navigation: Option<ForgeNavigation>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub section: ForgeSection,
    generation: u64,
    pub project_id: Option<String>,
    pub editing_task: Option<(String, u64)>,
    pub source_task: Option<(String, u64)>,
}
impl ForgeUi {
    pub fn new(cx: &mut Context<AppModel>) -> Self {
        let message = cx
            .new(|cx| NativeTextInput::new("Answer questions or refine your idea", cx).multiline());
        let message_submit = cx.subscribe(&message, |model, input, _: &Submit, cx| {
            if input == model.forge.message {
                model.forge_action(ForgeAction::Send, cx);
            }
        });
        Self {
            snapshot: ForgeSnapshot::default(),
            selected_feature: None,
            selected_tasks: HashSet::new(),
            history: Vec::new(),
            drafts: vec![DraftInputs::new(cx)],
            feature_title: cx.new(|cx| NativeTextInput::new("Feature title", cx)),
            idea: cx.new(|cx| NativeTextInput::new("What problem do you want to solve?", cx)),
            message,
            _message_submit: message_submit,
            batch_title: cx.new(|cx| NativeTextInput::new("Pulsation title", cx)),
            rationale: cx.new(|cx| NativeTextInput::new("Dependency or archiving rationale", cx)),
            wave_limit: cx.new(|cx| NativeTextInput::new("Current Wave capacity (default: 3)", cx)),
            filters: [
                "Value and need",
                "Product alignment",
                "Feasibility and risks",
                "Usability and adoption",
            ]
            .into_iter()
            .map(|s| cx.new(|cx| NativeTextInput::new(s, cx)))
            .collect(),
            impact: 3,
            confidence: 3,
            effort: 3,
            level: 0,
            new_open: false,
            capture_feature: false,
            busy: false,
            refreshing: false,
            refresh_pending: false,
            pending_navigation: None,
            error: None,
            notice: None,
            section: ForgeSection::Backlog,
            generation: 0,
            project_id: None,
            editing_task: None,
            source_task: None,
        }
    }
    pub fn feature(&self) -> Option<&Feature> {
        self.selected_feature
            .as_ref()
            .and_then(|id| self.snapshot.features.iter().find(|f| &f.id == id))
    }

    fn message_operation(
        &self,
        project: Option<&str>,
        cx: &Context<AppModel>,
    ) -> Option<Operation> {
        let feature = self.feature()?;
        let message = self.message.read(cx).value();
        if self.busy
            || project != self.project_id.as_deref()
            || project != Some(feature.project_id.as_str())
            || message.trim().is_empty()
            || feature.state == FeatureState::Archived
            || self
                .history
                .iter()
                .any(|turn| turn.feature_id == feature.id && turn.status == "running")
        {
            return None;
        }
        Some(Operation::Send(
            feature.id.clone(),
            feature.revision,
            message.into(),
        ))
    }

    fn clear_accepted_message(&self, feature: &str, message: &str, cx: &mut Context<AppModel>) {
        if self.selected_feature.as_deref() == Some(feature)
            && self.message.read(cx).value() == message
        {
            self.message.update(cx, |input, cx| input.clear(cx));
        }
    }

    fn begin_operation(&mut self, operation: &Operation) -> bool {
        let background = matches!(operation, Operation::Refresh);
        if self.busy {
            match operation {
                Operation::Refresh => self.refresh_pending = true,
                Operation::Navigate(navigation) => {
                    self.pending_navigation = Some(navigation.clone())
                }
                _ => {}
            }
            return false;
        }
        if background && self.refreshing {
            self.refresh_pending = true;
            return false;
        }
        self.refreshing = background;
        self.busy = !background;
        self.refresh_pending = false;
        self.generation += 1;
        true
    }

    fn finish_operation(&mut self) -> Option<Operation> {
        self.busy = false;
        self.refreshing = false;
        if let Some(navigation) = self.pending_navigation.take() {
            Some(Operation::Navigate(navigation))
        } else if std::mem::take(&mut self.refresh_pending) {
            Some(Operation::Refresh)
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub(crate) enum ForgeAction {
    Refresh,
    RetryPublications,
    OpenPulsation(String),
    /// Cross-view entry point (e.g. from Pulsations' "Compose pulsation with tasks"): jumps into the
    /// Forge view's Backlog section. In-view tab clicks use `Section` instead, which never switches
    /// `AppView` and so avoids an extra background refresh.
    OpenBacklog,
    New(bool),
    Close,
    AddDraft,
    RemoveDraft(usize),
    Kind(usize),
    SaveTasks,
    EditTask(String),
    TaskDependency(usize, String),
    CreateFeature,
    SelectFeature(String),
    ToggleTask(String),
    Dispatch,
    Send,
    CancelTurn(String),
    Approve,
    Archive,
    Reopen,
    Complete,
    Section(ForgeSection),
    Move(String, Wave),
    Dependency(String, bool),
    ConfirmSuggestion(String, String),
    SetLimit,
    Rating(usize),
    Level,
    Evaluate,
    SelectFeatureTasks(String),
    SuggestFeature(String),
}
enum Operation {
    EditTask(String, u64, TaskDraft),
    ConvertTask(String, u64, String, String),
    Refresh,
    RetryPublications,
    Navigate(ForgeNavigation),
    CreateTasks(Vec<TaskDraft>),
    CreateFeature(String, String),
    Dispatch(String, Vec<TaskSelection>),
    Send(String, u64, String),
    Approve(String, u64),
    Archive(String, String),
    Reopen(String),
    Complete(String, u64),
    Move(String, Wave),
    Dependency(String, String, Option<String>),
    SetLimit(u32),
    Evaluate(String, u64, FeatureEvaluation, u8),
}
impl AppModel {
    pub(crate) fn refresh_forge(&mut self, cx: &mut Context<Self>) {
        self.run_forge(Operation::Refresh, cx);
    }
    pub(crate) fn forge_action(&mut self, action: ForgeAction, cx: &mut Context<Self>) {
        match action {
            ForgeAction::EditTask(id) => {
                if let Some(t) = self
                    .forge
                    .snapshot
                    .tasks
                    .iter()
                    .find(|t| t.id == id)
                    .cloned()
                {
                    let mut input = DraftInputs::new(cx);
                    input
                        .title
                        .update(cx, |i, cx| i.set_value(t.draft.title, cx));
                    input
                        .description
                        .update(cx, |i, cx| i.set_value(t.draft.description, cx));
                    input.criteria.update(cx, |i, cx| {
                        i.set_value(
                            t.draft
                                .criteria
                                .iter()
                                .map(|c| c.description.as_str())
                                .collect::<Vec<_>>()
                                .join("; "),
                            cx,
                        )
                    });
                    input.kind = t.draft.kind;
                    input.dependencies = t.draft.dependencies;
                    self.forge.drafts = vec![input];
                    self.forge.editing_task = Some((id, t.revision));
                    self.forge.capture_feature = false;
                    self.forge.new_open = true;
                }
                cx.notify();
                return;
            }
            ForgeAction::TaskDependency(index, id) => {
                if let Some(d) = self.forge.drafts.get_mut(index) {
                    if d.dependencies.contains(&id) {
                        d.dependencies.retain(|d| d != &id);
                    } else {
                        d.dependencies.push(id);
                    }
                }
                cx.notify();
                return;
            }
            ForgeAction::RetryPublications => {
                self.run_forge(Operation::RetryPublications, cx);
                return;
            }
            ForgeAction::ConfirmSuggestion(pre, rationale) => {
                if let Some(f) = self.forge.feature() {
                    self.run_forge(
                        Operation::Dependency(f.id.clone(), pre, Some(rationale)),
                        cx,
                    );
                }
                return;
            }
            ForgeAction::OpenPulsation(id) => {
                self.select_pulsation(id, cx);
                self.select_view(AppView::Pulsations, cx);
                return;
            }
            ForgeAction::OpenBacklog => {
                self.forge.section = ForgeSection::Backlog;
                self.select_view(AppView::Forge, cx);
                return;
            }
            ForgeAction::New(feature) => {
                self.forge.new_open = true;
                self.forge.capture_feature = feature;
                self.forge.editing_task = None;
                self.forge.source_task = None;
                cx.notify();
                return;
            }
            ForgeAction::Close => {
                self.forge.new_open = false;
                cx.notify();
                return;
            }
            ForgeAction::AddDraft => {
                self.forge.drafts.push(DraftInputs::new(cx));
                cx.notify();
                return;
            }
            ForgeAction::RemoveDraft(i) => {
                if self.forge.drafts.len() > 1 && i < self.forge.drafts.len() {
                    self.forge.drafts.remove(i);
                }
                cx.notify();
                return;
            }
            ForgeAction::Kind(i) => {
                if let Some(d) = self.forge.drafts.get_mut(i) {
                    d.kind = match d.kind {
                        TaskKind::Bug => TaskKind::Adjustment,
                        TaskKind::Adjustment => TaskKind::Improvement,
                        TaskKind::Improvement => TaskKind::Refactor,
                        _ => TaskKind::Bug,
                    };
                }
                cx.notify();
                return;
            }
            ForgeAction::SelectFeature(id) => {
                self.forge.selected_feature = Some(id);
                self.forge.history.clear();
                if let Some(f) = self.forge.feature().cloned() {
                    for (input, value) in self.forge.filters.iter().zip([
                        f.evaluation.value,
                        f.evaluation.alignment,
                        f.evaluation.feasibility,
                        f.evaluation.usability,
                    ]) {
                        input.update(cx, |input, cx| input.set_value(value, cx));
                    }
                    self.forge.impact = f.evaluation.impact.unwrap_or(3);
                    self.forge.confidence = f.evaluation.confidence.unwrap_or(3);
                    self.forge.effort = f.evaluation.effort.unwrap_or(3);
                    self.forge.level = f.structural_level;
                }
                // Selecting a feature from the Backlog section has no core equivalent to land on;
                // Incubation is the natural first stop in a feature's lifecycle.
                if self.forge.section == ForgeSection::Backlog {
                    self.forge.section = ForgeSection::Incubation;
                }
                self.run_forge(
                    Operation::Navigate(ForgeNavigation {
                        selected_feature_id: self.forge.selected_feature.clone(),
                        section: self.forge.section.to_core_index().unwrap_or(0),
                    }),
                    cx,
                );
                return;
            }
            ForgeAction::ToggleTask(id) => {
                if !self.forge.selected_tasks.remove(&id) {
                    self.forge.selected_tasks.insert(id);
                }
                cx.notify();
                return;
            }
            ForgeAction::Section(section) => {
                self.forge.section = section;
                if let Some(core_index) = section.to_core_index() {
                    self.run_forge(
                        Operation::Navigate(ForgeNavigation {
                            selected_feature_id: self.forge.selected_feature.clone(),
                            section: core_index,
                        }),
                        cx,
                    );
                }
                cx.notify();
                return;
            }
            ForgeAction::Rating(index) => {
                let value = match index {
                    0 => &mut self.forge.impact,
                    1 => &mut self.forge.confidence,
                    _ => &mut self.forge.effort,
                };
                *value = *value % 5 + 1;
                cx.notify();
                return;
            }
            ForgeAction::Level => {
                self.forge.level = (self.forge.level + 1) % 4;
                cx.notify();
                return;
            }
            ForgeAction::CancelTurn(id) => {
                if let Some(core) = &self.core {
                    core.feature_agent().cancel(&id);
                }
                self.refresh_forge(cx);
                return;
            }
            ForgeAction::SelectFeatureTasks(id) => {
                self.forge.selected_tasks = self
                    .forge
                    .snapshot
                    .tasks
                    .iter()
                    .filter(|t| t.draft.feature_id.as_ref() == Some(&id) && t.state == "ready")
                    .map(|t| t.id.clone())
                    .collect();
                self.forge.section = ForgeSection::Backlog;
                cx.notify();
                return;
            }
            ForgeAction::SuggestFeature(id) => {
                if let Some(t) = self.forge.snapshot.tasks.iter().find(|t| t.id == id) {
                    self.forge
                        .feature_title
                        .update(cx, |i, cx| i.set_value(t.draft.title.clone(), cx));
                    self.forge
                        .idea
                        .update(cx, |i, cx| i.set_value(t.draft.description.clone(), cx));
                    self.forge.source_task = Some((t.id.clone(), t.revision));
                    self.forge.new_open = true;
                    self.forge.capture_feature = true;
                }
                cx.notify();
                return;
            }
            _ => {}
        }
        if self.forge.busy {
            return;
        }
        let f = self.forge.feature().cloned();
        let operation = match action {
            ForgeAction::Refresh => Operation::Refresh,
            ForgeAction::SaveTasks => {
                self.forge.section = ForgeSection::Backlog;
                if let Some((id, revision)) = &self.forge.editing_task {
                    Operation::EditTask(id.clone(), *revision, self.forge.drafts[0].draft(cx))
                } else {
                    Operation::CreateTasks(self.forge.drafts.iter().map(|d| d.draft(cx)).collect())
                }
            }
            ForgeAction::CreateFeature => {
                self.forge.section = ForgeSection::Incubation;
                if let Some((id, revision)) = &self.forge.source_task {
                    Operation::ConvertTask(
                        id.clone(),
                        *revision,
                        self.forge.feature_title.read(cx).value().into(),
                        self.forge.idea.read(cx).value().into(),
                    )
                } else {
                    Operation::CreateFeature(
                        self.forge.feature_title.read(cx).value().into(),
                        self.forge.idea.read(cx).value().into(),
                    )
                }
            }
            ForgeAction::Dispatch => Operation::Dispatch(
                self.forge.batch_title.read(cx).value().into(),
                self.forge
                    .snapshot
                    .tasks
                    .iter()
                    .filter(|t| self.forge.selected_tasks.contains(&t.id))
                    .map(|t| TaskSelection {
                        task_id: t.id.clone(),
                        expected_revision: t.revision,
                    })
                    .collect(),
            ),
            ForgeAction::Send => {
                let project = self
                    .workspace
                    .read(cx)
                    .project_id
                    .as_ref()
                    .map(ToString::to_string);
                let Some(operation) = self.forge.message_operation(project.as_deref(), cx) else {
                    return;
                };
                operation
            }
            ForgeAction::Approve => {
                let Some(f) = f else {
                    return;
                };
                Operation::Approve(f.id, f.revision)
            }
            ForgeAction::Archive => {
                let Some(f) = f else {
                    return;
                };
                Operation::Archive(f.id, self.forge.rationale.read(cx).value().into())
            }
            ForgeAction::Reopen => {
                let Some(f) = f else {
                    return;
                };
                Operation::Reopen(f.id)
            }
            ForgeAction::Complete => {
                let Some(f) = f else {
                    return;
                };
                Operation::Complete(f.id, f.revision)
            }
            ForgeAction::Move(id, wave) => Operation::Move(id, wave),
            ForgeAction::Dependency(prerequisite, add) => {
                let Some(f) = f else {
                    return;
                };
                Operation::Dependency(
                    f.id,
                    prerequisite,
                    add.then(|| self.forge.rationale.read(cx).value().into()),
                )
            }
            ForgeAction::SetLimit => {
                let Ok(limit) = self.forge.wave_limit.read(cx).value().parse() else {
                    self.forge.error = Some("Informe uma capacidade inteira positiva.".into());
                    cx.notify();
                    return;
                };
                Operation::SetLimit(limit)
            }
            ForgeAction::Evaluate => {
                let Some(f) = f else {
                    return;
                };
                Operation::Evaluate(
                    f.id,
                    f.revision,
                    FeatureEvaluation {
                        value: self.forge.filters[0].read(cx).value().into(),
                        alignment: self.forge.filters[1].read(cx).value().into(),
                        feasibility: self.forge.filters[2].read(cx).value().into(),
                        usability: self.forge.filters[3].read(cx).value().into(),
                        impact: Some(self.forge.impact),
                        confidence: Some(self.forge.confidence),
                        effort: Some(self.forge.effort),
                    },
                    self.forge.level,
                )
            }
            _ => return,
        };
        self.run_forge(operation, cx);
    }
    fn run_forge(&mut self, operation: Operation, cx: &mut Context<Self>) {
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
        if self.forge.project_id.as_ref() != Some(&project) {
            let generation = self.forge.generation;
            self.forge = ForgeUi::new(cx);
            self.forge.generation = generation;
            self.forge.project_id = Some(project.clone());
        }
        let background = matches!(operation, Operation::Refresh);
        if !self.forge.begin_operation(&operation) {
            return;
        }
        if !background {
            self.forge.error = None;
        }
        let generation = self.forge.generation;
        let selected = self.forge.selected_feature.clone();
        let original_selection = selected.clone();
        let runtime = core.runtime_handle().clone();
        let current_project = project.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn_blocking(move || -> Result<_, String> {
                    let forge = FeatureForgeService::new(core.persistence().clone());
                    let tasks = TaskService::new(core.persistence().clone());
                    let key = uuid::Uuid::new_v4().to_string();
                    let mut selected = selected;
                    let mut notice = None;
                    let mut navigate = None;
                    let mut clear_capture = false;
                    let mut accepted_message = None;
                    let outcome: Result<
                        (),
                        hvc_core::persistence_service::PersistenceServiceError,
                    > = (|| {
                        match operation {
                            Operation::EditTask(id, revision, draft) => {
                                tasks.revise(&project, &id, revision, draft)?;
                                clear_capture = true;
                                notice = Some("Task updated.".into());
                            }
                            Operation::ConvertTask(id, revision, title, idea) => {
                                let f = tasks.convert_to_feature(
                                    &project, &id, revision, &key, &title, &idea,
                                )?;
                                selected = Some(f.id.clone());
                                clear_capture = true;
                                navigate = Some(AppView::Forge);
                                core.feature_agent().start(
                                    &project,
                                    &f.id,
                                    f.revision,
                                    &format!("initial:{key}"),
                                    &f.original_idea,
                                )?;
                                clear_capture = true;
                                navigate = Some(AppView::Forge);
                            }
                            Operation::Refresh => {}
                            Operation::Navigate(navigation) => {
                                forge.save_navigation(&project, &navigation)?
                            }
                            Operation::RetryPublications => {
                                forge.retry_plan_publications(&project)?;
                            }
                            Operation::CreateTasks(drafts) => {
                                tasks.create(CreateTasksRequest {
                                    project_id: project.clone(),
                                    request_id: key,
                                    tasks: drafts,
                                })?;
                                notice = Some("Tasks added to backlog.".into());
                                clear_capture = true;
                                navigate = Some(AppView::Forge);
                            }
                            Operation::CreateFeature(title, idea) => {
                                let f = forge.create(CreateFeatureRequest {
                                    project_id: project.clone(),
                                    request_id: key.clone(),
                                    title,
                                    idea: idea.clone(),
                                })?;
                                selected = Some(f.id.clone());
                                clear_capture = true;
                                navigate = Some(AppView::Forge);
                                core.feature_agent().start(
                                    &project,
                                    &f.id,
                                    f.revision,
                                    &format!("initial:{key}"),
                                    &idea,
                                )?;
                                clear_capture = true;
                                navigate = Some(AppView::Forge);
                            }
                            Operation::Dispatch(title, selection) => {
                                let p = tasks.dispatch(CreatePulsationFromTasksRequest {
                                    project_id: project.clone(),
                                    request_id: key,
                                    title,
                                    tasks: selection,
                                })?;
                                notice = Some(format!(
                                    "Pulsation {} dispatched to queue.",
                                    p.feature_name
                                ));
                                navigate = Some(AppView::Pulsations);
                            }
                            Operation::Send(id, revision, message) => {
                                core.feature_agent()
                                    .start(&project, &id, revision, &key, &message)?;
                                accepted_message = Some((id, message));
                            }
                            Operation::Approve(id, revision) => {
                                forge.approve(&project, &id, revision)?;
                                notice = Some(
                                    "Scope approved; tasks created. Plan the wave before dispatch."
                                        .into(),
                                );
                            }
                            Operation::Archive(id, reason) => {
                                forge.archive(&project, &id, &reason)?
                            }
                            Operation::Reopen(id) => forge.reopen(&project, &id)?,
                            Operation::Complete(id, revision) => {
                                forge.complete(&project, &id, revision)?
                            }
                            Operation::Move(id, wave) => {
                                forge.move_to_wave(&project, &id, wave, 0)?;
                            }
                            Operation::Dependency(id, pre, reason) => {
                                forge.set_dependency(&project, &id, &pre, reason.as_deref())?
                            }
                            Operation::SetLimit(limit) => forge.set_wave_limit(&project, limit)?,
                            Operation::Evaluate(id, revision, evaluation, level) => {
                                forge.evaluate(&project, &id, revision, evaluation, level)?;
                            }
                        }
                        Ok(())
                    })();
                    let error = outcome.err().map(|e| e.to_string());
                    forge
                        .reconcile_plan_publications(&project)
                        .map_err(|e| e.to_string())?;
                    let issues = forge
                        .publication_issues(&project)
                        .map_err(|e| e.to_string())?;
                    if !issues.is_empty() {
                        notice = Some(issues.join("\n"));
                    }
                    let ForgeViewSnapshot {
                        snapshot,
                        selected_feature: selected,
                        history,
                    } = forge
                        .view_snapshot(&project, selected.as_deref())
                        .map_err(|e| e.to_string())?;
                    Ok((
                        snapshot,
                        selected,
                        history,
                        notice,
                        navigate,
                        clear_capture,
                        accepted_message,
                        error,
                    ))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = this.update(cx, |model, cx| {
                if model.forge.generation != generation
                    || model
                        .workspace
                        .read(cx)
                        .project_id
                        .as_ref()
                        .map(ToString::to_string)
                        .as_ref()
                        != Some(&current_project)
                {
                    return;
                }
                match result {
                    Ok((
                        snapshot,
                        selected,
                        history,
                        notice,
                        navigate,
                        clear_capture,
                        accepted_message,
                        error,
                    )) => {
                        // Backlog is UI-only and has no core equivalent, so a background refresh
                        // must never bounce the user out of it into a persisted feature section.
                        if model.forge.selected_feature.is_none()
                            && model.forge.section != ForgeSection::Backlog
                        {
                            model.forge.section =
                                ForgeSection::from_core_index(snapshot.navigation.section);
                        }
                        model.forge.snapshot = snapshot;
                        if model.forge.selected_feature == original_selection {
                            model.forge.selected_feature = selected;
                            model.forge.history = history;
                        }
                        if !background || notice.is_some() {
                            model.forge.notice = notice;
                        }
                        if !background || error.is_some() {
                            model.forge.error = error;
                        }
                        if clear_capture {
                            model.forge.new_open = false;
                            model.forge.drafts = vec![DraftInputs::new(cx)];
                        }
                        if let Some((feature, message)) = accepted_message {
                            model.forge.clear_accepted_message(&feature, &message, cx);
                        }
                        if let Some(view) = navigate {
                            model.select_view(view, cx);
                            model.refresh_pulsations(cx);
                        }
                    }
                    Err(error) => model.forge.error = Some(error),
                }
                if let Some(operation) = model.forge.finish_operation() {
                    model.run_forge(operation, cx);
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
    use gpui::{Focusable, Modifiers, TestAppContext, VisualTestContext, px, size};
    use hvc_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};

    fn feature() -> Feature {
        Feature {
            id: "feature".into(),
            project_id: "project".into(),
            title: "Export".into(),
            original_idea: "Export documents".into(),
            revision: 1,
            approved_revision: None,
            state: FeatureState::Incubating,
            brief: Default::default(),
            evaluation: Default::default(),
            structural_level: 0,
            wave: None,
            position: 0,
            archive_reason: None,
        }
    }

    #[gpui::test]
    fn message_admission_and_acknowledgement_preserve_newer_drafts(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            let forge = &mut model.forge;
            forge.project_id = Some("project".into());
            forge.snapshot.features = vec![feature()];
            forge.selected_feature = Some("feature".into());
            for draft in ["", " \n\t"] {
                forge.message.update(cx, |i, cx| i.set_value(draft, cx));
                assert!(forge.message_operation(Some("project"), cx).is_none());
            }
            let draft = "cafe 👋\nDetails";
            forge.message.update(cx, |i, cx| i.set_value(draft, cx));
            assert!(matches!(forge.message_operation(Some("project"), cx), Some(Operation::Send(id, 1, text)) if id == "feature" && text == draft));
            assert!(forge.message_operation(Some("other-project"), cx).is_none());
            forge.busy = true;
            assert!(forge.message_operation(Some("project"), cx).is_none());
            forge.busy = false;
            forge.history.push(FeatureTurn { id: "turn".into(), feature_id: "feature".into(), feature_revision: 1, status: "running".into(), user_message: "Previous message".into(), assistant_message: None, error: None, suggested_dependencies: vec![] });
            assert!(forge.message_operation(Some("project"), cx).is_none());
            forge.history[0].status = "failed".into();
            assert!(forge.message_operation(Some("project"), cx).is_some());
            forge.snapshot.features[0].state = FeatureState::Archived;
            assert!(forge.message_operation(Some("project"), cx).is_none());
            forge.clear_accepted_message("other-feature", draft, cx);
            assert_eq!(forge.message.read(cx).value(), draft);
            forge.clear_accepted_message("feature", "older draft", cx);
            assert_eq!(forge.message.read(cx).value(), draft);
            forge.clear_accepted_message("feature", draft, cx);
            assert_eq!(forge.message.read(cx).value(), "");
        });
    }

    fn finish_request(model: &Entity<AppModel>, visual: &mut VisualTestContext) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            visual.run_until_parked();
            if !model.read_with(visual, |m, _| m.forge.busy) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Forge request timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[gpui::test]
    fn forge_enter_and_button_share_admission_and_recreated_subscriptions(cx: &mut TestAppContext) {
        use hvc_core::persistence_service::EnsureProjectForWorkspaceRequest;
        let harness = hvc_test_support::CoreTestHarness::new();
        let core = harness.core();
        let project = core
            .persistence()
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: harness.workspace_dir().to_string_lossy().into(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        let feature = FeatureForgeService::new(core.persistence().clone())
            .create(CreateFeatureRequest {
                project_id: project.clone(),
                request_id: "composer-test".into(),
                title: "Export".into(),
                idea: "Export documents".into(),
            })
            .unwrap();
        cx.update(crate::text_input::register_text_input_keybindings);
        let (model, visual) = cx.add_window_view(|_, cx| AppModel::new_for_test(cx));
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        model.update(visual, |m, cx| {
            m.workspace
                .update(cx, |w, _| w.project_id = Some(project.clone().into()));
            m.select_view(AppView::Forge, cx);
            m.core = Some(core.clone());
            m.chat_draft_input
                .update(cx, |i, cx| i.set_value("Companion draft", cx));
        });
        let mut old_input = None;
        for _ in 0..3 {
            let input = model.update(visual, |m, cx| {
                m.forge = ForgeUi::new(cx);
                m.forge.project_id = Some(project.clone());
                m.forge.selected_feature = Some(feature.id.clone());
                m.forge.snapshot.features = vec![feature.clone()];
                // Force local admission failure before any provider can run.
                m.forge.snapshot.features[0].revision += 100;
                m.forge.section = ForgeSection::Incubation;
                m.forge
                    .message
                    .update(cx, |i, cx| i.set_value("cafe 👋", cx));
                cx.notify();
                m.forge.message.clone()
            });
            visual.run_until_parked();
            if let Some(old) = old_input.take() {
                let old: Entity<NativeTextInput> = old;
                old.update(visual, |_, cx| cx.emit(Submit));
                visual.run_until_parked();
                assert_eq!(model.read_with(visual, |m, _| m.forge.generation), 0);
            }
            visual.update(|window, cx| input.read(cx).focus_handle(cx).focus(window));
            visual.simulate_keystrokes("shift-enter");
            assert_eq!(model.read_with(visual, |m, _| m.forge.generation), 0);
            assert_eq!(
                input.read_with(visual, |i, _| i.value().to_owned()),
                "cafe 👋\n"
            );
            visual.simulate_keystrokes("enter enter enter");
            finish_request(&model, visual);
            model.read_with(visual, |m, cx| {
                assert_eq!(
                    m.forge.generation, 1,
                    "repeated Enter must have one admission"
                );
                assert!(m.forge.error.is_some());
                assert_eq!(m.forge.message.read(cx).value(), "cafe 👋\n");
                assert_eq!(m.chat_draft_input.read(cx).value(), "Companion draft");
            });
            model.update(visual, |m, cx| {
                m.forge.snapshot.features[0].revision += 100;
                cx.notify();
            });
            visual.run_until_parked();
            let button = visual.debug_bounds("send-feature-message").unwrap();
            visual.simulate_click(button.center(), Modifiers::default());
            finish_request(&model, visual);
            model.read_with(visual, |m, cx| {
                assert_eq!(m.forge.generation, 2);
                assert!(m.forge.error.is_some());
                assert_eq!(m.forge.message.read(cx).value(), "cafe 👋\n");
            });
            assert!(
                core.feature_agent()
                    .history(&project, &feature.id)
                    .unwrap()
                    .is_empty()
            );
            old_input = Some(input);
        }
    }

    #[gpui::test]
    fn refresh_bursts_coalesce_without_blocking_commands_or_losing_navigation(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, _| {
            let forge = &mut model.forge;
            assert!(forge.begin_operation(&Operation::Refresh));
            assert!(!forge.busy);
            let old_generation = forge.generation;
            for _ in 0..20 { assert!(!forge.begin_operation(&Operation::Refresh)); }
            assert_eq!(forge.generation, old_generation);
            let trailing = forge.finish_operation().expect("one trailing refresh");
            assert!(matches!(trailing, Operation::Refresh));
            assert!(forge.begin_operation(&trailing));
            // A deliberate mutation supersedes a background read immediately.
            assert!(forge.begin_operation(&Operation::SetLimit(4)));
            assert!(forge.busy);
            assert!(forge.generation > old_generation);
            assert!(!forge.begin_operation(&Operation::Refresh));
            for section in 1..=4 {
                assert!(!forge.begin_operation(&Operation::Navigate(ForgeNavigation { section, selected_feature_id: Some(format!("feature-{section}")) })));
            }
            assert!(!forge.begin_operation(&Operation::SetLimit(5)), "mutations must stay serial");
            let next = forge.finish_operation().unwrap();
            assert!(matches!(&next, Operation::Navigate(n) if n.section == 4 && n.selected_feature_id.as_deref() == Some("feature-4")));
            assert!(forge.begin_operation(&next));
            assert!(forge.finish_operation().is_none(), "navigation also loads the latest snapshot");
        });
    }

    #[test]
    fn unchanged_polls_and_stage_updates_do_not_invalidate_forge() {
        let pulse = Pulsation {
            id: "pulse".into(),
            project_id: "project".into(),
            feature_name: "Feature".into(),
            status: PulsationStatus::Running,
            stage: PulsationStage::Code,
            terminal_reason: None,
            created_at: "before".into(),
            updated_at: "before".into(),
        };
        let mut next = pulse.clone();
        next.updated_at = "after".into();
        next.stage = PulsationStage::Test;
        assert!(!task_runtime_changed(
            std::slice::from_ref(&pulse),
            std::slice::from_ref(&next)
        ));
        next.status = PulsationStatus::Done;
        assert!(task_runtime_changed(
            std::slice::from_ref(&pulse),
            std::slice::from_ref(&next)
        ));
        assert!(!task_runtime_changed(
            std::slice::from_ref(&next),
            std::slice::from_ref(&next)
        ));
        next.status = PulsationStatus::Failed;
        assert!(task_runtime_changed(&[pulse], &[next]));
    }
}
