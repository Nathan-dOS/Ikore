//! Project-scoped floating Companion presentation. Host decisions remain authoritative.
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Entity, Task};
use ikore_core::companion_dock::{CompanionDockItem, CompanionDockLayout, CompanionDraft};
use ikore_core::orchestration::decisions::{PendingDecision, PendingDecisionKind};

use crate::app_model::{AppModel, AppView};
use crate::text_input::NativeTextInput;

type DraftContext = (Option<String>, Option<String>, Option<u64>);

struct ShutdownSnapshot {
    project: String,
    drafts: Vec<CompanionDraft>,
    layout_revision: u64,
    layout_dirty: bool,
    layout: CompanionDockLayout,
}

fn persist_shutdown_snapshot(
    core: &ikore_core::AppCore,
    snapshot: ShutdownSnapshot,
) -> Result<(), ikore_core::persistence_service::PersistenceServiceError> {
    let stored = core.persistence().load_companion_dock(&snapshot.project)?;
    for mut draft in snapshot.drafts {
        if let Some(previous) = stored
            .drafts
            .iter()
            .find(|previous| previous.topic_id == draft.topic_id)
        {
            draft.revision = draft.revision.max(previous.revision.saturating_add(1));
        }
        core.persistence()
            .save_companion_draft(&snapshot.project, &draft)?;
    }
    if snapshot.layout_dirty {
        core.persistence().save_companion_dock(
            &snapshot.project,
            snapshot
                .layout_revision
                .max(stored.layout_revision.saturating_add(1)),
            &snapshot.layout,
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct DockWindow {
    pub key: String,
    pub conversation_id: Option<String>,
    pub decision_id: Option<String>,
    pub minimized: bool,
}

pub(crate) struct CompanionDockUi {
    pub project_id: Option<String>,
    pub open: bool,
    pub history_open: bool,
    pub history_search: String,
    pub windows: Vec<DockWindow>,
    pub pending: Vec<PendingDecision>,
    pub decisions_initialized: bool,
    seen: HashSet<String>,
    pub error: Option<String>,
    pub persistence_error: Option<String>,
    pub layout_revision: u64,
    layout_dirty: bool,
    draft_revisions: HashMap<String, u64>,
    draft_contents: HashMap<String, String>,
    draft_dirty: HashSet<String>,
    draft_context: HashMap<String, DraftContext>,
    pub restored_drafts: HashMap<String, String>,
    pub forms: HashMap<String, Entity<NativeTextInput>>,
    pub confirming: Option<String>,
    pub action_busy: HashSet<String>,
    pub history_input: Option<Entity<NativeTextInput>>,
    pub focus: Option<gpui::FocusHandle>,
    focus_owner: Option<gpui::FocusHandle>,
    focus_subscription: Option<gpui::Subscription>,
    pub subscriptions: Vec<gpui::Subscription>,
    pub new_chat_pending: bool,
    pub(crate) shutting_down: bool,
    loaded: bool,
    in_flight: bool,
    signature: u64,
    refreshed: Instant,
    task: Option<Task<()>>,
    decision_tasks: HashMap<String, Task<()>>,
    write_tasks: HashMap<String, Task<()>>,
    write_generation: u64,
    pub(crate) save_tasks: HashMap<String, Task<()>>,
}

impl Default for CompanionDockUi {
    fn default() -> Self {
        Self {
            project_id: None,
            open: false,
            history_open: false,
            history_search: String::new(),
            windows: Vec::new(),
            pending: Vec::new(),
            decisions_initialized: false,
            seen: HashSet::new(),
            error: None,
            persistence_error: None,
            layout_revision: 0,
            layout_dirty: false,
            draft_revisions: HashMap::new(),
            draft_contents: HashMap::new(),
            draft_dirty: HashSet::new(),
            draft_context: HashMap::new(),
            restored_drafts: HashMap::new(),
            forms: HashMap::new(),
            confirming: None,
            action_busy: HashSet::new(),
            history_input: None,
            focus: None,
            focus_owner: None,
            focus_subscription: None,
            subscriptions: Vec::new(),
            new_chat_pending: false,
            shutting_down: false,
            loaded: false,
            in_flight: false,
            signature: 0,
            refreshed: Instant::now(),
            task: None,
            decision_tasks: HashMap::new(),
            write_tasks: HashMap::new(),
            write_generation: 0,
            save_tasks: HashMap::new(),
        }
    }
}

pub(crate) fn window_capacity(width: f32) -> usize {
    if width >= 1440.0 {
        3
    } else if width >= 960.0 {
        2
    } else {
        1
    }
}

fn save_draft_verified(
    persistence: &ikore_core::persistence_service::PersistenceService,
    project: &str,
    draft: &CompanionDraft,
) -> Result<(), String> {
    if persistence
        .save_companion_draft(project, draft)
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let stored = persistence
        .load_companion_dock(project)
        .map_err(|error| error.to_string())?;
    if stored.drafts.iter().any(|saved| saved == draft) {
        Ok(())
    } else {
        Err("The draft changed on disk. Your unsent text is preserved; refresh its context before retrying.".into())
    }
}

fn save_layout_verified(
    persistence: &ikore_core::persistence_service::PersistenceService,
    project: &str,
    revision: u64,
    layout: &CompanionDockLayout,
) -> Result<(), String> {
    if persistence
        .save_companion_dock(project, revision, layout)
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let stored = persistence
        .load_companion_dock(project)
        .map_err(|error| error.to_string())?;
    if stored.layout_revision == revision && &stored.layout == layout {
        Ok(())
    } else {
        Err("The Companion layout changed on disk. Your current layout is preserved.".into())
    }
}

impl CompanionDockUi {
    pub(crate) fn prepare_benchmark(&mut self) {
        self.project_id = Some("benchmark-project".into());
        self.open = true;
        self.loaded = true;
        self.in_flight = true;
        self.windows = (0..3)
            .map(|index| DockWindow {
                key: format!("chat:benchmark-chat-{index}"),
                conversation_id: Some(format!("benchmark-chat-{index}")),
                decision_id: None,
                minimized: false,
            })
            .collect();
    }

    fn open_topic(
        &mut self,
        key: String,
        conversation_id: Option<String>,
        decision_id: Option<String>,
    ) {
        self.open = true;
        self.store_topic(DockWindow {
            key,
            conversation_id,
            decision_id,
            minimized: false,
        });
    }

    fn store_topic(&mut self, mut item: DockWindow) {
        if let Some(index) = self
            .windows
            .iter()
            .position(|previous| previous.key == item.key)
        {
            let previous = self.windows.remove(index);
            item.conversation_id = item.conversation_id.or(previous.conversation_id);
            item.decision_id = item.decision_id.or(previous.decision_id);
            item.minimized &= previous.minimized;
        }
        if let Some(conversation) = item.conversation_id.as_deref() {
            // A decision owns its card identity; a history link is only an alias
            // for its conversation. Never merge two distinct guarded decisions.
            if item.decision_id.is_none()
                && let Some(index) = self.windows.iter().position(|previous| {
                    previous.decision_id.is_some()
                        && previous.conversation_id.as_deref() == Some(conversation)
                })
            {
                let decision = self.windows.remove(index);
                item.key = decision.key;
                item.decision_id = decision.decision_id;
                item.minimized &= decision.minimized;
            }
            self.windows.retain(|previous| {
                let alias = previous.decision_id.is_none()
                    && previous.conversation_id.as_deref() == Some(conversation);
                if alias {
                    item.minimized &= previous.minimized;
                }
                !alias
            });
        }
        self.windows.push(item);
    }

    fn restore_layout(&mut self, layout: CompanionDockLayout) {
        self.open = layout.open;
        self.windows.clear();
        for item in layout.conversations {
            self.store_topic(DockWindow {
                key: item.topic_id,
                conversation_id: item.conversation_id,
                decision_id: item.decision_id,
                minimized: item.minimized,
            });
        }
        for item in &mut self.windows {
            item.minimized |= item.decision_id.is_some();
        }
        if !self.windows.is_empty() && self.windows.iter().all(|item| item.decision_id.is_some()) {
            self.open = false;
        }
    }

    fn accept_created_chat(&mut self, conversation: Option<String>) -> bool {
        if !self.new_chat_pending {
            return false;
        }
        self.new_chat_pending = false;
        let Some(conversation) = conversation else {
            return false;
        };
        self.open_topic(format!("chat:{conversation}"), Some(conversation), None);
        true
    }

    pub(crate) fn form_draft_is_stale(&self, key: &str, state_version: u64) -> bool {
        self.draft_context
            .get(key)
            .and_then(|context| context.2)
            .is_some_and(|saved| saved != state_version)
    }

    fn close(&mut self, key: &str) {
        self.windows.retain(|item| item.key != key);
    }
    fn minimize(&mut self, key: &str) {
        if let Some(item) = self.windows.iter_mut().find(|item| item.key == key) {
            item.minimized = true;
        }
    }
    pub fn visible_windows(&self, width: f32) -> Vec<DockWindow> {
        if !self.open {
            return Vec::new();
        }
        self.windows
            .iter()
            .rev()
            .filter(|item| !item.minimized)
            .take(window_capacity(width))
            .cloned()
            .collect()
    }
    fn should_auto_open(&mut self, identity: &str) -> bool {
        self.seen.insert(identity.to_owned()) && self.decisions_initialized
    }
    fn layout(&self) -> CompanionDockLayout {
        CompanionDockLayout {
            open: self.open,
            conversations: self
                .windows
                .iter()
                .map(|item| CompanionDockItem {
                    topic_id: item.key.clone(),
                    conversation_id: item.conversation_id.clone(),
                    decision_id: item.decision_id.clone(),
                    minimized: item.minimized,
                })
                .collect(),
        }
    }

    fn record_draft(&mut self, id: String, content: String) -> bool {
        if content.is_empty() && !self.draft_contents.contains_key(&id) {
            return false;
        }
        if self.draft_contents.get(&id) == Some(&content) {
            return false;
        }
        let version = self.draft_revisions.entry(id.clone()).or_default();
        *version = version.saturating_add(1);
        self.draft_contents.insert(id.clone(), content);
        self.draft_dirty.insert(id);
        true
    }

    fn dirty_drafts(&self) -> Vec<CompanionDraft> {
        self.draft_dirty
            .iter()
            .map(|id| CompanionDraft {
                topic_id: id.clone(),
                conversation_id: self
                    .draft_context
                    .get(id)
                    .and_then(|context| context.0.clone()),
                revision: *self.draft_revisions.get(id).unwrap_or(&1),
                content: self.draft_contents.get(id).cloned().unwrap_or_default(),
                decision_id: self
                    .draft_context
                    .get(id)
                    .and_then(|context| context.1.clone()),
                decision_state_version: self.draft_context.get(id).and_then(|context| context.2),
            })
            .collect()
    }

    fn next_write_key(&mut self, kind: &str) -> String {
        self.write_generation = self.write_generation.saturating_add(1);
        format!("{kind}:{}", self.write_generation)
    }

    fn acknowledge_draft(
        &mut self,
        project: &str,
        topic: &str,
        revision: u64,
        result: Result<(), String>,
    ) {
        if self.project_id.as_deref() != Some(project)
            || self.draft_revisions.get(topic) != Some(&revision)
        {
            return;
        }
        match result {
            Ok(()) => {
                self.draft_dirty.remove(topic);
            }
            Err(error) => self.persistence_error = Some(error),
        }
        if self.draft_dirty.is_empty() && !self.layout_dirty {
            self.persistence_error = None;
        }
    }

    fn acknowledge_layout(&mut self, project: &str, revision: u64, result: Result<(), String>) {
        if self.project_id.as_deref() != Some(project) || self.layout_revision != revision {
            return;
        }
        match result {
            Ok(()) => self.layout_dirty = false,
            Err(error) => self.persistence_error = Some(error),
        }
        if self.draft_dirty.is_empty() && !self.layout_dirty {
            self.persistence_error = None;
        }
    }
}

impl AppModel {
    pub(crate) fn observe_companion_dock(&mut self, cx: &mut Context<Self>) {
        if self.companion_dock.shutting_down {
            return;
        }
        let project = self
            .chat
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        if project != self.companion_dock.project_id {
            self.persist_companion_immediately(cx);
            let focus = self.companion_dock.focus.take();
            let focus_subscription = self.companion_dock.focus_subscription.take();
            self.companion_dock = CompanionDockUi {
                project_id: project.clone(),
                focus,
                focus_subscription,
                ..Default::default()
            };
        }
        self.companion_dock
            .focus
            .get_or_insert_with(|| cx.focus_handle());
        let Some(project) = project else {
            return;
        };
        let mut ids = self
            .companion_dock
            .windows
            .iter()
            .filter(|item| self.companion_dock.open && !item.minimized)
            .filter_map(|item| item.conversation_id.clone())
            .collect::<Vec<_>>();
        if self.active_view() == AppView::Chat
            && let Some(id) = self.chat.read(cx).active_conversation.as_ref()
        {
            ids.push(id.to_string());
        }
        if !self.companion_dock.restored_drafts.is_empty() {
            ids.extend(
                self.companion_dock
                    .restored_drafts
                    .keys()
                    .filter(|id| self.chat_composer_inputs.contains_key(id.as_str()))
                    .cloned(),
            );
        }
        ids.sort();
        ids.dedup();
        for id in ids {
            self.ensure_chat_composer(&id, cx);
            let chat = self.chat.read(cx);
            if !chat.messages_by_conversation.contains_key(&id)
                && !chat.is_loading(&id)
                && chat.error_for(&id).is_none()
            {
                self.load_chat_conversation(id, cx);
            }
        }
        if self.companion_dock.history_input.is_none() {
            let input = cx.new(|cx| NativeTextInput::new("Search conversations", cx));
            self.companion_dock
                .subscriptions
                .push(cx.observe(&input, |model, input, cx| {
                    model.companion_dock.history_search = input.read(cx).value().to_owned();
                    cx.notify();
                }));
            self.companion_dock.history_input = Some(input);
        }
        let Some(core) = self.core.clone() else {
            return;
        };
        let state = self.pulsations.read(cx);
        let mut signature = DefaultHasher::new();
        for checkpoint in &state.hitl_checkpoints {
            checkpoint.checkpoint_id.hash(&mut signature);
            checkpoint.state_version.hash(&mut signature);
        }
        for checkpoint in &state.failure_checkpoints {
            checkpoint.checkpoint_id.hash(&mut signature);
            checkpoint.state_version.hash(&mut signature);
        }
        for fallback in &self.agents.read(cx).fallbacks {
            fallback.checkpoint_id.hash(&mut signature);
            fallback.state_version.hash(&mut signature);
        }
        for feature in &self.forge.snapshot.features {
            feature.id.hash(&mut signature);
            feature.revision.hash(&mut signature);
            std::mem::discriminant(&feature.state).hash(&mut signature);
        }
        if let Some(turn) = self.forge.history.last() {
            turn.id.hash(&mut signature);
            turn.status.hash(&mut signature);
        }
        let signature = signature.finish();
        if self.companion_dock.in_flight
            || (self.companion_dock.loaded
                && signature == self.companion_dock.signature
                && self.companion_dock.refreshed.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        self.companion_dock.in_flight = true;
        let first = !self.companion_dock.loaded;
        let runtime = core.runtime_handle().clone();
        self.companion_dock.task = Some(cx.spawn(async move |this, cx| {
            let query_project = project.clone();
            let result = runtime
                .spawn_blocking(move || {
                    let pending = core.persistence().list_pending_decisions(&query_project)?;
                    let snapshot = if first {
                        Some(core.persistence().load_companion_dock(&query_project)?)
                    } else {
                        None
                    };
                    Ok::<_, ikore_core::persistence_service::PersistenceServiceError>((
                        pending, snapshot,
                    ))
                })
                .await;
            let _ = this.update(cx, |model, cx| {
                if model.companion_dock.project_id.as_deref() != Some(&project) {
                    return;
                }
                model.companion_dock.in_flight = false;
                model.companion_dock.signature = signature;
                model.companion_dock.refreshed = Instant::now();
                match result {
                    Ok(Ok((pending, snapshot))) => {
                        if let Some(snapshot) = snapshot {
                            if !model.companion_dock.layout_dirty {
                                model.companion_dock.layout_revision = snapshot.layout_revision;
                                model.companion_dock.restore_layout(snapshot.layout);
                            } else {
                                model.companion_dock.layout_revision = model
                                    .companion_dock
                                    .layout_revision
                                    .max(snapshot.layout_revision)
                                    .saturating_add(1);
                            }
                            for draft in snapshot.drafts {
                                let key = draft.topic_id.clone();
                                let dirty = model.companion_dock.draft_dirty.contains(&key);
                                let local = model
                                    .companion_dock
                                    .draft_revisions
                                    .entry(key.clone())
                                    .or_default();
                                *local = if dirty {
                                    (*local).max(draft.revision).saturating_add(1)
                                } else {
                                    (*local).max(draft.revision)
                                };
                                model.companion_dock.draft_context.insert(
                                    key.clone(),
                                    (
                                        draft.conversation_id.clone(),
                                        draft.decision_id,
                                        draft.decision_state_version,
                                    ),
                                );
                                if !dirty {
                                    model.companion_dock.restored_drafts.insert(
                                        draft.conversation_id.unwrap_or(key.clone()),
                                        draft.content.clone(),
                                    );
                                    model
                                        .companion_dock
                                        .draft_contents
                                        .insert(key, draft.content);
                                }
                            }
                            if !model.companion_dock.draft_dirty.is_empty() {
                                model.persist_companion_immediately(cx);
                            }
                            if model.companion_dock.layout_dirty {
                                model.persist_companion_layout(cx);
                            }
                        }
                        for decision in &pending {
                            if model.companion_dock.should_auto_open(&decision.id) {
                                model.companion_dock.open_topic(
                                    format!("decision:{}", decision.id),
                                    None,
                                    Some(decision.id.clone()),
                                );
                            }
                        }
                        model.companion_dock.decisions_initialized = true;
                        model.companion_dock.pending = pending;
                        model.companion_dock.loaded = true;
                        model.companion_dock.error = None;
                    }
                    Ok(Err(error)) => model.companion_dock.error = Some(error.to_string()),
                    Err(error) => {
                        model.companion_dock.error =
                            Some(format!("Companion refresh interrupted: {error}"))
                    }
                }
                let pending = model.companion_dock.pending.clone();
                for decision in &pending {
                    model.ensure_companion_forge_forms(decision, cx);
                    model.ensure_companion_form(
                        format!("feedback:{}", decision.id),
                        decision,
                        "Feedback or reason",
                        cx,
                    );
                    if let Some(set) = &decision.question_set {
                        for question in &set.questions {
                            model.ensure_companion_form(
                                format!("answer:{}:{}", decision.id, question.id),
                                decision,
                                "Your answer",
                                cx,
                            );
                        }
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_companion_dock(&mut self, cx: &mut Context<Self>) {
        self.companion_dock.open = !self.companion_dock.open;
        self.persist_companion_layout(cx);
        cx.notify();
    }
    pub(crate) fn observe_companion_dock_focus(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.shutting_down {
            return;
        }
        let Some(focus) = self.companion_dock.focus.clone() else {
            return;
        };
        // Retain the exact previous owner through paint, even if its button is
        // removed while the window is inactive. A deliberate blur clears it.
        self.companion_dock.focus_owner = if focus.contains_focused(window, cx) {
            window.focused(cx)
        } else {
            None
        };
        if self.companion_dock.focus_subscription.is_some() {
            return;
        }
        self.companion_dock.focus_subscription =
            Some(cx.on_focus_lost(window, |model, window, _| {
                let previous_owner = model.companion_dock.focus_owner.take();
                if !model.companion_dock.shutting_down
                    && window.context_stack().is_empty()
                    && previous_owner.is_some_and(|owner| owner.is_focused(window))
                    && let Some(focus) = &model.companion_dock.focus
                {
                    // The container stays mounted with the launcher, so shell
                    // shortcuts remain reachable after any child is replaced/hidden,
                    // including while inactive. This does not activate the window.
                    focus.focus(window);
                }
            }));
    }
    pub(crate) fn open_companion_chat(&mut self, id: String, cx: &mut Context<Self>) {
        self.companion_dock
            .open_topic(format!("chat:{id}"), Some(id.clone()), None);
        self.ensure_chat_composer(&id, cx);
        self.select_chat_conversation(id, cx);
        self.persist_companion_layout(cx);
        cx.notify();
    }
    pub(crate) fn open_companion_decision(&mut self, id: String, cx: &mut Context<Self>) {
        self.companion_dock
            .open_topic(format!("decision:{id}"), None, Some(id));
        self.persist_companion_layout(cx);
        cx.notify();
    }
    pub(crate) fn close_companion_topic(
        &mut self,
        key: String,
        minimize: bool,
        cx: &mut Context<Self>,
    ) {
        if minimize {
            self.companion_dock.minimize(&key);
        } else {
            self.companion_dock.close(&key);
        }
        self.persist_companion_immediately(cx);
        self.persist_companion_layout(cx);
        cx.notify();
    }
    pub(crate) fn expand_companion_chat(&mut self, id: String, cx: &mut Context<Self>) {
        let mut minimized = false;
        for window in &mut self.companion_dock.windows {
            if window.conversation_id.as_deref() == Some(id.as_str()) && !window.minimized {
                window.minimized = true;
                minimized = true;
            }
        }
        if minimized {
            self.persist_companion_layout(cx);
        }
        self.select_chat_conversation(id, cx);
        self.select_view(AppView::Chat, cx);
    }
    fn persist_companion_layout(&mut self, cx: &mut Context<Self>) {
        self.companion_dock.layout_dirty = true;
        if self.companion_dock.shutting_down {
            self.companion_dock.layout_revision =
                self.companion_dock.layout_revision.saturating_add(1);
            return;
        }
        let (Some(core), Some(project)) =
            (self.core.clone(), self.companion_dock.project_id.clone())
        else {
            return;
        };
        self.companion_dock.layout_revision = self.companion_dock.layout_revision.saturating_add(1);
        let revision = self.companion_dock.layout_revision;
        let layout = self.companion_dock.layout();
        let io_project = project.clone();
        let job = core.runtime_handle().clone().spawn_blocking(move || {
            save_layout_verified(core.persistence(), &io_project, revision, &layout)
        });
        let task_key = self.companion_dock.next_write_key("layout");
        let completion_key = task_key.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = job
                .await
                .map_err(|error| format!("Companion layout save interrupted: {error}"))
                .and_then(|result| result);
            let _ = this.update(cx, |model, cx| {
                model
                    .companion_dock
                    .acknowledge_layout(&project, revision, result);
                if model.companion_dock.project_id.as_deref() == Some(project.as_str()) {
                    model.companion_dock.write_tasks.remove(&completion_key);
                }
                cx.notify();
            });
        });
        self.companion_dock.write_tasks.insert(task_key, task);
    }

    pub(crate) fn companion_draft_changed(
        &mut self,
        id: String,
        content: String,
        cx: &mut Context<Self>,
    ) {
        let key = format!("chat:{id}");
        self.companion_dock
            .draft_context
            .insert(key.clone(), (Some(id), None, None));
        self.companion_save_draft(key, content, cx);
    }
    fn companion_save_draft(&mut self, id: String, content: String, cx: &mut Context<Self>) {
        if !self.companion_dock.record_draft(id.clone(), content) {
            return;
        }
        if self.companion_dock.shutting_down {
            return;
        }
        self.companion_dock.save_tasks.insert(
            id.clone(),
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let _ = this.update(cx, |model, cx| model.persist_companion_immediately(cx));
            }),
        );
    }

    pub(crate) fn persist_companion_immediately(&mut self, cx: &mut Context<Self>) {
        if self.companion_dock.shutting_down {
            return;
        }
        let (Some(core), Some(project)) =
            (self.core.clone(), self.companion_dock.project_id.clone())
        else {
            return;
        };
        let drafts = self.companion_dock.dirty_drafts();
        if drafts.is_empty() {
            return;
        }
        let identities = drafts
            .iter()
            .map(|draft| (draft.topic_id.clone(), draft.revision))
            .collect::<Vec<_>>();
        let io_project = project.clone();
        // Submit I/O now: resetting this project's UI may discard the acknowledgement,
        // but must never discard a final project-switch write before it starts.
        let job = core.runtime_handle().clone().spawn_blocking(move || {
            drafts
                .into_iter()
                .map(|draft| {
                    let result = save_draft_verified(core.persistence(), &io_project, &draft);
                    (draft.topic_id, draft.revision, result)
                })
                .collect::<Vec<_>>()
        });
        let task_key = self.companion_dock.next_write_key("drafts");
        let completion_key = task_key.clone();
        let task = cx.spawn(async move |this, cx| {
            let acknowledgements = match job.await {
                Ok(acknowledgements) => acknowledgements,
                Err(error) => identities
                    .into_iter()
                    .map(|(id, revision)| {
                        (
                            id,
                            revision,
                            Err(format!("Companion draft save interrupted: {error}")),
                        )
                    })
                    .collect(),
            };
            let _ = this.update(cx, |model, cx| {
                for (id, revision, result) in acknowledgements {
                    model
                        .companion_dock
                        .acknowledge_draft(&project, &id, revision, result);
                }
                if model.companion_dock.project_id.as_deref() == Some(project.as_str()) {
                    model.companion_dock.write_tasks.remove(&completion_key);
                }
                cx.notify();
            });
        });
        self.companion_dock.write_tasks.insert(task_key, task);
    }

    pub(crate) fn retry_companion_persistence(&mut self, cx: &mut Context<Self>) {
        self.persist_companion_immediately(cx);
        if self.companion_dock.layout_dirty {
            self.persist_companion_layout(cx);
        }
    }

    pub(crate) fn stage_companion_inputs(&mut self, cx: &mut Context<Self>) {
        if self.chat.read(cx).project_id.as_ref().map(AsRef::as_ref)
            == self.companion_dock.project_id.as_deref()
        {
            let inputs = self
                .chat_composer_inputs
                .iter()
                .map(|(id, input)| (id.clone(), input.read(cx).value().to_owned()))
                .collect::<Vec<_>>();
            for (id, content) in inputs {
                let key = format!("chat:{id}");
                if content.is_empty() && !self.companion_dock.draft_contents.contains_key(&key) {
                    continue;
                }
                self.companion_dock
                    .draft_context
                    .insert(key.clone(), (Some(id), None, None));
                self.companion_dock.record_draft(key, content);
            }
        }
        let forms = self
            .companion_dock
            .forms
            .iter()
            .map(|(id, input)| (id.clone(), input.read(cx).value().to_owned()))
            .collect::<Vec<_>>();
        for (id, content) in forms {
            if content.is_empty() && !self.companion_dock.draft_contents.contains_key(&id) {
                continue;
            }
            self.companion_dock.record_draft(id, content);
        }
    }

    fn capture_companion_shutdown_snapshot(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<ShutdownSnapshot> {
        self.stage_companion_inputs(cx);
        Some(ShutdownSnapshot {
            project: self.companion_dock.project_id.clone()?,
            drafts: self.companion_dock.dirty_drafts(),
            layout_revision: self.companion_dock.layout_revision,
            layout_dirty: self.companion_dock.layout_dirty,
            layout: self.companion_dock.layout(),
        })
    }

    pub(crate) fn flush_companion_for_shutdown(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.companion_dock.shutting_down = true;
        let initial = self.capture_companion_shutdown_snapshot(cx);
        let core = self.core.clone();
        // Start the protective write before GPUI begins its bounded shutdown wait.
        let initial_write = match (core.clone(), initial) {
            (Some(core), Some(snapshot)) => Some(
                core.runtime_handle()
                    .clone()
                    .spawn_blocking(move || persist_shutdown_snapshot(&core, snapshot)),
            ),
            _ => None,
        };
        self.companion_dock.task.take();
        self.companion_dock.decision_tasks.clear();
        let mut actions = self
            .companion_dock
            .save_tasks
            .drain()
            .filter_map(|(key, task)| key.starts_with("action:").then_some(task))
            .collect::<Vec<_>>();
        actions.extend(self.take_chat_admissions_for_shutdown());
        let has_actions = !actions.is_empty();
        // GPUI releases window-owned entities before awaiting quit futures. Keep
        // their admitted callbacks alive through the final persistence snapshot.
        let owner = cx.entity();
        cx.spawn(async move |_, cx| {
            if let Some(write) = initial_write {
                match write.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("Companion shutdown persistence failed: {error}"),
                    Err(error) => eprintln!("Companion shutdown persistence interrupted: {error}"),
                }
            }
            for action in actions {
                action.await;
            }
            if has_actions {
                // Accepted actions clear only the submitted draft. Snapshot after their
                // callbacks, including raw input values whose observers have not run yet.
                let final_snapshot = owner
                    .update(cx, |model, cx| {
                        model.capture_companion_shutdown_snapshot(cx)
                    })
                    .ok()
                    .flatten();
                if let (Some(core), Some(snapshot)) = (core, final_snapshot) {
                    match core
                        .runtime_handle()
                        .clone()
                        .spawn_blocking(move || persist_shutdown_snapshot(&core, snapshot))
                        .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            eprintln!("Companion final shutdown persistence failed: {error}")
                        }
                        Err(error) => {
                            eprintln!("Companion final shutdown persistence interrupted: {error}")
                        }
                    }
                }
            }
        })
    }

    pub(crate) fn open_companion_decision_detail(
        &mut self,
        decision: PendingDecision,
        cx: &mut Context<Self>,
    ) {
        match decision.kind {
            PendingDecisionKind::ForgeQuestions | PendingDecisionKind::Delivery => {
                self.forge.section = if decision.kind == PendingDecisionKind::Delivery {
                    crate::forge::ForgeSection::Delivery
                } else {
                    crate::forge::ForgeSection::Incubation
                };
                if let Some(id) = decision.feature_id {
                    self.forge_action(crate::forge::ForgeAction::SelectFeature(id), cx);
                }
                self.select_view(AppView::Forge, cx);
            }
            PendingDecisionKind::DescriptionReview => {
                if let Some(id) = decision.pulsation_id {
                    self.select_pulsation(id, cx);
                }
                self.pulsations.update(cx, |pulsations, _| {
                    pulsations.detail_section =
                        crate::pulsations::PulsationDetailSection::Preparation
                });
                self.select_view(AppView::Pulsations, cx);
            }
            PendingDecisionKind::CliFallback => {
                let role = self
                    .agents
                    .read(cx)
                    .fallbacks
                    .iter()
                    .find(|fallback| {
                        Some(&fallback.checkpoint_id) == decision.checkpoint_id.as_ref()
                    })
                    .map(|fallback| fallback.agent_role);
                if let Some(role) = role {
                    self.agents.update(cx, |agents, _| {
                        agents.selected_agent =
                            Some(crate::agents::AgentIdentifier::Canonical(role))
                    });
                }
                self.select_view(AppView::Agents, cx);
            }
            _ => {
                if let Some(id) = decision.pulsation_id {
                    self.open_pulsation_review(id, cx);
                } else {
                    self.select_view(AppView::Pulsations, cx);
                }
            }
        }
    }

    pub(crate) fn ensure_companion_form(
        &mut self,
        key: String,
        decision: &PendingDecision,
        placeholder: &str,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.forms.contains_key(&key) {
            return;
        }
        let value = self
            .companion_dock
            .restored_drafts
            .remove(&key)
            .unwrap_or_default();
        let input = cx.new(|cx| {
            let mut input = NativeTextInput::new(placeholder, cx)
                .multiline()
                .max_rows(4);
            input.set_value(value, cx);
            input
        });
        self.companion_dock
            .draft_context
            .entry(key.clone())
            .or_insert_with(|| {
                (
                    None,
                    Some(decision.id.clone()),
                    Some(decision.state_version),
                )
            });
        let observed = key.clone();
        let decision_id = decision.id.clone();
        self.companion_dock
            .subscriptions
            .push(cx.observe(&input, move |model, input, cx| {
                if let Some(current) = model
                    .companion_dock
                    .pending
                    .iter()
                    .find(|decision| decision.id == decision_id)
                {
                    model.companion_dock.draft_context.insert(
                        observed.clone(),
                        (None, Some(decision_id.clone()), Some(current.state_version)),
                    );
                }
                model.companion_save_draft(observed.clone(), input.read(cx).value().to_owned(), cx);
                cx.notify();
            }));
        self.companion_dock.forms.insert(key, input);
    }

    pub(crate) fn invalidate_companion_decisions(&mut self) {
        self.companion_dock.refreshed = Instant::now() - Duration::from_secs(6);
        self.companion_dock.signature = 0;
    }

    pub(crate) fn new_companion_chat(&mut self, cx: &mut Context<Self>) {
        if !self.chat.read(cx).new_conversation_target_available() {
            self.companion_dock.error = Some(
                "Configure a Companion model in the full Chat view before starting a conversation."
                    .into(),
            );
            self.select_view(AppView::Chat, cx);
            return;
        }
        self.companion_dock.new_chat_pending = true;
        self.create_chat_conversation(cx);
    }

    pub(crate) fn companion_chat_creation_finished(
        &mut self,
        conversation: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.accept_created_chat(conversation) {
            self.persist_companion_layout(cx);
        }
    }

    pub(crate) fn ask_companion_about_decision(&mut self, id: String, cx: &mut Context<Self>) {
        let (Some(core), Some(project)) =
            (self.core.clone(), self.companion_dock.project_id.clone())
        else {
            return;
        };
        let runtime = core.runtime_handle().clone();
        let task_key = id.clone();
        let task = cx.spawn(async move |this, cx| {
            let query_id = id.clone();
            let query_project = project.clone();
            let result = runtime
                .spawn_blocking(move || {
                    core.persistence()
                        .ensure_decision_conversation(&query_project, &query_id)
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |model, cx| {
                if model.companion_dock.project_id.as_deref() != Some(&project) {
                    return;
                }
                match result {
                    Ok(conversation) => {
                        model.companion_dock.open_topic(
                            format!("decision:{id}"),
                            Some(conversation.clone()),
                            Some(id),
                        );
                        model.ensure_chat_composer(&conversation, cx);
                        model.select_chat_conversation(conversation, cx);
                        model.persist_companion_layout(cx);
                    }
                    Err(error) => model.companion_dock.error = Some(error),
                }
                cx.notify();
            });
        });
        self.companion_dock.decision_tasks.insert(task_key, task);
    }

    pub(crate) fn use_companion_answer(
        &mut self,
        conversation: &str,
        form: &str,
        cx: &mut Context<Self>,
    ) {
        let text = self
            .chat
            .read(cx)
            .messages_for(conversation)
            .iter()
            .rev()
            .find(|message| message.role == "assistant" && message.status == "completed")
            .map(|message| message.content.clone());
        if let (Some(text), Some(input)) = (text, self.companion_dock.forms.get(form).cloned()) {
            input.update(cx, |input, cx| input.set_value(text, cx));
        }
    }

    pub(crate) fn resolve_companion_decision(
        &mut self,
        decision: PendingDecision,
        action: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.companion_dock.action_busy.contains(&decision.id) {
            return;
        }
        let confirmation = format!("{}:{action}", decision.id);
        if matches!(action, "abort" | "reject" | "grant")
            && self.companion_dock.confirming.as_deref() != Some(&confirmation)
        {
            self.companion_dock.confirming = Some(confirmation);
            cx.notify();
            return;
        }
        self.companion_dock.confirming = None;
        let Some(core) = self.core.clone() else {
            return;
        };
        let feedback = self
            .companion_dock
            .forms
            .get(&format!("feedback:{}", decision.id))
            .map(|input| input.read(cx).value().trim().to_owned())
            .filter(|text| !text.is_empty());
        let answers = decision
            .question_set
            .as_ref()
            .map(|set| {
                set.questions
                    .iter()
                    .map(
                        |question| ikore_core::orchestration::ClarificationAnswerV1 {
                            question_id: question.id.clone(),
                            text_answer: self
                                .companion_dock
                                .forms
                                .get(&format!("answer:{}:{}", decision.id, question.id))
                                .map(|input| input.read(cx).value().to_owned())
                                .unwrap_or_default(),
                            planned_paths: Vec::new(),
                            modified_paths: Vec::new(),
                        },
                    )
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let scope = self
            .pulsations
            .read(cx)
            .failure_checkpoints
            .iter()
            .find(|checkpoint| Some(&checkpoint.checkpoint_id) == decision.checkpoint_id.as_ref())
            .and_then(|checkpoint| checkpoint.scope_request.clone());
        self.companion_dock.action_busy.insert(decision.id.clone());
        let runtime = core.runtime_handle().clone();
        let project = decision.project_id.clone();
        let id = decision.id.clone();
        let task_key = format!("action:{id}");
        let task = cx.spawn(async move |this, cx| {
            let result = runtime.spawn_blocking(move || {
                use ikore_core::persistence_service::{ApplyHitlDecisionRequest, HitlDecision, ResolveStageFailureRequest, StageFailureDecision};
                let invalid = |message: &str| ikore_core::persistence_service::PersistenceServiceError::Validation(message.into());
                let fresh = core.persistence().read_pending_decision(&decision.project_id, &decision.id)?
                    .ok_or_else(|| invalid("This decision has already been resolved."))?;
                if fresh.state_version != decision.state_version || fresh.input_revision != decision.input_revision {
                    return Err(invalid("This decision changed. Review the current card and try again."));
                }
                let checkpoint = decision.checkpoint_id.clone().unwrap_or_default();
                let pulse = decision.pulsation_id.clone().unwrap_or_default();
                match action {
                    "answer" => {
                        let set = decision.question_set.ok_or_else(|| invalid("The questions are no longer available."))?;
                        core.persistence().submit_specification_answers(ikore_core::orchestration::SubmitSpecificationAnswersRequest {
                            pulsation_id: pulse, checkpoint_id: checkpoint, question_set_id: set.id,
                            expected_input_revision: decision.input_revision.unwrap_or_default(), expected_state_version: decision.state_version, answers,
                        }).map(|_| ())
                    }
                    "approve" | "reject" | "revision" if decision.kind == PendingDecisionKind::ArtifactReview => {
                        core.persistence().apply_hitl_decision(ApplyHitlDecisionRequest {
                            pulsation_id: pulse, checkpoint_id: checkpoint, expected_state_version: decision.state_version,
                            feedback, decision: match action { "approve" => HitlDecision::Approve, "reject" => HitlDecision::Reject, _ => HitlDecision::RequestRevision },
                        }).map(|_| ())
                    }
                    "retry" | "revision" | "abort" => core.persistence().resolve_stage_failure(ResolveStageFailureRequest {
                        checkpoint_id: checkpoint, expected_state_version: decision.state_version, feedback,
                        decision: match action { "retry" => StageFailureDecision::RetryStage, "abort" => StageFailureDecision::Abort, _ => StageFailureDecision::RequestRevision },
                    }).map(|_| ()),
                    "grant" => {
                        let scope = scope.ok_or_else(|| invalid("Open the full review to inspect the current scope request."))?;
                        core.persistence().grant_scope_and_retry(ikore_core::orchestration::mandate::ScopeGrantCommandV1 {
                            schema_version: ikore_core::orchestration::mandate::SCOPE_GRANT_COMMAND_VERSION,
                            project_id: decision.project_id, pulsation_id: pulse, checkpoint_id: checkpoint,
                            request_id: scope.request_id, expected_state_version: decision.state_version,
                            expected_derivation_sha256: scope.derivation_sha256,
                        }).map(|_| ())
                    }
                    _ => Err(invalid("Open the full decision for these actions.")),
                }
            }).await.map_err(|error| error.to_string()).and_then(|result| result.map_err(|error| error.to_string()));
            let _ = this.update(cx, |model, cx| {
                if model.companion_dock.project_id.as_deref() != Some(&project) { return; }
                model.companion_dock.action_busy.remove(&id);
                model.companion_dock.error = result.err();
                model.companion_dock.refreshed = Instant::now() - Duration::from_secs(6);
                if !model.companion_dock.shutting_down { model.refresh_pulsations(cx); }
                cx.notify();
            });
        });
        self.companion_dock.save_tasks.insert(task_key, task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_an_empty_form_does_not_schedule_a_draft_write() {
        let mut dock = CompanionDockUi::default();
        assert!(!dock.record_draft("answer:decision:question".into(), String::new()));
        assert!(dock.dirty_drafts().is_empty());
        assert!(dock.record_draft("answer:decision:question".into(), "Chosen answer".into()));
        assert!(dock.record_draft("answer:decision:question".into(), String::new()));
        assert_eq!(
            dock.dirty_drafts()[0].content,
            "",
            "Clearing an edited answer must still create a tombstone"
        );
    }
    use gpui::{Focusable, TestAppContext, px, size};

    struct DockSnapshot(Entity<AppModel>);
    impl gpui::Render for DockSnapshot {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut Context<Self>,
        ) -> impl gpui::IntoElement {
            self.0.update(cx, |model, cx| {
                crate::companion_dock_view::dock(model, window, cx)
            })
        }
    }

    fn assert_dock_element_absent(
        model: &Entity<AppModel>,
        visual: &mut gpui::VisualTestContext,
        selector: &'static str,
    ) {
        // GPUI retains debug selectors across reused frames. Render the current
        // dock in a fresh frame, without mounting the full Chat a second time.
        let (_, snapshot) = visual.add_window_view(|_, _| DockSnapshot(model.clone()));
        assert!(
            snapshot.debug_bounds(selector).is_none(),
            "unexpected {selector}"
        );
    }

    fn dock_fixture(cx: &mut Context<AppModel>) -> AppModel {
        use ikore_core::chat_service::{
            ChatConversation, ChatTransportKind, GetChatConversationResponse,
        };
        let mut model = crate::layout_tests::fixture(cx);
        model.chat.update(cx, |chat, _| {
            chat.begin_hydration("layout-project".into(), None);
            for id in ["dock-a", "dock-b", "dock-c"] {
                chat.store_conversation_snapshot(GetChatConversationResponse {
                    conversation: ChatConversation {
                        id: id.into(),
                        project_id: "layout-project".into(),
                        connection_id: Some("fixture".into()),
                        provider_id: "openai".into(),
                        model_id: "fixture-model".into(),
                        transport: ChatTransportKind::Api,
                        cli_backend: None,
                        model_profile: None,
                        model_source: ikore_core::terminals::CliModelSelectionSource::Default,
                        title: format!("Chat {id}"),
                        connection_available: true,
                        created_at: "now".into(),
                        updated_at: "now".into(),
                    },
                    messages: Vec::new(),
                    tool_calls: Vec::new(),
                });
                chat.acknowledge_disclosure_for(id);
            }
            chat.select_conversation_locally("dock-c");
            chat.finish_hydration_without_backend();
        });
        model.companion_dock.project_id = Some("layout-project".into());
        model.companion_dock.decisions_initialized = true;
        for id in ["dock-a", "dock-b", "dock-c"] {
            model
                .companion_dock
                .open_topic(format!("chat:{id}"), Some(id.into()), None);
            model.ensure_chat_composer(id, cx);
        }
        model
    }

    fn decision_fixture() -> PendingDecision {
        PendingDecision {
            id: "checkpoint:fixture".into(),
            project_id: "layout-project".into(),
            kind: PendingDecisionKind::StageFailure,
            source_id: "fixture".into(),
            pulsation_id: Some("pulse-a".into()),
            checkpoint_id: Some("fixture".into()),
            feature_id: None,
            input_revision: Some(1),
            state_version: 4,
            stage: Some("code".into()),
            title: "Fixture decision".into(),
            description: "Review this fixture failure.".into(),
            actions: vec![
                ikore_core::orchestration::decisions::PendingDecisionAction::OpenFullReview,
            ],
            question_set: None,
            forge_questions: None,
        }
    }

    #[gpui::test]
    fn a_late_draft_restore_updates_an_existing_composer_while_the_dock_is_closed(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(dock_fixture);
        model.update(cx, |model, cx| {
            model.companion_dock.open = false;
            model.companion_dock.windows.clear();
            model
                .companion_dock
                .restored_drafts
                .insert("dock-b".into(), "restored after hydration".into());
            model.observe_companion_dock(cx);
            assert_eq!(
                model.chat_composer_for("dock-b").unwrap().read(cx).value(),
                "restored after hydration"
            );
            assert!(!model.companion_dock.restored_drafts.contains_key("dock-b"));
            assert!(!model.companion_dock.open);
        });
    }

    #[gpui::test]
    fn a_closed_dock_with_many_conversations_renders_only_its_launcher_until_clicked(
        cx: &mut TestAppContext,
    ) {
        let (model, visual) = cx.add_window_view(|_, cx| {
            let mut model = dock_fixture(cx);
            model.chat.update(cx, |chat, _| {
                let template = chat.conversations[0].clone();
                for index in 0..256 {
                    let mut conversation = template.clone();
                    conversation.id = format!("archived-{index}");
                    conversation.title = format!("Archived conversation {index}");
                    chat.conversations.push(conversation);
                }
            });
            for index in 0..256 {
                model.ensure_chat_composer(&format!("archived-{index}"), cx);
            }
            model.companion_dock.open = false;
            model.companion_dock.windows.clear();
            model
        });
        visual.simulate_resize(size(px(960.0), px(640.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("companion-dock-history").is_none());
        assert!(visual.debug_bounds("dock-topic-chat:dock-c").is_none());
        let launcher = visual.debug_bounds("companion-dock").unwrap();
        visual.simulate_mouse_down(
            launcher.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        visual.simulate_mouse_up(
            launcher.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        visual.run_until_parked();
        assert!(visual.debug_bounds("companion-dock-history").is_some());
        visual.simulate_keystrokes("space");
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("companion-dock-history").is_some(),
            "A keyboard press activates on release, not twice on down and up"
        );
        visual.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("companion-dock-history").is_none(),
            "The focused launcher must be keyboard-operable"
        );
        model.read_with(visual, |model, cx| {
            assert!(!model.companion_dock.open);
            assert_eq!(model.chat.read(cx).conversations.len(), 259);
        });
    }

    #[gpui::test]
    fn native_dock_windows_fit_small_and_large_viewports(cx: &mut TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| dock_fixture(cx));
        for (width, height) in [(960.0, 640.0), (1440.0, 900.0), (800.0, 640.0)] {
            visual.simulate_resize(size(px(width), px(height)));
            visual.run_until_parked();
            let windows = model.read_with(visual, |model, _| {
                model.companion_dock.visible_windows(width)
            });
            assert_eq!(windows.len(), window_capacity(width));
            for window in windows {
                let selector = match window.key.as_str() {
                    "chat:dock-a" => "dock-topic-chat:dock-a",
                    "chat:dock-b" => "dock-topic-chat:dock-b",
                    "chat:dock-c" => "dock-topic-chat:dock-c",
                    _ => panic!("unexpected fixture topic"),
                };
                let bounds = visual.debug_bounds(selector).unwrap();
                assert!(
                    bounds.left() >= px(0.0) && bounds.right() <= px(width),
                    "{bounds:?}"
                );
                assert!(
                    bounds.top() >= px(0.0) && bounds.bottom() <= px(height),
                    "{bounds:?}"
                );
            }
        }
    }

    #[gpui::test]
    fn the_full_chat_owns_its_composer_geometry_and_expand_minimizes_its_topic(
        cx: &mut TestAppContext,
    ) {
        let (model, visual) = cx.add_window_view(|_, cx| {
            let mut model = dock_fixture(cx);
            model
                .companion_dock
                .windows
                .retain(|window| window.conversation_id.as_deref() == Some("dock-c"));
            model
                .chat_composer_for("dock-c")
                .unwrap()
                .update(cx, |input, cx| {
                    input.set_value("First line\nSecond line · 2026-11-07", cx);
                });
            model
        });
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("dock-composer-dock-c").is_some());
        model.update(visual, |model, cx| {
            model.select_chat_conversation("dock-c".into(), cx);
            model.select_view(AppView::Chat, cx);
            assert_eq!(model.active_view(), AppView::Chat);
            assert_eq!(
                model
                    .chat
                    .read(cx)
                    .active_conversation
                    .as_ref()
                    .map(AsRef::as_ref),
                Some("dock-c")
            );
        });
        visual.run_until_parked();
        assert!(visual.debug_bounds("dock-topic-chat:dock-c").is_some());
        assert_dock_element_absent(&model, visual, "dock-composer-dock-c");
        assert!(visual.debug_bounds("dock-continue-dock-c").is_some());
        let composer = visual.debug_bounds("chat-composer").unwrap();
        let input = visual
            .debug_bounds("native-input:Message to Companion")
            .unwrap();
        assert!(input.left() >= composer.left() && input.right() <= composer.right());
        assert!(input.top() >= composer.top() && input.bottom() <= composer.bottom());
        assert!(
            input.size.height >= px(48.0),
            "the full composer must retain both lines: {input:?}"
        );
        model.update(visual, |model, cx| {
            model.select_chat_conversation("dock-b".into(), cx);
        });
        visual.run_until_parked();
        assert!(
            visual.debug_bounds("dock-composer-dock-c").is_some(),
            "another full Chat conversation can coexist with this dock input"
        );
        model.update(visual, |model, cx| {
            model.expand_companion_chat("dock-c".into(), cx);
            assert_eq!(
                model.chat_draft_input.entity_id(),
                model.chat_composer_for("dock-c").unwrap().entity_id()
            );
            assert_eq!(
                model.chat_draft_input.read(cx).value(),
                "First line\nSecond line · 2026-11-07"
            );
            assert!(
                model
                    .companion_dock
                    .windows
                    .iter()
                    .find(|window| window.key == "chat:dock-c")
                    .unwrap()
                    .minimized
            );
        });
        visual.run_until_parked();
        assert_dock_element_absent(&model, visual, "dock-topic-chat:dock-c");
        assert!(visual.debug_bounds("chat-composer").is_some());
    }

    #[gpui::test]
    fn history_reopens_only_minimized_or_overflow_topics(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, cx| {
            let mut model = dock_fixture(cx);
            model.companion_dock.minimize("chat:dock-a");
            model.companion_dock.history_open = true;
            model
        });
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("dock-restore-chat:dock-a").is_some());
        assert!(visual.debug_bounds("dock-topic-chat:dock-b").is_some());
        assert!(visual.debug_bounds("dock-topic-chat:dock-c").is_some());
        assert!(visual.debug_bounds("dock-restore-chat:dock-b").is_none());
        assert!(visual.debug_bounds("dock-restore-chat:dock-c").is_none());
        visual.simulate_resize(size(px(800.0), px(640.0)));
        visual.run_until_parked();
        assert!(visual.debug_bounds("dock-restore-chat:dock-b").is_some());
        assert!(visual.debug_bounds("dock-restore-chat:dock-c").is_none());
    }

    #[gpui::test]
    fn conversation_aliases_reuse_the_decision_window_and_its_composer(cx: &mut TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| {
            let mut model = dock_fixture(cx);
            model.companion_dock.windows.clear();
            let decision = decision_fixture();
            model.companion_dock.pending.push(decision.clone());
            model.companion_dock.open_topic(
                format!("decision:{}", decision.id),
                Some("dock-c".into()),
                Some(decision.id.clone()),
            );
            let composer = model.chat_composer_for("dock-c").unwrap();
            composer.update(cx, |input, cx| {
                input.set_value("Unsent conversation text", cx)
            });
            let feedback = format!("feedback:{}", decision.id);
            model.ensure_companion_form(feedback.clone(), &decision, "Feedback", cx);
            model.companion_dock.forms[&feedback]
                .update(cx, |input, cx| input.set_value("Unsent decision text", cx));
            let form_id = model.companion_dock.forms[&feedback].entity_id();
            model.open_companion_chat("dock-c".into(), cx);
            assert_eq!(model.companion_dock.windows.len(), 1);
            let topic = &model.companion_dock.windows[0];
            assert_eq!(topic.key, format!("decision:{}", decision.id));
            assert_eq!(topic.decision_id.as_deref(), Some(decision.id.as_str()));
            assert_eq!(
                model.chat_composer_for("dock-c").unwrap().entity_id(),
                composer.entity_id()
            );
            assert_eq!(model.companion_dock.forms[&feedback].entity_id(), form_id);
            assert_eq!(
                model.companion_dock.pending[0].state_version,
                decision.state_version
            );
            model.stage_companion_inputs(cx);
            assert_eq!(
                model.companion_dock.draft_contents["chat:dock-c"],
                "Unsent conversation text"
            );
            assert_eq!(
                model.companion_dock.draft_contents[&feedback],
                "Unsent decision text"
            );
            model
        });
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("dock-topic-decision:checkpoint:fixture")
                .is_some()
        );
        assert!(visual.debug_bounds("dock-composer-dock-c").is_some());
        assert_dock_element_absent(&model, visual, "dock-topic-chat:dock-c");
    }

    #[test]
    fn conversation_aliases_merge_after_late_binding_without_merging_decisions() {
        let mut dock = CompanionDockUi::default();
        dock.open_topic("chat:shared".into(), Some("shared".into()), None);
        dock.open_topic("decision:first".into(), None, Some("first".into()));
        dock.open_topic(
            "decision:first".into(),
            Some("shared".into()),
            Some("first".into()),
        );
        assert_eq!(dock.windows.len(), 1);
        assert_eq!(dock.windows[0].key, "decision:first");
        assert_eq!(dock.windows[0].decision_id.as_deref(), Some("first"));
        assert_eq!(dock.windows[0].conversation_id.as_deref(), Some("shared"));
        dock.open_topic(
            "decision:second".into(),
            Some("shared".into()),
            Some("second".into()),
        );
        assert_eq!(
            dock.windows.len(),
            2,
            "distinct decisions retain their own guarded cards"
        );
        assert_eq!(dock.windows[0].decision_id.as_deref(), Some("first"));
        assert_eq!(dock.windows[1].decision_id.as_deref(), Some("second"));
    }

    #[test]
    fn conversation_aliases_restore_one_canonical_decision_without_rebinding_drafts() {
        for decision_first in [false, true] {
            let chat = CompanionDockItem {
                topic_id: "chat:shared".into(),
                conversation_id: Some("shared".into()),
                decision_id: None,
                minimized: false,
            };
            let decision = CompanionDockItem {
                topic_id: "decision:checkpoint:first".into(),
                conversation_id: Some("shared".into()),
                decision_id: Some("checkpoint:first".into()),
                minimized: false,
            };
            let mut conversations = if decision_first {
                vec![decision, chat]
            } else {
                vec![chat, decision]
            };
            conversations.push(CompanionDockItem {
                topic_id: "chat:other".into(),
                conversation_id: Some("other".into()),
                decision_id: None,
                minimized: false,
            });
            let mut dock = CompanionDockUi {
                layout_revision: 9,
                ..Default::default()
            };
            dock.record_draft("chat:shared".into(), "Unsent message".into());
            dock.record_draft("feedback:checkpoint:first".into(), "Unsent feedback".into());
            dock.draft_context.insert(
                "feedback:checkpoint:first".into(),
                (None, Some("checkpoint:first".into()), Some(4)),
            );
            dock.restored_drafts
                .insert("shared".into(), "Unsent message".into());
            dock.restore_layout(CompanionDockLayout {
                open: true,
                conversations,
            });
            assert_eq!(dock.windows.len(), 2);
            let topic = dock
                .windows
                .iter()
                .find(|topic| topic.conversation_id.as_deref() == Some("shared"))
                .unwrap();
            assert_eq!(topic.key, "decision:checkpoint:first");
            assert_eq!(topic.decision_id.as_deref(), Some("checkpoint:first"));
            assert!(
                topic.minimized,
                "restoring an alias must not auto-open a decision"
            );
            assert!(
                dock.open,
                "the other ordinary chat keeps its restored visibility"
            );
            assert_eq!(dock.visible_windows(1440.0)[0].key, "chat:other");
            assert_eq!(dock.draft_contents["chat:shared"], "Unsent message");
            assert_eq!(
                dock.draft_contents["feedback:checkpoint:first"],
                "Unsent feedback"
            );
            assert_eq!(
                dock.draft_context["feedback:checkpoint:first"],
                (None, Some("checkpoint:first".into()), Some(4))
            );
            assert_eq!(dock.restored_drafts["shared"], "Unsent message");
            assert_eq!(dock.layout_revision, 9);
            assert!(!dock.layout_dirty);
            let saved = dock.layout();
            assert_eq!(saved.conversations.len(), 2);
            assert!(
                !saved
                    .conversations
                    .iter()
                    .any(|topic| topic.topic_id == "chat:shared")
            );
            dock.open_topic("chat:shared".into(), Some("shared".into()), None);
            assert_eq!(dock.windows.len(), 2);
            assert_eq!(
                dock.visible_windows(1440.0)[0].key,
                "decision:checkpoint:first"
            );
        }
    }

    #[gpui::test]
    fn dock_focus_blocks_primary_flow_and_survives_a_new_decision(cx: &mut TestAppContext) {
        cx.update(crate::shell::register_navigation_keybindings);
        cx.update(crate::text_input::register_text_input_keybindings);
        let (model, visual) = cx.add_window_view(|window, cx| {
            let model = dock_fixture(cx);
            model
                .chat_composer_for("dock-c")
                .unwrap()
                .read(cx)
                .focus_handle(cx)
                .focus(window);
            model
        });
        visual.simulate_resize(size(px(960.0), px(640.0)));
        visual.simulate_input("before");
        visual.run_until_parked();
        visual.simulate_keystrokes("ctrl-enter");
        visual.run_until_parked();
        model.update(visual, |model, cx| {
            assert_eq!(
                model.active_view(),
                AppView::Home,
                "Ctrl+Enter must not execute the primary flow behind the dock"
            );
            let decision = decision_fixture();
            assert!(model.companion_dock.should_auto_open(&decision.id));
            model.companion_dock.open_topic(
                format!("decision:{}", decision.id),
                None,
                Some(decision.id.clone()),
            );
            model.companion_dock.pending.push(decision);
            cx.notify();
        });
        visual.run_until_parked();
        visual.simulate_input(" after");
        visual.run_until_parked();
        model.read_with(visual, |model, cx| {
            assert_eq!(
                model.chat_composer_for("dock-c").unwrap().read(cx).value(),
                "before after"
            );
            assert_eq!(
                model.chat_draft_input.entity_id(),
                model.chat_composer_for("dock-c").unwrap().entity_id()
            );
        });
    }

    #[gpui::test]
    fn dock_shortcut_reopens_after_hiding_its_focused_history_button_or_composer(
        cx: &mut TestAppContext,
    ) {
        cx.update(crate::shell::register_navigation_keybindings);
        cx.update(crate::text_input::register_text_input_keybindings);
        let mut failures = Vec::new();
        for focus_button in [true, false] {
            let (model, visual) = cx.add_window_view(|_, cx| {
                let mut model = dock_fixture(cx);
                model.companion_dock.history_open = true;
                model
                    .chat_composer_for("dock-c")
                    .unwrap()
                    .update(cx, |input, cx| {
                        input.set_value("Unsent UTF-8 · a\u{e7}\u{e3}o", cx)
                    });
                model
            });
            visual.update(|window, _| window.activate_window());
            visual.simulate_resize(size(px(1440.0), px(900.0)));
            visual.run_until_parked();
            if focus_button {
                let button = visual.debug_bounds("dock-history-dock-c").unwrap();
                visual.simulate_click(button.center(), gpui::Modifiers::default());
            } else {
                let input = model.read_with(visual, |model, _| {
                    model.chat_composer_for("dock-c").unwrap()
                });
                visual.update(|window, cx| input.read(cx).focus_handle(cx).focus(window));
            }
            visual.run_until_parked();
            let before = visual.update(|window, cx| (window.focused(cx), window.context_stack()));
            assert!(before.0.is_some());
            assert!(
                before
                    .1
                    .iter()
                    .any(|context| context.contains("CompanionDock")),
                "{before:?}"
            );
            visual.simulate_keystrokes("ctrl-shift-c");
            visual.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("ctrl-shift-c").unwrap(),
            });
            visual.run_until_parked();
            assert!(!model.read_with(visual, |model, _| model.companion_dock.open));
            let closed = visual.update(|window, cx| (window.focused(cx), window.context_stack()));
            visual.simulate_keystrokes("ctrl-shift-c");
            visual.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("ctrl-shift-c").unwrap(),
            });
            visual.run_until_parked();
            let reopened = visual.update(|window, cx| (window.focused(cx), window.context_stack()));
            if !model.read_with(visual, |model, _| model.companion_dock.open)
                || !closed.1.iter().any(|context| context.contains("Shell"))
            {
                failures.push(format!("must reopen without an intervening click and keep a mounted focus path; button={focus_button}, before={before:?}, closed={closed:?}, reopened={reopened:?}"));
            }
            model.read_with(visual, |model, cx| {
                assert_eq!(
                    model.chat_composer_for("dock-c").unwrap().read(cx).value(),
                    "Unsent UTF-8 · a\u{e7}\u{e3}o"
                );
                assert_eq!(model.active_view(), AppView::Home);
            });
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[gpui::test]
    fn dock_shortcut_keeps_focus_in_the_full_chat_when_hiding_the_dock(cx: &mut TestAppContext) {
        cx.update(crate::shell::register_navigation_keybindings);
        cx.update(crate::text_input::register_text_input_keybindings);
        let (model, visual) = cx.add_window_view(|_, cx| {
            let mut model = dock_fixture(cx);
            model
                .companion_dock
                .windows
                .retain(|topic| topic.conversation_id.as_deref() == Some("dock-c"));
            model.select_chat_conversation("dock-b".into(), cx);
            model.select_view(AppView::Chat, cx);
            model
        });
        visual.update(|window, _| window.activate_window());
        visual.simulate_resize(size(px(1440.0), px(900.0)));
        visual.run_until_parked();
        let input = model.read_with(visual, |model, _| {
            model.chat_composer_for("dock-b").unwrap()
        });
        let focus = visual.update(|window, cx| {
            let focus = input.read(cx).focus_handle(cx);
            focus.focus(window);
            focus
        });
        for expected_open in [false, true] {
            visual.simulate_keystrokes("ctrl-shift-c");
            visual.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("ctrl-shift-c").unwrap(),
            });
            visual.run_until_parked();
            assert_eq!(
                model.read_with(visual, |model, _| model.companion_dock.open),
                expected_open
            );
            visual.update(|window, _| {
                assert!(
                    focus.is_focused(window),
                    "a full Chat input must keep focus"
                )
            });
        }
    }

    #[gpui::test]
    fn dock_shortcut_remains_available_after_hiding_its_focused_topic(cx: &mut TestAppContext) {
        cx.update(crate::shell::register_navigation_keybindings);
        let mut failures = Vec::new();
        for minimize in [true, false] {
            let (model, visual) = cx.add_window_view(|_, cx| {
                let mut model = dock_fixture(cx);
                model
                    .companion_dock
                    .windows
                    .retain(|topic| topic.conversation_id.as_deref() == Some("dock-c"));
                model
            });
            visual.update(|window, _| window.activate_window());
            visual.simulate_resize(size(px(1440.0), px(900.0)));
            visual.run_until_parked();
            let button = visual
                .debug_bounds(if minimize {
                    "dock-minimize-chat:dock-c"
                } else {
                    "dock-close-chat:dock-c"
                })
                .unwrap();
            visual.simulate_click(button.center(), gpui::Modifiers::default());
            visual.run_until_parked();
            let hidden = visual.update(|window, cx| (window.focused(cx), window.context_stack()));
            assert!(model.read_with(visual, |model, _| model.companion_dock.open));
            visual.simulate_keystrokes("ctrl-shift-c");
            visual.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("ctrl-shift-c").unwrap(),
            });
            visual.run_until_parked();
            if model.read_with(visual, |model, _| model.companion_dock.open) {
                failures.push(format!("shortcut must close after hiding focused topic; minimize={minimize}, focus={hidden:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[gpui::test]
    fn dock_shortcut_recovers_when_async_controls_are_replaced(cx: &mut TestAppContext) {
        assert_shortcut_after_async_replacement(cx, false, false);
    }

    #[gpui::test]
    fn dock_shortcut_recovers_when_async_controls_are_replaced_while_inactive(
        cx: &mut TestAppContext,
    ) {
        assert_shortcut_after_async_replacement(cx, true, false);
    }

    #[gpui::test]
    fn dock_shortcut_does_not_reclaim_blurred_async_controls_after_reactivation(
        cx: &mut TestAppContext,
    ) {
        assert_shortcut_after_async_replacement(cx, true, true);
    }

    fn assert_shortcut_after_async_replacement(
        cx: &mut TestAppContext,
        inactive: bool,
        explicit_blur: bool,
    ) {
        cx.update(crate::shell::register_navigation_keybindings);
        let mut failures = Vec::new();
        for ask_companion in [true, false] {
            let (model, visual) = cx.add_window_view(|_, cx| {
                let mut model = dock_fixture(cx);
                model.companion_dock.windows.clear();
                if ask_companion {
                    let decision = decision_fixture();
                    model.companion_dock.pending.push(decision.clone());
                    model.companion_dock.open_topic(
                        format!("decision:{}", decision.id),
                        None,
                        Some(decision.id),
                    );
                } else {
                    model.companion_dock.open_topic(
                        "chat:dock-c".into(),
                        Some("dock-c".into()),
                        None,
                    );
                    model
                        .chat_composer_for("dock-c")
                        .unwrap()
                        .update(cx, |input, cx| input.set_value("Fixture message", cx));
                }
                model
            });
            visual.update(|window, _| window.activate_window());
            visual.simulate_resize(size(px(1440.0), px(900.0)));
            visual.run_until_parked();
            let button = visual
                .debug_bounds(if ask_companion {
                    "dock-help-checkpoint:fixture"
                } else {
                    "dock-send-dock-c"
                })
                .unwrap();
            visual.simulate_click(button.center(), gpui::Modifiers::default());
            visual.run_until_parked();
            let mut before =
                visual.update(|window, cx| (window.focused(cx), window.context_stack()));
            assert!(before.0.is_some());
            if explicit_blur {
                before.0.take();
            }
            if inactive {
                visual.deactivate_window();
            }
            visual.update(|window, cx| {
                if explicit_blur {
                    window.blur();
                }
                model.update(cx, |model, cx| {
                    if ask_companion {
                        model.companion_dock.open_topic(
                            "decision:checkpoint:fixture".into(),
                            Some("dock-c".into()),
                            Some("checkpoint:fixture".into()),
                        );
                    } else {
                        model.chat.update(cx, |chat, _| {
                            let conversation = chat
                                .conversations
                                .iter()
                                .find(|conversation| conversation.id == "dock-c")
                                .unwrap()
                                .clone();
                            chat.store_conversation_snapshot(
                                ikore_core::chat_service::GetChatConversationResponse {
                                    conversation,
                                    messages: vec![ikore_core::chat_service::ChatMessage {
                                        id: "fixture-stream".into(),
                                        conversation_id: "dock-c".into(),
                                        ordinal: 1,
                                        role: "assistant".into(),
                                        content: "Fixture response".into(),
                                        status: "streaming".into(),
                                        execution_id: Some("fixture-execution".into()),
                                        generation_id: Some("fixture-generation".into()),
                                        last_sequence: 1,
                                        input_tokens: None,
                                        output_tokens: None,
                                        estimated_cost_usd: None,
                                        error_code: None,
                                        error_message: None,
                                        target_label: None,
                                        model_evidence: None,
                                        created_at: "now".into(),
                                        updated_at: "now".into(),
                                    }],
                                    tool_calls: Vec::new(),
                                },
                            );
                        });
                    }
                    cx.notify();
                });
            });
            visual.run_until_parked();
            assert!(
                visual
                    .debug_bounds(if ask_companion {
                        "dock-composer-dock-c"
                    } else {
                        "dock-cancel-dock-c"
                    })
                    .is_some()
            );
            if inactive {
                visual.update(|window, _| {
                    assert!(
                        !window.is_window_active(),
                        "recovery must not activate the window"
                    )
                });
                visual.update(|window, _| window.activate_window());
                visual.run_until_parked();
            }
            if explicit_blur {
                visual.update(|window, cx| {
                    assert!(
                        window.focused(cx).is_none(),
                        "reactivation must preserve explicit blur after Ask/Send removal"
                    )
                });
                continue;
            }
            let replaced = visual.update(|window, cx| (window.focused(cx), window.context_stack()));
            visual.simulate_keystrokes("ctrl-shift-c");
            visual.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("ctrl-shift-c").unwrap(),
            });
            visual.run_until_parked();
            if model.read_with(visual, |model, _| model.companion_dock.open) {
                failures.push(format!("shortcut must work after replacing a focused control; ask={ask_companion}, inactive={inactive}, before={before:?}, replaced={replaced:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[gpui::test]
    fn dock_shortcut_does_not_reclaim_an_explicitly_blurred_input(cx: &mut TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| dock_fixture(cx));
        visual.update(|window, _| window.activate_window());
        let input = model.read_with(visual, |model, _| {
            model.chat_composer_for("dock-c").unwrap()
        });
        visual.update(|window, cx| input.read(cx).focus_handle(cx).focus(window));
        visual.run_until_parked();
        visual.update(|window, _| window.blur());
        visual.run_until_parked();
        visual.update(|window, cx| assert!(window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn obsolete_draft_is_preserved_with_a_visible_context_warning(cx: &mut TestAppContext) {
        let (model, visual) = cx.add_window_view(|_, cx| dock_fixture(cx));
        model.update(visual, |model, cx| {
            let decision = decision_fixture();
            let key = format!("feedback:{}", decision.id);
            model
                .companion_dock
                .draft_context
                .insert(key.clone(), (None, Some(decision.id.clone()), Some(3)));
            model
                .companion_dock
                .restored_drafts
                .insert(key.clone(), "saved explanation".into());
            model.companion_dock.pending.push(decision.clone());
            model.ensure_companion_form(key.clone(), &decision, "Feedback", cx);
            model.companion_dock.open_topic(
                format!("decision:{}", decision.id),
                None,
                Some(decision.id),
            );
            assert_eq!(
                model.companion_dock.forms[&key].read(cx).value(),
                "saved explanation"
            );
            cx.notify();
        });
        visual.run_until_parked();
        assert!(
            visual
                .debug_bounds("dock-stale-draft-feedback:checkpoint:fixture")
                .is_some()
        );
    }

    #[gpui::test]
    async fn shutdown_flush_waits_for_admission_before_saving_accepted_answer_tombstones(
        cx: &mut TestAppContext,
    ) {
        use ikore_core::orchestration::decisions::PendingForgeQuestions;
        use ikore_core::persistence_service::EnsureProjectForWorkspaceRequest;
        cx.executor().allow_parking();
        for (accepted, edited) in [(true, false), (true, true), (false, false)] {
            let harness = ikore_test_support::CoreTestHarness::new();
            let core = harness.core();
            let project = core
                .persistence()
                .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                    workspace_path: harness.workspace_dir().to_string_lossy().into_owned(),
                    preferred_name: None,
                })
                .unwrap()
                .project
                .id;
            let mut decision = decision_fixture();
            decision.project_id = project.clone();
            decision.id = "forge:shutdown-answer".into();
            decision.kind = PendingDecisionKind::ForgeQuestions;
            decision.feature_id = Some("fixture-feature".into());
            decision.forge_questions = Some(PendingForgeQuestions {
                feature_id: "fixture-feature".into(),
                feature_revision: 1,
                turn_id: "fixture-turn".into(),
                questions: vec!["Which format?".into(), "Who reads it?".into()],
                question_details: Vec::new(),
            });
            let first_key = format!("forge-answer:{}:0", decision.id);
            let second_key = format!("forge-answer:{}:1", decision.id);
            let model = cx.new(AppModel::new_for_test);
            model.update(cx, |model, cx| {
                model.core = Some(core.clone());
                model.workspace.update(cx, |workspace, _| {
                    workspace.project_id = Some(project.clone().into())
                });
                model.companion_dock.project_id = Some(project.clone());
                model.companion_dock.pending.push(decision.clone());
                model.ensure_companion_forge_forms(&decision, cx);
                model.companion_dock.forms[&first_key]
                    .update(cx, |input, cx| input.set_value("JSON", cx));
                model.companion_dock.forms[&second_key]
                    .update(cx, |input, cx| input.set_value("Accounting", cx));
                model.stage_companion_inputs(cx);
                for draft in model.companion_dock.dirty_drafts() {
                    save_draft_verified(core.persistence(), &project, &draft).unwrap();
                }
            });
            let (release, barrier) = tokio::sync::oneshot::channel();
            let started = std::rc::Rc::new(std::cell::Cell::new(false));
            let callback_finished = std::rc::Rc::new(std::cell::Cell::new(false));
            model.update(cx, |model, cx| {
                let project = project.clone();
                let id = decision.id.clone();
                let sent = vec![
                    (first_key.clone(), "JSON".into()),
                    (second_key.clone(), "Accounting".into()),
                ];
                let started = started.clone();
                let callback_finished = callback_finished.clone();
                model.companion_dock.action_busy.insert(id.clone());
                let action = cx.spawn(async move |this, cx| {
                    started.set(true);
                    barrier.await.unwrap();
                    this.update(cx, |model, cx| {
                        model.finish_companion_forge_admission(
                            &project,
                            &id,
                            sent,
                            if accepted {
                                Ok(())
                            } else {
                                Err("Fixture admission refused".into())
                            },
                            cx,
                        );
                        assert!(model.companion_dock.shutting_down);
                        assert!(model.companion_dock.action_busy.is_empty());
                        assert!(
                            model.companion_dock.save_tasks.is_empty(),
                            "shutdown callbacks must not schedule another debounce"
                        );
                        assert!(
                            model.forge.project_id.is_none(),
                            "shutdown callbacks must not start a Forge refresh"
                        );
                        callback_finished.set(true);
                    })
                    .expect("shutdown must retain the model for admitted callbacks");
                });
                model
                    .companion_dock
                    .save_tasks
                    .insert("action:forge:shutdown-answer".into(), action);
            });
            cx.run_until_parked();
            assert!(
                started.get(),
                "the admission must already be waiting when shutdown begins"
            );
            if edited {
                model.update(cx, |model, cx| {
                    model.companion_dock.forms[&first_key].update(cx, |input, cx| {
                        input.set_value("A newer unsent question", cx)
                    })
                });
            }
            let flush = model.update(cx, |model, cx| model.flush_companion_for_shutdown(cx));
            drop(model);
            release.send(()).unwrap();
            flush.await;
            assert!(callback_finished.get());
            let snapshot = core.persistence().load_companion_dock(&project).unwrap();
            let first = snapshot
                .drafts
                .iter()
                .find(|draft| draft.topic_id == first_key)
                .unwrap();
            let second = snapshot
                .drafts
                .iter()
                .find(|draft| draft.topic_id == second_key)
                .unwrap();
            assert_eq!(
                first.content,
                if edited {
                    "A newer unsent question"
                } else if accepted {
                    ""
                } else {
                    "JSON"
                }
            );
            assert_eq!(second.content, if accepted { "" } else { "Accounting" });
        }
        cx.executor().forbid_parking();
    }

    #[gpui::test]
    fn shutdown_stages_latest_composer_values_before_observer_delivery(cx: &mut TestAppContext) {
        let model = cx.new(dock_fixture);
        model.update(cx, |model, cx| {
            let input = model.chat_composer_for("dock-c").unwrap();
            input.update(cx, |input, cx| input.set_value("latest unsent value", cx));
            model.stage_companion_inputs(cx);
            let draft = model
                .companion_dock
                .dirty_drafts()
                .into_iter()
                .find(|draft| draft.conversation_id.as_deref() == Some("dock-c"))
                .unwrap();
            assert_eq!(draft.content, "latest unsent value");
            assert!(draft.revision > 0);
        });
    }

    #[test]
    fn dock_capacity_tracks_available_width() {
        assert_eq!(window_capacity(800.0), 1);
        assert_eq!(window_capacity(960.0), 2);
        assert_eq!(window_capacity(1440.0), 3);
    }

    #[test]
    fn closed_and_minimized_topics_keep_identity_and_draft_binding() {
        let mut dock = CompanionDockUi::default();
        dock.open_topic("chat:a".into(), Some("a".into()), None);
        dock.minimize("chat:a");
        assert!(dock.windows[0].minimized);
        dock.close("chat:a");
        assert!(dock.windows.is_empty());
        dock.open_topic("chat:a".into(), Some("a".into()), None);
        assert_eq!(dock.windows[0].conversation_id.as_deref(), Some("a"));
    }

    #[test]
    fn overflow_does_not_remove_other_topics() {
        let mut dock = CompanionDockUi::default();
        for id in ["a", "b", "c", "d"] {
            dock.open_topic(id.into(), Some(id.into()), None);
        }
        assert_eq!(dock.visible_windows(960.0).len(), 2);
        assert_eq!(dock.windows.len(), 4);
        dock.minimize("d");
        assert_eq!(dock.visible_windows(960.0).len(), 2);
    }

    #[test]
    fn reopening_an_overflow_topic_makes_it_visible() {
        let mut dock = CompanionDockUi::default();
        for id in ["a", "b", "c", "d"] {
            dock.open_topic(id.into(), Some(id.into()), None);
        }
        assert!(
            !dock
                .visible_windows(960.0)
                .iter()
                .any(|window| window.key == "a")
        );
        dock.open_topic("a".into(), Some("a".into()), None);
        assert_eq!(dock.visible_windows(960.0)[0].key, "a");
        assert_eq!(dock.windows.len(), 4);
    }

    #[test]
    fn a_new_chat_opens_only_from_its_creation_receipt() {
        let mut dock = CompanionDockUi::default();
        dock.open_topic("chat:old".into(), Some("old".into()), None);
        dock.new_chat_pending = true;
        assert!(dock.accept_created_chat(Some("new".into())));
        assert_eq!(
            dock.visible_windows(960.0)[0].conversation_id.as_deref(),
            Some("new")
        );
        assert!(!dock.new_chat_pending);
        assert!(!dock.accept_created_chat(Some("unrelated".into())));
        dock.new_chat_pending = true;
        assert!(!dock.accept_created_chat(None));
        assert!(!dock.new_chat_pending);
        assert_eq!(dock.windows.len(), 2);
    }

    #[test]
    fn restored_decision_drafts_keep_their_old_version_until_edited() {
        let mut dock = CompanionDockUi::default();
        dock.draft_context.insert(
            "answer:decision:q".into(),
            (None, Some("decision".into()), Some(3)),
        );
        assert!(!dock.form_draft_is_stale("answer:decision:q", 3));
        assert!(dock.form_draft_is_stale("answer:decision:q", 4));
        assert_eq!(dock.draft_context["answer:decision:q"].2, Some(3));
    }

    #[test]
    fn persistence_acknowledgements_clear_only_the_same_project_and_revision() {
        let mut dock = CompanionDockUi {
            project_id: Some("project-a".into()),
            ..Default::default()
        };
        dock.record_draft("chat:a".into(), "first".into());
        dock.record_draft("chat:b".into(), "other".into());
        dock.record_draft("chat:a".into(), "newer".into());
        dock.acknowledge_draft("project-a", "chat:a", 1, Ok(()));
        dock.acknowledge_draft("project-b", "chat:b", 1, Ok(()));
        assert_eq!(dock.draft_dirty.len(), 2);
        dock.acknowledge_draft("project-a", "chat:b", 1, Ok(()));
        assert_eq!(dock.draft_dirty.len(), 1);
        dock.acknowledge_draft("project-a", "chat:a", 2, Err("disk full".into()));
        assert!(dock.draft_dirty.contains("chat:a"));
        assert_eq!(dock.persistence_error.as_deref(), Some("disk full"));
        dock.acknowledge_draft("project-a", "chat:a", 2, Ok(()));
        assert!(dock.dirty_drafts().is_empty());
        assert!(dock.persistence_error.is_none());
        assert_eq!(dock.draft_contents["chat:a"], "newer");
    }

    #[test]
    fn tombstones_and_layout_changes_survive_stale_persistence_results() {
        let mut dock = CompanionDockUi {
            project_id: Some("project-a".into()),
            layout_revision: 3,
            layout_dirty: true,
            ..Default::default()
        };
        dock.record_draft("chat:a".into(), "old text".into());
        dock.record_draft("chat:a".into(), String::new());
        dock.acknowledge_draft("project-a", "chat:a", 1, Ok(()));
        assert!(dock.draft_dirty.contains("chat:a"));
        assert_eq!(dock.dirty_drafts()[0].content, "");
        dock.acknowledge_layout("project-a", 2, Ok(()));
        assert!(dock.layout_dirty);
        dock.acknowledge_layout("project-a", 3, Err("layout unavailable".into()));
        assert!(dock.layout_dirty);
        assert!(dock.persistence_error.is_some());
        dock.acknowledge_layout("project-a", 3, Ok(()));
        assert!(!dock.layout_dirty);
        assert!(dock.persistence_error.is_some(), "an unsaved draft remains");
        dock.acknowledge_draft("project-a", "chat:a", 2, Ok(()));
        assert!(dock.persistence_error.is_none());
    }

    #[test]
    fn idempotent_writes_are_verified_and_conflicts_preserve_stored_tombstones() {
        use ikore_core::persistence_service::{
            EnsureProjectForWorkspaceRequest, PersistenceService,
        };
        let environment = ikore_test_support::IsolatedTestEnvironment::new();
        let persistence = PersistenceService::initialize(environment.data_dir()).unwrap();
        let project = persistence
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: environment.workspace_dir().to_string_lossy().into_owned(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        let mut draft = CompanionDraft {
            topic_id: "feedback:fixture".into(),
            conversation_id: None,
            revision: 1,
            content: "Unsent fixture".into(),
            decision_id: Some("checkpoint:fixture".into()),
            decision_state_version: Some(1),
        };
        assert!(save_draft_verified(&persistence, &project, &draft).is_ok());
        assert!(save_draft_verified(&persistence, &project, &draft).is_ok());
        draft.content = "conflicting same revision".into();
        assert!(save_draft_verified(&persistence, &project, &draft).is_err());
        draft.revision = 2;
        draft.content.clear();
        assert!(save_draft_verified(&persistence, &project, &draft).is_ok());
        draft.revision = 1;
        draft.content = "late old content".into();
        assert!(save_draft_verified(&persistence, &project, &draft).is_err());
        let layout = CompanionDockLayout {
            open: true,
            conversations: Vec::new(),
        };
        assert!(save_layout_verified(&persistence, &project, 1, &layout).is_ok());
        assert!(save_layout_verified(&persistence, &project, 1, &layout).is_ok());
        assert!(
            save_layout_verified(&persistence, &project, 1, &CompanionDockLayout::default())
                .is_err()
        );
        assert_eq!(
            persistence.load_companion_dock(&project).unwrap().drafts[0].content,
            ""
        );
    }

    #[test]
    fn pending_decisions_are_badges_on_restore_and_autoopen_only_once_later() {
        let mut dock = CompanionDockUi::default();
        assert!(!dock.should_auto_open("first"));
        dock.decisions_initialized = true;
        assert!(dock.should_auto_open("second"));
        dock.close("decision:second");
        assert!(!dock.should_auto_open("second"));
    }
}
