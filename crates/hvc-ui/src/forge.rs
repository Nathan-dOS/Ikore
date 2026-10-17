use crate::{
    AppModel, AppView,
    text_input::{NativeTextInput, Submit},
};
use gpui::{AppContext, Context, Entity, Subscription};
use hvc_core::feature_forge::*;
use std::collections::{HashMap, HashSet};

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

    /// One line under the header saying what the section is for. Kept next to
    /// `label` so a new section cannot ship with a name but no explanation.
    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Backlog => {
                "Loose tasks that belong to no feature. Capture a bug or chore here and dispatch it without going through discovery."
            }
            Self::Incubation => {
                "Shape the idea with the Feature Agent until the scope stops moving. Answer its questions and the proposed specification fills in on the right."
            }
            Self::Triage => {
                "Decide whether the feature is worth building. Rate impact, confidence and effort, or let the agent judge it and approve the scope to create tasks."
            }
            Self::Dependencies => {
                "Declare what must ship before this feature. Prerequisites block dispatch until they are delivered, so the graph is what keeps waves honest."
            }
            Self::Waves => {
                "Choose what is in flight now. Current-wave capacity limits how much is committed at once; it does not change execution concurrency."
            }
            Self::Delivery => {
                "Track the tasks a feature produced through execution and review, and close the feature when all of them are done."
            }
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

/// Lifecycle order for the grouped feature list, mirroring `STATUS_GROUPS` in
/// `pulsations.rs`. A feature moves down this list as it progresses, so the
/// order is the pipeline itself.
pub(crate) const FEATURE_STATE_GROUPS: [FeatureState; 8] = [
    FeatureState::Incubating,
    FeatureState::InTriage,
    FeatureState::Approved,
    FeatureState::Planned,
    FeatureState::Executing,
    FeatureState::ReadyForReview,
    FeatureState::Done,
    FeatureState::Archived,
];

/// Stable key for expansion state. Deliberately not the serde name: this keys a
/// UI preference and must not move if the wire format ever changes.
pub(crate) const fn feature_state_group_id(state: FeatureState) -> &'static str {
    match state {
        FeatureState::Incubating => "incubating",
        FeatureState::InTriage => "in-triage",
        FeatureState::Approved => "approved",
        FeatureState::Planned => "planned",
        FeatureState::Executing => "executing",
        FeatureState::ReadyForReview => "ready-for-review",
        FeatureState::Done => "done",
        FeatureState::Archived => "archived",
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
    /// One optional path-hint input per selected task, keyed by task id
    /// (ADR-022 clarification loop, P1/A1.3): satisfies the dispatch-time
    /// anchoring gate for a task without persisting the hint to its stored
    /// draft. Created lazily as tasks are selected.
    pub path_hint_inputs: HashMap<String, Entity<NativeTextInput>>,
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
    /// Set when analysis was requested but the on-screen triage answers differ
    /// from what is stored, so the developer gets to confirm before the agent
    /// replaces their edits.
    pub pending_triage_overwrite: bool,
    /// A triage analysis was dispatched and its result has not been pulled in
    /// yet. Starting an analysis only queues the turn, so the verdict lands on
    /// a later refresh -- this is what tells that refresh to reload the form.
    pub awaiting_triage_analysis: bool,
    /// Whether the Triage analysis column is showing. The column carries the
    /// agent's verdict, so it opens on request and stays out of the way until
    /// there is something to read.
    pub triage_aside_open: bool,
    /// Width of the dependency graph panel measured by its last paint; the columns follow it.
    pub(crate) graph_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    /// Collapsed/expanded state per feature-state group in the list, keyed by
    /// [`feature_state_group_id`].
    pub group_expansion: HashMap<&'static str, bool>,
    /// Failed turns whose technical detail is open. Collapsed by default: the
    /// explanation above it is what the developer acts on.
    pub expanded_failures: HashSet<String>,
}

/// Which message a failure card's button sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureFollowUp {
    Retry,
    Ask,
}

/// Sent as the developer's own message, so the exchange reads as a
/// conversation and the agent answers it in the developer's language.
const RETRY_MESSAGE: &str = "Please try that reply again.";
const ASK_AGENT_MESSAGE: &str = "Your last reply could not be applied. Explain in plain language what went wrong and tell me what you need from me to continue.";

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
            path_hint_inputs: HashMap::new(),
            history: Vec::new(),
            drafts: vec![DraftInputs::new(cx)],
            feature_title: cx.new(|cx| NativeTextInput::new("Feature title", cx)),
            idea: cx.new(|cx| NativeTextInput::new("What problem do you want to solve?", cx)),
            message,
            _message_submit: message_submit,
            batch_title: cx.new(|cx| NativeTextInput::new("Pulsation title", cx)),
            rationale: cx.new(|cx| {
                NativeTextInput::new("Dependency or archiving rationale", cx)
                    .multiline()
                    .max_rows(6)
            }),
            wave_limit: cx.new(|cx| NativeTextInput::new("3", cx)),
            filters: [
                "Value and need",
                "Product alignment",
                "Feasibility and risks",
                "Usability and adoption",
            ]
            .into_iter()
            // Multiline, because these hold written answers: a single-line input
            // clips to one row and scrolls sideways, hiding everything the agent
            // wrote past the first line. Shift+Enter inserts a newline here --
            // plain Enter emits Submit, which the Forge only listens for on the
            // chat composer.
            .map(|s| cx.new(|cx| NativeTextInput::new(s, cx).multiline().max_rows(14)))
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
            pending_triage_overwrite: false,
            awaiting_triage_analysis: false,
            triage_aside_open: false,
            graph_bounds: Default::default(),
            group_expansion: HashMap::new(),
            expanded_failures: HashSet::new(),
        }
    }

    pub fn clear_batch_composer(&mut self, cx: &mut Context<AppModel>) {
        self.selected_tasks.clear();
        self.path_hint_inputs.clear();
        self.batch_title
            .update(cx, |input, cx| input.set_value(String::new(), cx));
    }

    pub fn is_task_selectable(&self, task: &WorkTask, failed_pulsations: &HashSet<String>) -> bool {
        task.state == "ready"
            || (task.state == "queued"
                && task
                    .active_pulsation_id
                    .as_ref()
                    .is_some_and(|id| failed_pulsations.contains(id)))
    }

    pub fn reconcile_selected_tasks(&mut self, failed_pulsations: &HashSet<String>) {
        let eligible: HashSet<String> = self
            .snapshot
            .tasks
            .iter()
            .filter(|t| self.is_task_selectable(t, failed_pulsations))
            .map(|t| t.id.clone())
            .collect();
        self.selected_tasks.retain(|id| eligible.contains(id));
        self.path_hint_inputs.retain(|id, _| eligible.contains(id));
    }

    pub fn apply_snapshot(&mut self, snapshot: ForgeSnapshot, failed_pulsations: &HashSet<String>) {
        self.snapshot = snapshot;
        self.reconcile_selected_tasks(failed_pulsations);
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

    /// The message behind a failure card's button.
    ///
    /// Same gates as the composer, and only while the newest discovery turn is
    /// a failure that allows it: a button for an older failure would resend a
    /// stale request, and one for a running turn would queue a second reply.
    fn failure_operation(
        &self,
        project: Option<&str>,
        follow_up: FailureFollowUp,
    ) -> Option<Operation> {
        let feature = self.feature()?;
        if self.busy
            || project != self.project_id.as_deref()
            || project != Some(feature.project_id.as_str())
            || feature.state == FeatureState::Archived
        {
            return None;
        }
        let history: Vec<FeatureTurn> = self
            .history
            .iter()
            .filter(|turn| turn.feature_id == feature.id)
            .cloned()
            .collect();
        let (_, actions) = failure_actions(&history, TurnKind::Discovery)?;
        let (allowed, message) = match follow_up {
            FailureFollowUp::Retry => (actions.retry, RETRY_MESSAGE),
            FailureFollowUp::Ask => (actions.ask_agent, ASK_AGENT_MESSAGE),
        };
        allowed.then(|| Operation::Send(feature.id.clone(), feature.revision, message.into()))
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

impl ForgeUi {
    /// Terminal states start collapsed: a finished or abandoned feature is
    /// history, and leaving it expanded buries the work in flight.
    pub fn group_expanded(&self, state: FeatureState) -> bool {
        self.group_expansion
            .get(feature_state_group_id(state))
            .copied()
            .unwrap_or(!matches!(
                state,
                FeatureState::Done | FeatureState::Archived
            ))
    }

    pub fn toggle_group(&mut self, state: FeatureState) {
        self.group_expansion
            .insert(feature_state_group_id(state), !self.group_expanded(state));
    }

    /// Why an agent analysis cannot start right now, or `None` when it can.
    ///
    /// Both tabs go through this so their gates cannot drift apart, and the
    /// reason is a sentence rather than a bool so the disabled button can say
    /// what is missing -- a greyed-out control with no explanation is worse
    /// than no control.
    pub fn analysis_blocked(&self, f: &Feature) -> Option<&'static str> {
        if self.busy {
            return Some("Another Forge operation is still running.");
        }
        if self.history.iter().any(|turn| turn.status == "running") {
            return Some("The Feature Agent is still working on this feature.");
        }
        if !f.brief.pending_decisions.is_empty() {
            return Some("Answer the open incubation questions first.");
        }
        None
    }

    /// Dependencies additionally wait for triage, because the structural level
    /// it assigns is what the agent reasons about when ordering the graph.
    pub fn dependency_analysis_blocked(&self, f: &Feature) -> Option<&'static str> {
        if let Some(reason) = self.analysis_blocked(f) {
            return Some(reason);
        }
        if f.state == FeatureState::Incubating || f.evaluation.score().is_none() {
            return Some("Run the triage analysis first.");
        }
        if matches!(f.state, FeatureState::Executing | FeatureState::Done) {
            return Some("Dependencies are frozen during and after execution.");
        }
        None
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
    ClearSelectedTasks,
    Dispatch,
    Send,
    CancelTurn(String),
    /// Send the same kind of reply request again after a Feature Agent reply
    /// the host could not apply (Incubation chat).
    RetryTurn,
    /// Tell the Feature Agent its last reply could not be applied and ask what
    /// it needs from the developer to continue.
    AskAgentAboutFailure,
    /// Show or hide a failed turn's raw technical detail.
    ToggleFailureDetail(String),
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
    /// "Ask the Feature Agent for a path" (ADR-022 clarification loop,
    /// P1/A1.3): the task id, resolved to its feature at dispatch time since
    /// only feature-owned tasks can reach this action.
    RequestPathClarification(String),
    /// Ask the Feature Agent for the triage judgement. Prompts first if the
    /// on-screen answers would be overwritten; `ConfirmAnalyzeTriage` is the
    /// same request past that prompt.
    AnalyzeTriage,
    ConfirmAnalyzeTriage,
    CancelAnalyzeTriage,
    AnalyzeDependencies,
    ClearAgentDependencies,
    CloseTriageAside,
    ToggleFeatureGroup(FeatureState),
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
    AnalyzeTriage(String, u64),
    AnalyzeDependencies(String, u64),
    /// Feature id plus the prerequisite ids to drop. The list is resolved from
    /// the snapshot on the UI side so this reuses `set_dependency` rather than
    /// needing a bulk-delete of its own.
    ClearAgentDependencies(String, Vec<String>),
    /// Feature id, task title.
    RequestPathClarification(String, String),
}
impl AppModel {
    pub(crate) fn refresh_forge(&mut self, cx: &mut Context<Self>) {
        self.run_forge(Operation::Refresh, cx);
    }
    /// Loads the stored triage verdict into the on-screen controls.
    ///
    /// Called when a feature is selected and again once an agent analysis lands
    /// -- and deliberately nowhere else. A background refresh must not call
    /// this, or it would wipe answers the developer is in the middle of typing.
    pub(crate) fn hydrate_triage_inputs(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.forge.feature().cloned() else {
            return;
        };
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

    /// Whether the on-screen triage answers would lose content if the agent
    /// overwrote them. Only non-empty divergent text counts: an untouched form
    /// or one that merely matches what is stored is not worth a prompt.
    fn triage_inputs_diverge(&self, f: &Feature, cx: &Context<Self>) -> bool {
        let stored = [
            &f.evaluation.value,
            &f.evaluation.alignment,
            &f.evaluation.feasibility,
            &f.evaluation.usability,
        ];
        self.forge
            .filters
            .iter()
            .zip(stored)
            .any(|(input, stored)| {
                let current = input.read(cx).value();
                !current.trim().is_empty() && current.trim() != stored.trim()
            })
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
                self.forge.pending_triage_overwrite = false;
                self.forge.triage_aside_open = false;
                self.hydrate_triage_inputs(cx);
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
                    let failed_pulsations: HashSet<String> = self
                        .pulsations
                        .read(cx)
                        .pulsations
                        .iter()
                        .filter(|p| {
                            p.status == hvc_core::persistence_service::PulsationStatus::Failed
                        })
                        .map(|p| p.id.clone())
                        .collect();
                    let can_select = self
                        .forge
                        .snapshot
                        .tasks
                        .iter()
                        .find(|t| t.id == id)
                        .is_some_and(|t| self.forge.is_task_selectable(t, &failed_pulsations));
                    if can_select {
                        self.forge
                            .path_hint_inputs
                            .entry(id.clone())
                            .or_insert_with(|| {
                                cx.new(|cx| {
                                    NativeTextInput::new("Optional: path this task touches", cx)
                                })
                            });
                        self.forge.selected_tasks.insert(id);
                    }
                } else {
                    self.forge.path_hint_inputs.remove(&id);
                }
                cx.notify();
                return;
            }
            ForgeAction::ClearSelectedTasks => {
                self.forge.selected_tasks.clear();
                self.forge.path_hint_inputs.clear();
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
            ForgeAction::AnalyzeTriage => {
                if self.forge.busy {
                    return;
                }
                let Some(f) = self.forge.feature().cloned() else {
                    return;
                };
                if self.triage_inputs_diverge(&f, cx) {
                    self.forge.pending_triage_overwrite = true;
                    cx.notify();
                    return;
                }
                self.forge.awaiting_triage_analysis = true;
                self.forge.triage_aside_open = true;
                self.run_forge(Operation::AnalyzeTriage(f.id, f.revision), cx);
                return;
            }
            ForgeAction::ConfirmAnalyzeTriage => {
                self.forge.pending_triage_overwrite = false;
                if let Some(f) = self.forge.feature().cloned() {
                    self.forge.awaiting_triage_analysis = true;
                    self.forge.triage_aside_open = true;
                    self.run_forge(Operation::AnalyzeTriage(f.id, f.revision), cx);
                }
                return;
            }
            ForgeAction::CancelAnalyzeTriage => {
                self.forge.pending_triage_overwrite = false;
                cx.notify();
                return;
            }
            ForgeAction::CloseTriageAside => {
                self.forge.triage_aside_open = false;
                cx.notify();
                return;
            }
            ForgeAction::ToggleFailureDetail(id) => {
                if !self.forge.expanded_failures.remove(&id) {
                    self.forge.expanded_failures.insert(id);
                }
                cx.notify();
                return;
            }
            ForgeAction::ToggleFeatureGroup(state) => {
                self.forge.toggle_group(state);
                cx.notify();
                return;
            }
            ForgeAction::AnalyzeDependencies => {
                if let Some(f) = self.forge.feature().cloned() {
                    self.run_forge(Operation::AnalyzeDependencies(f.id, f.revision), cx);
                }
                return;
            }
            ForgeAction::ClearAgentDependencies => {
                let Some(f) = self.forge.feature().cloned() else {
                    return;
                };
                let prerequisites = self
                    .forge
                    .snapshot
                    .dependencies
                    .iter()
                    .filter(|edge| {
                        edge.feature_id == f.id && edge.origin == DependencyOrigin::Agent
                    })
                    .map(|edge| edge.prerequisite_id.clone())
                    .collect::<Vec<_>>();
                if !prerequisites.is_empty() {
                    self.run_forge(Operation::ClearAgentDependencies(f.id, prerequisites), cx);
                }
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
            ForgeAction::RequestPathClarification(id) => {
                let Some(task) = self.forge.snapshot.tasks.iter().find(|t| t.id == id) else {
                    return;
                };
                let Some(feature_id) = task.draft.feature_id.clone() else {
                    return;
                };
                Operation::RequestPathClarification(feature_id, task.draft.title.clone())
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
                        manual_path_hint: self
                            .forge
                            .path_hint_inputs
                            .get(&t.id)
                            .map(|input| input.read(cx).value().to_string())
                            .filter(|value| !value.trim().is_empty()),
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
            ForgeAction::RetryTurn => {
                let Some(operation) = self.failure_follow_up(FailureFollowUp::Retry, cx) else {
                    return;
                };
                operation
            }
            ForgeAction::AskAgentAboutFailure => {
                let Some(operation) = self.failure_follow_up(FailureFollowUp::Ask, cx) else {
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
                    self.forge.error =
                        Some("Enter a positive whole number for the capacity.".into());
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
    fn failure_follow_up(
        &self,
        follow_up: FailureFollowUp,
        cx: &Context<Self>,
    ) -> Option<Operation> {
        let project = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        self.forge.failure_operation(project.as_deref(), follow_up)
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
                    let mut clear_batch_composer = false;
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
                                clear_batch_composer = true;
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
                            Operation::RequestPathClarification(feature_id, title) => {
                                forge.request_path_clarification(&project, &feature_id, &title)?;
                                notice = Some(
                                    "Incubation reopened; the Feature Agent will ask for a file path."
                                        .into(),
                                );
                                selected = Some(feature_id);
                                navigate = Some(AppView::Forge);
                            }
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
                            Operation::AnalyzeTriage(id, revision) => {
                                core.feature_agent()
                                    .start_triage_analysis(&project, &id, revision, &key)?;
                                notice = Some("Feature Agent is evaluating this feature…".into());
                            }
                            Operation::AnalyzeDependencies(id, revision) => {
                                core.feature_agent()
                                    .start_dependency_analysis(&project, &id, revision, &key)?;
                                notice = Some("Feature Agent is mapping prerequisites…".into());
                            }
                            Operation::ClearAgentDependencies(id, prerequisites) => {
                                let removed = prerequisites.len();
                                for prerequisite in prerequisites {
                                    forge.set_dependency(&project, &id, &prerequisite, None)?;
                                }
                                notice =
                                    Some(format!("Removed {removed} agent-proposed relation(s)."));
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
                        clear_batch_composer,
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
                        clear_batch_composer,
                        error,
                    )) => {
                        if clear_batch_composer {
                            model.forge.clear_batch_composer(cx);
                        }
                        // Backlog is UI-only and has no core equivalent, so a background refresh
                        // must never bounce the user out of it into a persisted feature section.
                        if model.forge.selected_feature.is_none()
                            && model.forge.section != ForgeSection::Backlog
                        {
                            model.forge.section =
                                ForgeSection::from_core_index(snapshot.navigation.section);
                        }
                        let failed_pulsations: HashSet<String> = model
                            .pulsations
                            .read(cx)
                            .pulsations
                            .iter()
                            .filter(|p| {
                                p.status == hvc_core::persistence_service::PulsationStatus::Failed
                            })
                            .map(|p| p.id.clone())
                            .collect();
                        model.forge.apply_snapshot(snapshot, &failed_pulsations);
                        if model.forge.selected_feature == original_selection {
                            model.forge.selected_feature = selected;
                            model.forge.history = history;
                        }
                        // Starting an analysis only queues the turn, so the verdict
                        // lands on a later refresh. Reload the form the moment that
                        // turn stops running -- and only then, so an unrelated
                        // refresh never overwrites answers being typed.
                        if model.forge.awaiting_triage_analysis
                            && !model.forge.history.iter().any(|turn| {
                                turn.kind == TurnKind::Triage && turn.status == "running"
                            })
                        {
                            model.forge.awaiting_triage_analysis = false;
                            model.hydrate_triage_inputs(cx);
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
            forge.history.push(FeatureTurn { id: "turn".into(), feature_id: "feature".into(), feature_revision: 1, status: "running".into(), user_message: "Previous message".into(), assistant_message: None, error: None, suggested_dependencies: vec![], kind: TurnKind::Discovery, failure: None, notices: vec![] });
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

    fn failed_turn(id: &str, kind: TurnFailureKind) -> FeatureTurn {
        FeatureTurn {
            id: id.into(),
            feature_id: "feature".into(),
            feature_revision: 1,
            status: "failed".into(),
            user_message: "Answers".into(),
            assistant_message: None,
            error: Some("raw".into()),
            suggested_dependencies: vec![],
            kind: TurnKind::Discovery,
            failure: Some(TurnFailure::new(kind, "raw")),
            notices: vec![],
        }
    }

    #[gpui::test]
    fn failure_follow_ups_exist_only_for_the_newest_failed_reply(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, _| {
            let forge = &mut model.forge;
            forge.project_id = Some("project".into());
            forge.snapshot.features = vec![feature()];
            forge.selected_feature = Some("feature".into());
            let retry = |forge: &ForgeUi| forge.failure_operation(Some("project"), FailureFollowUp::Retry);
            let ask = |forge: &ForgeUi| forge.failure_operation(Some("project"), FailureFollowUp::Ask);

            assert!(retry(forge).is_none() && ask(forge).is_none(), "nothing failed yet");

            forge.history = vec![failed_turn("t1", TurnFailureKind::ReplyUnreadable)];
            assert!(matches!(retry(forge), Some(Operation::Send(id, 1, text)) if id == "feature" && text == RETRY_MESSAGE));
            assert!(matches!(ask(forge), Some(Operation::Send(id, 1, text)) if id == "feature" && text == ASK_AGENT_MESSAGE));
            assert!(forge.failure_operation(Some("other-project"), FailureFollowUp::Retry).is_none());

            forge.busy = true;
            assert!(retry(forge).is_none(), "the same gate as the composer");
            forge.busy = false;

            let mut running = failed_turn("t2", TurnFailureKind::Unclassified);
            running.status = "running".into();
            running.failure = None;
            forge.history.push(running);
            assert!(retry(forge).is_none(), "a newer running turn owns the conversation");

            forge.history = vec![failed_turn("t1", TurnFailureKind::ProviderSetup)];
            assert!(retry(forge).is_some());
            assert!(ask(forge).is_none(), "the agent is not asked about a route it cannot use");

            forge.history = vec![failed_turn("t1", TurnFailureKind::ContextTooLarge)];
            assert!(retry(forge).is_none() && ask(forge).is_none());

            forge.history = vec![
                failed_turn("t1", TurnFailureKind::ReplyLanguage),
                failed_turn("t2", TurnFailureKind::ReplyLanguage),
            ];
            assert!(retry(forge).is_some());
            assert!(ask(forge).is_none(), "the second identical failure stops the paid follow-up");

            forge.history = vec![failed_turn("t1", TurnFailureKind::ReplyLanguage)];
            forge.snapshot.features[0].state = FeatureState::Archived;
            assert!(retry(forge).is_none(), "an archived feature takes no messages");
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

    fn test_task(id: &str, state: &str, active_pulse: Option<&str>) -> WorkTask {
        WorkTask {
            id: id.into(),
            project_id: "project".into(),
            revision: 1,
            draft: TaskDraft {
                title: format!("Task {id}"),
                kind: TaskKind::Feature,
                description: "Test description".into(),
                criteria: vec![],
                dependencies: vec![],
                feature_id: None,
            },
            feature_revision: None,
            state: state.into(),
            active_pulsation_id: active_pulse.map(Into::into),
            completed_pulsation_id: None,
        }
    }

    #[gpui::test]
    fn reconcile_selected_tasks_evicts_done_and_ineligible_tasks(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            let forge = &mut model.forge;
            forge.snapshot.tasks = vec![
                test_task("task-ready", "ready", None),
                test_task("task-done", "done", None),
                test_task("task-queued-failed", "queued", Some("pulse-failed")),
                test_task("task-queued-running", "queued", Some("pulse-running")),
            ];
            for id in [
                "task-ready",
                "task-done",
                "task-queued-failed",
                "task-queued-running",
            ] {
                forge.selected_tasks.insert(id.into());
                forge
                    .path_hint_inputs
                    .insert(id.into(), cx.new(|cx| NativeTextInput::new("hint", cx)));
            }
            let mut failed_pulsations = HashSet::new();
            failed_pulsations.insert("pulse-failed".into());

            forge.reconcile_selected_tasks(&failed_pulsations);

            assert!(forge.selected_tasks.contains("task-ready"));
            assert!(forge.path_hint_inputs.contains_key("task-ready"));

            assert!(forge.selected_tasks.contains("task-queued-failed"));
            assert!(forge.path_hint_inputs.contains_key("task-queued-failed"));

            assert!(!forge.selected_tasks.contains("task-done"));
            assert!(!forge.path_hint_inputs.contains_key("task-done"));

            assert!(!forge.selected_tasks.contains("task-queued-running"));
            assert!(!forge.path_hint_inputs.contains_key("task-queued-running"));
        });
    }

    #[gpui::test]
    fn clear_batch_composer_resets_selection_hints_and_title(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            let forge = &mut model.forge;
            forge.selected_tasks.insert("task-1".into());
            forge.path_hint_inputs.insert(
                "task-1".into(),
                cx.new(|cx| NativeTextInput::new("hint", cx)),
            );
            forge
                .batch_title
                .update(cx, |input, cx| input.set_value("Batch Title", cx));

            forge.clear_batch_composer(cx);

            assert!(forge.selected_tasks.is_empty());
            assert!(forge.path_hint_inputs.is_empty());
            assert_eq!(forge.batch_title.read(cx).value(), "");
        });
    }

    #[gpui::test]
    fn toggle_task_rejects_ineligible_tasks_and_allows_deselection(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.forge.snapshot.tasks = vec![
                test_task("task-ready", "ready", None),
                test_task("task-done", "done", None),
            ];

            // Toggling ready task selects it
            model.forge_action(ForgeAction::ToggleTask("task-ready".into()), cx);
            assert!(model.forge.selected_tasks.contains("task-ready"));

            // Toggling done task does NOT select it
            model.forge_action(ForgeAction::ToggleTask("task-done".into()), cx);
            assert!(!model.forge.selected_tasks.contains("task-done"));

            // If a done task was already selected (e.g. transient state), toggle deselects it
            model.forge.selected_tasks.insert("task-done".into());
            model.forge_action(ForgeAction::ToggleTask("task-done".into()), cx);
            assert!(!model.forge.selected_tasks.contains("task-done"));
        });
    }

    #[gpui::test]
    fn clear_selected_tasks_action_clears_selection_and_hints(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.forge.selected_tasks.insert("task-1".into());
            model.forge.path_hint_inputs.insert(
                "task-1".into(),
                cx.new(|cx| NativeTextInput::new("hint", cx)),
            );
            model.forge_action(ForgeAction::ClearSelectedTasks, cx);
            assert!(model.forge.selected_tasks.is_empty());
            assert!(model.forge.path_hint_inputs.is_empty());
        });
    }

    #[gpui::test]
    fn selected_done_task_retains_deselection_button_and_composer_remove_button(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let (model, visual) = cx.add_window_view(|_, cx| AppModel::new_for_test(cx));
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        model.update(visual, |m, cx| {
            m.workspace
                .update(cx, |w, _| w.project_id = Some("test-project".into()));
            m.forge.project_id = Some("test-project".into());
            m.select_view(AppView::Forge, cx);
            m.forge.section = ForgeSection::Backlog;
            m.forge.snapshot.tasks = vec![test_task("task-done-1", "done", None)];
            m.forge.selected_tasks.insert("task-done-1".into());
            cx.notify();
        });
        visual.run_until_parked();

        // The backlog card still displays "select-task-task-done-1" because selected == true
        assert!(visual.debug_bounds("select-task-task-done-1").is_some());
        // The composer card displays "remove-selected-task-task-done-1"
        assert!(
            visual
                .debug_bounds("remove-selected-task-task-done-1")
                .is_some()
        );
        // The composer card displays "clear-selected-tasks"
        assert!(visual.debug_bounds("clear-selected-tasks").is_some());

        // Clicking the ✕ button in composer removes it
        let remove_btn = visual
            .debug_bounds("remove-selected-task-task-done-1")
            .unwrap();
        visual.simulate_click(remove_btn.center(), Modifiers::default());
        visual.run_until_parked();

        model.read_with(visual, |m, _| {
            assert!(m.forge.selected_tasks.is_empty());
        });

        // A non-selected done task in a fresh window renders neither the backlog select button nor composer remove button
        let (model2, visual2) = cx.add_window_view(|_, cx| AppModel::new_for_test(cx));
        visual2.simulate_resize(size(px(1440.0), px(900.0)));
        model2.update(visual2, |m, cx| {
            m.workspace
                .update(cx, |w, _| w.project_id = Some("test-project".into()));
            m.forge.project_id = Some("test-project".into());
            m.select_view(AppView::Forge, cx);
            m.forge.section = ForgeSection::Backlog;
            m.forge.snapshot.tasks = vec![test_task("task-done-2", "done", None)];
            cx.notify();
        });
        visual2.run_until_parked();

        assert!(visual2.debug_bounds("select-task-task-done-2").is_none());
        assert!(
            visual2
                .debug_bounds("remove-selected-task-task-done-2")
                .is_none()
        );
        assert!(visual2.debug_bounds("clear-selected-tasks").is_none());
    }
}
