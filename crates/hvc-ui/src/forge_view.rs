use crate::{
    AppModel,
    forge::{FEATURE_STATE_GROUPS, ForgeAction, ForgeSection, feature_state_group_id},
    sizing::{INPUT_LONG, INPUT_MEDIUM, INPUT_SHORT, StyledExt, sized_input, text_area},
    theme,
};
use gpui::{
    AnyElement, Context, Div, Focusable, PathBuilder, Render, SharedString, Window, canvas,
    deferred, div, point, prelude::*, px, relative,
};
use hvc_core::feature_forge::*;

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .min_w_0()
}
fn label(value: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(value.into())
}
fn heading(value: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(value.into())
}
pub(crate) fn button(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    action: ForgeAction,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let keyboard_action = action.clone();
    div()
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .self_start()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::surface_raised())
        .text_color(theme::accent())
        .text_sm()
        .cursor_pointer()
        .tab_index(0)
        .hover(|s| s.bg(theme::accent_soft()))
        .on_key_down(
            cx.listener(move |model, event: &gpui::KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    model.forge_action(keyboard_action.clone(), cx);
                    if matches!(keyboard_action, ForgeAction::New(_)) {
                        focus_capture(model, window, cx);
                    }
                    cx.stop_propagation();
                }
            }),
        )
        .on_click(cx.listener(move |model, _, window, cx| {
            model.forge_action(action.clone(), cx);
            if matches!(action, ForgeAction::New(_)) {
                focus_capture(model, window, cx);
            }
        }))
        .child(title.into())
        .into_any_element()
}
/// A `button` that can be switched off.
///
/// Only colour and cursor change between states -- padding, radius and text
/// size are identical, because `forge_busy_state_never_moves_the_task_controls`
/// pins the geometry of Forge controls across state changes. Both the click and
/// the key handler are gated: leaving `on_key_down` bound would let enter or
/// space fire a disabled action.
pub(crate) fn gated_button(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    action: ForgeAction,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let keyboard_action = action.clone();
    div()
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .self_start()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .bg(if enabled {
            theme::surface_raised()
        } else {
            theme::surface()
        })
        .text_color(if enabled {
            theme::accent()
        } else {
            theme::text_muted()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .tab_index(0)
                .hover(|s| s.bg(theme::accent_soft()))
                .on_key_down(
                    cx.listener(move |model, event: &gpui::KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            model.forge_action(keyboard_action.clone(), cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .on_click(
                    cx.listener(move |model, _, _, cx| model.forge_action(action.clone(), cx)),
                )
        })
        .child(title.into())
        .into_any_element()
}

fn focus_capture(model: &AppModel, window: &mut Window, cx: &Context<AppModel>) {
    let input = if model.forge.capture_feature {
        &model.forge.feature_title
    } else {
        &model.forge.drafts[0].title
    };
    input.read(cx).focus_handle(cx).focus(window);
}

pub(crate) fn forge_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    if model.workspace.read(cx).project_id.is_none() {
        return card()
            .child(heading("Abra um projeto para organizar tasks e features"))
            .into_any_element();
    }
    let mut tabs = div().flex().flex_wrap().gap_2();
    for section in ForgeSection::ALL {
        tabs = tabs.child(button(
            format!("forge-section-{}", section.id()),
            if model.forge.section == section {
                format!("● {}", section.label())
            } else {
                section.label().to_string()
            },
            ForgeAction::Section(section),
            cx,
        ));
    }
    let body = if model.forge.section == ForgeSection::Backlog {
        backlog_body(model, cx)
    } else {
        feature_body(model, cx)
    };
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    // The description takes the slack so the buttons keep their
                    // place; `min_w_0` is what lets it shrink instead of pushing
                    // them off the edge on a narrow window.
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .flex_1()
                        .min_w_0()
                        .child(heading("Forge"))
                        .child(label(model.forge.section.description()).min_w_0()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .gap_2()
                        .child(button(
                            "capture-feature",
                            "Nova feature",
                            ForgeAction::New(true),
                            cx,
                        ))
                        .child(button(
                            "new-tasks",
                            "Adicionar tasks",
                            ForgeAction::New(false),
                            cx,
                        ))
                        .child(button(
                            "refresh-forge",
                            "Atualizar",
                            ForgeAction::Refresh,
                            cx,
                        )),
                ),
        )
        .child(tabs)
        .child(body)
        .into_any_element()
}

fn backlog_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut list = div()
        .id("task-backlog")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();
    for task in model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| t.state != "superseded")
    {
        let selected = model.forge.selected_tasks.contains(&task.id);
        let source = task
            .draft
            .feature_id
            .as_ref()
            .and_then(|id| model.forge.snapshot.features.iter().find(|f| &f.id == id))
            .map(|f| f.title.clone())
            .unwrap_or_else(|| "Standalone task".into());
        let mut row = card()
            .child(heading(task.draft.title.clone()))
            .child(label(format!(
                "{source} · {:?} · {}",
                task.draft.kind, task.state
            )))
            .child(label(task.draft.description.clone()));
        for criterion in &task.draft.criteria {
            row = row.child(label(format!("□ {}", criterion.description)));
        }
        if !task.draft.dependencies.is_empty() {
            row = row.child(label(format!(
                "Prerequisites: {}",
                task.draft
                    .dependencies
                    .iter()
                    .map(|id| model
                        .forge
                        .snapshot
                        .tasks
                        .iter()
                        .find(|t| &t.id == id)
                        .map(|t| t.draft.title.clone())
                        .unwrap_or_else(|| id.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(pulse) = task
            .active_pulsation_id
            .as_ref()
            .or(task.completed_pulsation_id.as_ref())
        {
            row = row.child(button(
                format!("task-pulsation-{}", task.id),
                "View pulsation and evidence",
                ForgeAction::OpenPulsation(pulse.clone()),
                cx,
            ));
        }
        let assessment = hvc_core::orchestration::task_refine::assess(
            &task.draft.description,
            task.revision,
            40.try_into().expect("bounded threshold"),
        );
        if assessment.score.get() >= assessment.threshold.get() {
            row = row.child(label("Description may need more detail:"));
            for contribution in assessment.contributions {
                row = row.child(label(contribution.reason));
            }
        }
        if let Some(evidence) = model
            .forge
            .snapshot
            .integration_evidence
            .iter()
            .find(|e| e.task_id == task.id)
        {
            row = row
                .child(label(format!("Integrated evidence: {}", evidence.summary)))
                .child(label(format!(
                    "Criteria: {}",
                    evidence.criterion_ids.join(", ")
                )));
        }
        if task.state == "ready"
            || (task.state == "queued"
                && task.active_pulsation_id.as_ref().is_some_and(|id| {
                    model.pulsations.read(cx).pulsations.iter().any(|p| {
                        &p.id == id
                            && p.status == hvc_core::persistence_service::PulsationStatus::Failed
                    })
                }))
        {
            row = row.child(button(
                format!("select-task-{}", task.id),
                if selected {
                    "✓ Selected"
                } else {
                    "Select for pulsation"
                },
                ForgeAction::ToggleTask(task.id.clone()),
                cx,
            ));
        }
        if task.draft.feature_id.is_none() && task.state == "ready" {
            row = row.child(button(
                format!("edit-task-{}", task.id),
                "Edit task",
                ForgeAction::EditTask(task.id.clone()),
                cx,
            ));
        }
        if task.draft.feature_id.is_none()
            && task.state == "ready"
            && TaskService::suggest_feature(&task.draft)
        {
            row = row
                .child(label(
                    "This request may benefit from a feature conversation.",
                ))
                .child(button(
                    format!("convert-task-{}", task.id),
                    "Convert to Feature in Forge",
                    ForgeAction::SuggestFeature(task.id.clone()),
                    cx,
                ));
        }
        list = list.child(row);
    }
    if model.forge.snapshot.tasks.is_empty() {
        list = list.child(label(
            "Add bugs and small tweaks or approve a feature to create tasks.",
        ));
    }
    let mut selected = card()
        .child(heading("Compose pulsation"))
        .child(label(
            "Select tasks to execute and integrate as a single batch.",
        ))
        .child(sized_input(model.forge.batch_title.clone(), INPUT_MEDIUM));
    for task in model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| model.forge.selected_tasks.contains(&t.id))
    {
        let has_explicit_path = !hvc_core::orchestration::composition::recipes::explicit_paths(
            &format!("{} {}", task.draft.title, task.draft.description),
        )
        .is_empty();
        let mut item = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label(format!("• {}", task.draft.title)));
        if !has_explicit_path {
            item = item.child(label(
                "No explicit file path detected; the dispatch gate may block this batch \
                 unless another selected task has one.",
            ));
            if let Some(input) = model.forge.path_hint_inputs.get(&task.id) {
                item = item.child(sized_input(input.clone(), INPUT_LONG));
            }
            if task.draft.feature_id.is_some() {
                item = item.child(button(
                    format!("request-path-{}", task.id),
                    "Ask Feature Agent for a path",
                    ForgeAction::RequestPathClarification(task.id.clone()),
                    cx,
                ));
            }
        }
        selected = selected.child(item);
    }
    selected = selected
        .child(label(format!(
            "{} task(s) selected",
            model.forge.selected_tasks.len()
        )))
        .child(button(
            "dispatch-batch",
            "Dispatch batch for execution",
            ForgeAction::Dispatch,
            cx,
        ));
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(button(
                    "retry-forge-publications",
                    "Reconcile planning notes",
                    ForgeAction::RetryPublications,
                    cx,
                ))
                .child(list),
        )
        .child(
            div()
                .w(px(310.0))
                .min_h_0()
                .id("batch-composer")
                .overflow_y_scroll()
                .child(selected),
        )
        .into_any_element()
}

#[derive(Clone)]
struct FeatureDrag {
    id: String,
    title: String,
}
impl Render for FeatureDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        card().child(self.title.clone())
    }
}

/// A scrolling pane that fills its column. Every column owns its own scroll:
/// nesting one inside another traps the wheel in the inner box.
///
/// `size_full`, not `flex_1`: `split_pane` gives each pane a fixed box and does
/// not lay its child out as flex, so a `flex_1` root has nothing to stretch
/// against and grows to its content instead of scrolling.
fn scroll_column(id: &'static str, content: AnyElement) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .child(content)
        .into_any_element()
}

fn feature_card(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let selected = model.forge.selected_feature.as_ref() == Some(&f.id);
    let selector = SharedString::from(format!("feature-{}", f.id));
    let debug_id = selector.clone();
    let title_selector = format!("feature-title:{}", f.id);
    div()
        .id(selector)
        .debug_selector(move || debug_id.to_string())
        .p_3()
        .rounded_md()
        .bg(if selected {
            theme::accent_soft()
        } else {
            theme::surface_raised()
        })
        .cursor_pointer()
        .on_click(cx.listener({
            let id = f.id.clone();
            move |m, _, _, cx| m.forge_action(ForgeAction::SelectFeature(id.clone()), cx)
        }))
        // Dragging a feature moves it between waves; it never reorders the list,
        // so grouping the list does not compete with it.
        .on_drag(
            FeatureDrag {
                id: f.id.clone(),
                title: f.title.clone(),
            },
            |drag, _, _, cx| cx.new(|_| drag.clone()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .debug_selector(move || title_selector.clone())
                        .child(crate::fading_title::FadingTitle::new(
                            f.title.clone(),
                            if selected {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            },
                        )),
                )
                .child(label(
                    f.evaluation
                        .score()
                        .map(|s| format!("score {s:.1}"))
                        .unwrap_or_else(|| "not scored".into()),
                )),
        )
        .into_any_element()
}

/// The feature list, grouped by lifecycle state the way Pulsations groups by
/// status. The state moves to the group header, so the cards themselves no
/// longer repeat it.
fn feature_list(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut list = div()
        .id("feature-list")
        .debug_selector(|| "feature-list".into())
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();
    for group_state in FEATURE_STATE_GROUPS {
        let members = model
            .forge
            .snapshot
            .features
            .iter()
            .filter(|f| f.state == group_state)
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        let expanded = model.forge.group_expanded(group_state);
        let selected = members
            .iter()
            .any(|f| model.forge.selected_feature.as_ref() == Some(&f.id));
        let id = SharedString::from(format!(
            "feature-group-{}",
            feature_state_group_id(group_state)
        ));
        let debug_id = id.clone();
        let mut group = div()
            .flex_shrink_0()
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(
                div()
                    .id(id)
                    .debug_selector(move || debug_id.to_string())
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .text_sm()
                    .bg(theme::surface_raised())
                    .text_color(if selected {
                        theme::accent()
                    } else {
                        theme::text()
                    })
                    .hover(|style| style.bg(theme::accent_soft()))
                    .on_click(cx.listener(move |m, _, _, cx| {
                        m.forge_action(ForgeAction::ToggleFeatureGroup(group_state), cx)
                    }))
                    .child(format!(
                        "{} {} · {}{}",
                        if expanded { "▾" } else { "▸" },
                        state(group_state),
                        members.len(),
                        if selected && !expanded {
                            " · selected"
                        } else {
                            ""
                        }
                    )),
            );
        if expanded {
            let mut cards = div().p_2().flex().flex_col().gap_2();
            for f in members {
                cards = cards.child(feature_card(model, f, cx));
            }
            group = group.child(cards);
        }
        list = list.child(group);
    }
    list.into_any_element()
}

/// The right-hand side of the feature split: either one pane, or two when the
/// section has an aside.
fn section_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    if model.forge.section == ForgeSection::Waves {
        return scroll_column("forge-detail", waves(model, cx));
    }
    let Some(f) = model.forge.feature() else {
        return scroll_column(
            "forge-detail",
            card().child(heading("Turn an idea into an executable plan")).child(label("Capture the idea to discuss with the Feature Agent, evaluate alternatives, and prepare tasks.")).child(button("forge-first-feature","New feature",ForgeAction::New(true),cx)).into_any_element(),
        );
    };
    match model.forge.section {
        ForgeSection::Incubation => crate::split_pane::split_pane(
            "forge-incubation-columns",
            &model.forge_incubation_split,
            scroll_column("forge-detail", incubation_main(model, f, cx)),
            scroll_column("forge-incubation-aside", incubation_aside(model, f, cx)),
            cx,
        ),
        ForgeSection::Triage => {
            let main = scroll_column("forge-detail", triage_main(model, f, cx));
            // Conditional, following the workspace context drawer: the analysis
            // column earns its width only once there is a verdict to read.
            if model.forge.triage_aside_open {
                crate::split_pane::split_pane(
                    "forge-triage-columns",
                    &model.forge_triage_split,
                    main,
                    scroll_column("forge-triage-aside", triage_aside(model, f, cx)),
                    cx,
                )
            } else {
                main
            }
        }
        ForgeSection::Dependencies => scroll_column("forge-detail", dependencies(model, f, cx)),
        _ => scroll_column("forge-detail", delivery(model, f, cx)),
    }
}

fn feature_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let list = feature_list(model, cx);
    let body = section_body(model, cx);
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .child(crate::split_pane::split_pane(
            "forge-feature-columns",
            &model.forge_feature_split,
            list,
            body,
            cx,
        ))
        .into_any_element()
}
fn state(s: FeatureState) -> &'static str {
    match s {
        FeatureState::Incubating => "Incubation",
        FeatureState::InTriage => "Triage",
        FeatureState::Approved => "Approved",
        FeatureState::Planned => "Planned",
        FeatureState::Executing => "Executing",
        FeatureState::ReadyForReview => "Review delivery",
        FeatureState::Done => "Done",
        FeatureState::Archived => "Archived",
    }
}
/// The conversation column: what the feature is, the exchange with the agent,
/// and the composer.
fn incubation_main(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut panel = card()
        .child(heading(f.title.clone()))
        .child(label(format!(
            "{} · revision {} · context {}",
            state(f.state),
            f.revision,
            f.brief.coverage
        )))
        .child(label(f.original_idea.clone()));
    // Analysis turns share the table but are not conversation -- their verdicts
    // are read in the Triage and Dependencies columns, not here.
    for turn in model
        .forge
        .history
        .iter()
        .filter(|turn| turn.kind == TurnKind::Discovery)
    {
        panel = panel.child(card().child(label("You")).child(turn.user_message.clone()));
        if let Some(reply) = &turn.assistant_message {
            panel = panel.child(card().child(label("Feature Agent")).child(reply.clone()));
        }
        if let Some(error) = &turn.error {
            panel = panel.child(div().text_color(theme::danger()).child(error.clone()));
        }
        if turn.status == "running" {
            panel = panel
                .child(label("Feature Agent is analyzing context…"))
                .child(button(
                    format!("cancel-feature-turn-{}", turn.id),
                    "Cancel response",
                    ForgeAction::CancelTurn(turn.id.clone()),
                    cx,
                ));
        }
    }
    panel = panel.child(
        div()
            .debug_selector(|| "forge-message-composer".into())
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .child(text_area(model.forge.message.clone()))
            .child(label("Enter sends · Shift+Enter inserts a newline"))
            .child(button(
                "send-feature-message",
                "Send message",
                ForgeAction::Send,
                cx,
            )),
    );
    panel.into_any_element()
}

/// The specification column: what the agent has assembled so far, and the
/// control that turns it into tasks.
fn incubation_aside(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    if f.brief.objective.is_empty() {
        return card()
            .child(heading("Proposed specification"))
            .child(label(
                "Nothing proposed yet. Describe the idea and answer the agent's questions -- the specification fills in here as the scope settles.",
            ))
            .into_any_element();
    }
    let already_approved = f.approved_revision == Some(f.revision);
    let mut panel = card().child(brief(f)).child(gated_button(
        "approve-feature",
        "Approve scope and create tasks",
        ForgeAction::Approve,
        !already_approved,
        cx,
    ));
    if already_approved {
        panel = panel.child(label(
            "This revision is already approved and its tasks exist. Keep talking to the agent to produce a new revision.",
        ));
    }
    if model.forge.busy {
        panel = panel.child(label("Working…"));
    }
    panel.into_any_element()
}
fn brief(f: &Feature) -> Div {
    let mut d = card().child(heading("Proposed specification"));
    for (title, value) in [
        ("Objective", &f.brief.objective),
        ("Audience", &f.brief.audience),
        ("Behavior", &f.brief.behavior),
        ("Scope", &f.brief.scope),
        ("Out of scope", &f.brief.exclusions),
    ] {
        d = d.child(label(title)).child(value.clone());
    }
    for decision in &f.brief.decisions {
        d = d.child(label(format!("Decision: {decision}")));
    }
    for pending in &f.brief.pending_decisions {
        d = d.child(
            div()
                .text_color(theme::warning())
                .child(format!("Pending: {pending}")),
        );
    }
    for alternative in &f.brief.alternatives {
        d = d.child(label(format!("Alternative: {alternative}")));
    }
    for c in &f.brief.criteria {
        d = d.child(label(format!("Acceptance: {}", c.description)));
    }
    for t in &f.brief.tasks {
        d = d.child(label(format!(
            "Proposed task: {} — {}",
            t.title, t.description
        )));
    }
    for s in &f.brief.sources {
        d = d.child(label(format!(
            "Consulted source: {}",
            s.relative_path.as_str()
        )));
    }
    d
}
/// Renders the analysis affordance plus whatever the last analysis said.
/// The control that asks for an analysis. Stays in the main column, because the
/// analysis column is what it opens.
fn triage_analysis_trigger(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> Div {
    let blocked = model.forge.analysis_blocked(f);
    let mut agent = card().child(heading("Feature Agent analysis"));
    if model.forge.pending_triage_overwrite {
        agent = agent
            .child(
                div()
                    .text_color(theme::warning())
                    .child("Your answers on screen differ from what is saved. Running the analysis replaces them."),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button(
                        "confirm-analyze-triage",
                        "Replace with the analysis",
                        ForgeAction::ConfirmAnalyzeTriage,
                        cx,
                    ))
                    .child(button(
                        "cancel-analyze-triage",
                        "Keep my answers",
                        ForgeAction::CancelAnalyzeTriage,
                        cx,
                    )),
            );
    } else {
        agent = agent
            .child(label(
                "The agent answers the four screens, sets the ratings and the structural level, then recommends approving or archiving.",
            ))
            .child(gated_button(
                "analyze-triage",
                "Analyze with Feature Agent",
                ForgeAction::AnalyzeTriage,
                blocked.is_none(),
                cx,
            ));
        if let Some(reason) = blocked {
            agent = agent.child(label(reason));
        }
    }
    agent
}

/// The analysis column: whatever the last triage turn produced.
fn triage_aside(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut agent = card()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(heading("Feature Agent analysis"))
                .child(button(
                    "close-triage-aside",
                    "Close",
                    ForgeAction::CloseTriageAside,
                    cx,
                )),
        )
        .child(label(format!("Feature: {}", f.title)));
    match model
        .forge
        .history
        .iter()
        .rev()
        .find(|turn| turn.kind == TurnKind::Triage)
    {
        Some(turn) => {
            if turn.status == "running" {
                agent = agent
                    .child(label("Feature Agent is evaluating this feature…"))
                    .child(button(
                        format!("cancel-triage-turn-{}", turn.id),
                        "Cancel analysis",
                        ForgeAction::CancelTurn(turn.id.clone()),
                        cx,
                    ));
            } else if let Some(message) = &turn.assistant_message {
                agent = agent.child(message.clone());
            }
            if let Some(error) = &turn.error {
                agent = agent.child(div().text_color(theme::danger()).child(error.clone()));
            }
        }
        None => {
            agent = agent.child(label(
                "No analysis for this feature yet. Run one from the evaluation column and the verdict lands here.",
            ));
        }
    }
    agent.into_any_element()
}

fn triage_main(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut p = card().child(heading("Triage and decision")).child(label(
        "Evaluation guides priority. You decide whether the feature moves forward.",
    ));
    p = p.child(triage_analysis_trigger(model, f, cx));
    for (i, name) in [
        "Value and need",
        "Product alignment",
        "Feasibility and engineering",
        "Usability and adoption",
    ]
    .into_iter()
    .enumerate()
    {
        p = p
            .child(label(name))
            .child(text_area(model.forge.filters[i].clone()));
    }
    p = p
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(button(
                    "rating-impact",
                    format!("Impact: {}", model.forge.impact),
                    ForgeAction::Rating(0),
                    cx,
                ))
                .child(button(
                    "rating-confidence",
                    format!("Confidence: {}", model.forge.confidence),
                    ForgeAction::Rating(1),
                    cx,
                ))
                .child(button(
                    "rating-effort",
                    format!("Effort: {}", model.forge.effort),
                    ForgeAction::Rating(2),
                    cx,
                )),
        )
        .child(label(
            "Click a rating to cycle 1 to 5. Score = impact × confidence ÷ effort.",
        ))
        .child(button(
            "structural-level",
            format!(
                "Level: {}",
                ["Foundation", "Domain", "Interface", "Optimization"][model.forge.level as usize]
            ),
            ForgeAction::Level,
            cx,
        ))
        .child(button(
            "save-evaluation",
            "Save evaluation",
            ForgeAction::Evaluate,
            cx,
        ))
        .child(text_area(model.forge.rationale.clone()));
    if f.state == FeatureState::Archived {
        p = p.child(button(
            "reopen-feature",
            "Reopen feature",
            ForgeAction::Reopen,
            cx,
        ));
    } else {
        // Approval is once per revision: the tasks it created already exist and
        // may be in delivery. The core declines the repeat, so the control says
        // so instead of offering an action that does nothing.
        let already_approved = f.approved_revision == Some(f.revision);
        p = p
            .child(button(
                "archive-feature",
                "Archive with rationale",
                ForgeAction::Archive,
                cx,
            ))
            .child(gated_button(
                "triage-approve",
                "Approve scope and tasks",
                ForgeAction::Approve,
                !already_approved,
                cx,
            ));
        if already_approved {
            p = p.child(label(
                "This revision is already approved and its tasks exist. Revise the scope in Incubation to produce a new revision.",
            ));
        }
    }
    p.into_any_element()
}
fn dependency_analysis_card(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> Div {
    let blocked = model.forge.dependency_analysis_blocked(f);
    let agent_edges = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|edge| edge.feature_id == f.id && edge.origin == DependencyOrigin::Agent)
        .count();
    let mut agent = card()
        .child(heading("Feature Agent analysis"))
        .child(label(
            "The agent maps prerequisites and applies them directly. Relations it proposes are marked, and you can remove any of them.",
        ))
        .child(gated_button(
            "analyze-dependencies",
            "Map prerequisites with Feature Agent",
            ForgeAction::AnalyzeDependencies,
            blocked.is_none(),
            cx,
        ));
    if let Some(reason) = blocked {
        agent = agent.child(label(reason));
    }
    if agent_edges > 0 {
        agent = agent.child(button(
            "clear-agent-dependencies",
            format!("Remove all {agent_edges} agent-proposed relation(s)"),
            ForgeAction::ClearAgentDependencies,
            cx,
        ));
    }
    if let Some(turn) = model
        .forge
        .history
        .iter()
        .rev()
        .find(|turn| turn.kind == TurnKind::Dependencies)
    {
        if turn.status == "running" {
            agent = agent
                .child(label("Feature Agent is mapping prerequisites…"))
                .child(button(
                    format!("cancel-dependency-turn-{}", turn.id),
                    "Cancel analysis",
                    ForgeAction::CancelTurn(turn.id.clone()),
                    cx,
                ));
        } else if let Some(message) = &turn.assistant_message {
            agent = agent.child(label(message.clone()));
        }
        if let Some(error) = &turn.error {
            agent = agent.child(div().text_color(theme::danger()).child(error.clone()));
        }
    }
    agent
}

fn dependencies(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut p=card().child(heading("Confirmed dependencies")).child(label("Arrows point from the feature to its prerequisites. Adding a relation requires confirmation and rationale.")).child(text_area(model.forge.rationale.clone()));
    p = p.child(dependency_analysis_card(model, f, cx));
    for edge in model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.feature_id == f.id)
    {
        let pre = model
            .forge
            .snapshot
            .features
            .iter()
            .find(|x| x.id == edge.prerequisite_id);
        p = p.child(
            card()
                .child(format!(
                    "{} → {}",
                    f.title,
                    pre.map(|x| x.title.as_str())
                        .unwrap_or("Feature unavailable")
                ))
                .child(match edge.origin {
                    DependencyOrigin::Agent => div()
                        .text_xs()
                        .text_color(theme::warning())
                        .child("Proposed by the Feature Agent"),
                    DependencyOrigin::Human => div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Confirmed by you"),
                })
                .child(label(edge.rationale.clone()))
                .child(button(
                    format!("remove-edge-{}", edge.prerequisite_id),
                    "Remove relation",
                    ForgeAction::Dependency(edge.prerequisite_id.clone(), false),
                    cx,
                )),
        );
    }
    p = p.child(heading("Confirm a prerequisite"));
    for other in model.forge.snapshot.features.iter().filter(|x| {
        x.id != f.id
            && !model
                .forge
                .snapshot
                .dependencies
                .iter()
                .any(|e| e.feature_id == f.id && e.prerequisite_id == x.id)
    }) {
        p = p.child(button(
            format!("add-edge-{}", other.id),
            format!("Depends on {} ({})", other.title, state(other.state)),
            ForgeAction::Dependency(other.id.clone(), true),
            cx,
        ));
    }
    if let Some(turn) = model
        .forge
        .history
        .iter()
        .rev()
        .find(|t| t.status == "completed")
    {
        for edge in &turn.suggested_dependencies {
            if model
                .forge
                .snapshot
                .dependencies
                .iter()
                .any(|d| d.feature_id == f.id && d.prerequisite_id == edge.prerequisite_id)
            {
                continue;
            }
            if let Some(pre) = model
                .forge
                .snapshot
                .features
                .iter()
                .find(|x| x.id == edge.prerequisite_id)
            {
                p = p.child(
                    card()
                        .child(label(format!(
                            "Pending suggestion: {} → {}",
                            f.title, pre.title
                        )))
                        .child(label(edge.rationale.clone()))
                        .child(button(
                            format!("accept-suggestion-{}", pre.id),
                            "Confirm suggested dependency",
                            ForgeAction::ConfirmSuggestion(
                                edge.prerequisite_id.clone(),
                                edge.rationale.clone(),
                            ),
                            cx,
                        )),
                );
            }
        }
    }
    card()
        .child(feature_graph(model, cx))
        .child(p)
        .into_any_element()
}
fn feature_graph(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut counts = [0usize; 4];
    let nodes = model
        .forge
        .snapshot
        .features
        .iter()
        .filter(|f| f.state != FeatureState::Archived)
        .map(|f| {
            let level = usize::from(f.structural_level.min(3));
            let row = counts[level];
            counts[level] += 1;
            (
                f.clone(),
                10.0 + level as f32 * 200.0,
                42.0 + row as f32 * 82.0,
            )
        })
        .collect::<Vec<_>>();
    let edges = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter_map(|e| {
            let (_, x, y) = nodes.iter().find(|(f, _, _)| f.id == e.feature_id)?;
            let (_, tx, ty) = nodes.iter().find(|(f, _, _)| f.id == e.prerequisite_id)?;
            Some(if x == tx {
                (
                    *x + 90.0,
                    if y < ty { *y + 60.0 } else { *y - 3.0 },
                    *tx + 90.0,
                    if y < ty { *ty - 3.0 } else { *ty + 60.0 },
                )
            } else {
                (
                    if x < tx { *x + 180.0 } else { *x },
                    *y + 26.0,
                    if x < tx { *tx - 3.0 } else { *tx + 183.0 },
                    *ty + 26.0,
                )
            })
        })
        .collect::<Vec<_>>();
    let height = (counts.into_iter().max().unwrap_or(1) as f32 * 82.0 + 58.0).max(200.0);
    let mut graph = div().relative().w(px(800.0)).h(px(height)).child(
        canvas(
            move |_, _, _| edges,
            |bounds, edges, window, _| {
                for (x, y, tx, ty) in edges {
                    let mut path = PathBuilder::stroke(px(2.0));
                    let a = point(bounds.left() + px(x), bounds.top() + px(y));
                    let b = point(bounds.left() + px(tx), bounds.top() + px(ty));
                    path.move_to(a);
                    path.line_to(b);
                    let angle = (ty - y).atan2(tx - x);
                    for offset in [-0.5f32, 0.5] {
                        path.move_to(b);
                        path.line_to(point(
                            b.x - px((angle + offset).cos() * 10.0),
                            b.y - px((angle + offset).sin() * 10.0),
                        ));
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, theme::accent());
                    }
                }
            },
        )
        .absolute()
        .size_full(),
    );
    for (i, title) in ["Foundation / Wave 0", "Domain", "Interface", "Optimization"]
        .iter()
        .enumerate()
    {
        graph = graph.child(
            label(*title)
                .absolute()
                .left(px(10.0 + i as f32 * 200.0))
                .top(px(8.0)),
        );
    }
    for (f, x, y) in nodes {
        graph = graph.child(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(180.0))
                .child(button(
                    format!("graph-node-{}", f.id),
                    f.title,
                    ForgeAction::SelectFeature(f.id),
                    cx,
                )),
        );
    }
    div()
        .id("feature-graph")
        .debug_selector(|| "feature-graph".into())
        .min_w_0()
        .h(px(280.0))
        .overflow_scroll()
        .child(graph)
        .into_any_element()
}
fn waves(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut columns = div().flex().gap_2().min_w_0();
    for (wave, title) in [
        (Wave::Current, "Current Wave"),
        (Wave::Next, "Next Wave"),
        (Wave::Horizon, "Horizon"),
    ] {
        let mut column = card().flex_1().child(heading(title));
        for f in model
            .forge
            .snapshot
            .features
            .iter()
            .filter(|f| f.wave == Some(wave) && f.state != FeatureState::Archived)
        {
            column = column.child(
                div()
                    .id(SharedString::from(format!("wave-feature-{}", f.id)))
                    .on_drag(
                        FeatureDrag {
                            id: f.id.clone(),
                            title: f.title.clone(),
                        },
                        |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .child(
                        card()
                            .child(f.title.clone())
                            .child(label(state(f.state)))
                            .children(
                                [
                                    (Wave::Current, "Current"),
                                    (Wave::Next, "Next"),
                                    (Wave::Horizon, "Horizon"),
                                ]
                                .into_iter()
                                .filter(|(w, _)| *w != wave)
                                .map(|(w, name)| {
                                    button(
                                        format!("move-{}-{w:?}", f.id),
                                        name,
                                        ForgeAction::Move(f.id.clone(), w),
                                        cx,
                                    )
                                }),
                            ),
                    ),
            );
        }
        columns = columns.child(
            column
                .id(SharedString::from(format!("wave-{wave:?}")))
                .debug_selector(move || format!("wave-{wave:?}"))
                .min_h(px(180.0))
                .on_drop(cx.listener(move |m, drag: &FeatureDrag, _, cx| {
                    m.forge_action(ForgeAction::Move(drag.id.clone(), wave), cx)
                })),
        );
    }
    let mut p = card()
        .child(heading("Wave planning"))
        .child(label(format!(
            "Current Wave capacity: {} features. Drag a feature to a wave or use the buttons.",
            model.forge.snapshot.current_wave_limit
        )))
        .child(sized_input(model.forge.wave_limit.clone(), INPUT_SHORT))
        .child(button(
            "set-wave-limit",
            "Update capacity",
            ForgeAction::SetLimit,
            cx,
        ))
        .child(columns);
    for f in model.forge.snapshot.features.iter().filter(|f| {
        f.wave.is_none()
            && f.approved_revision == Some(f.revision)
            && f.state != FeatureState::Archived
    }) {
        p = p.child(
            card()
                .child(f.title.clone())
                .child(button(
                    format!("plan-current-{}", f.id),
                    if f.brief.foundation {
                        "Plan foundation / Wave 0"
                    } else {
                        "Plan in Current Wave"
                    },
                    ForgeAction::Move(f.id.clone(), Wave::Current),
                    cx,
                ))
                .child(button(
                    format!("plan-next-{}", f.id),
                    "Plan in Next Wave",
                    ForgeAction::Move(f.id.clone(), Wave::Next),
                    cx,
                )),
        );
    }
    p.into_any_element()
}
fn delivery(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let tasks = model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| {
            t.draft.feature_id.as_ref() == Some(&f.id) && t.feature_revision == f.approved_revision
        })
        .collect::<Vec<_>>();
    let done = tasks.iter().filter(|t| t.state == "done").count();
    let mut p = card()
        .child(heading("Feature delivery"))
        .child(label(format!("{done}/{} tasks integrated", tasks.len())))
        .child(button(
            "compose-feature-tasks",
            "Select tasks from this feature",
            ForgeAction::SelectFeatureTasks(f.id.clone()),
            cx,
        ));
    for t in tasks {
        p = p.child(
            card()
                .child(t.draft.title.clone())
                .child(label(t.state.clone()))
                .children(
                    t.draft
                        .criteria
                        .iter()
                        .map(|c| label(format!("□ {}", c.description))),
                ),
        );
    }
    for evidence in &model.forge.snapshot.integration_evidence {
        if let Some(task) = model.forge.snapshot.tasks.iter().find(|t| {
            t.id == evidence.task_id
                && t.draft.feature_id.as_ref() == Some(&f.id)
                && t.feature_revision == f.approved_revision
        }) {
            p = p.child(
                card()
                    .child(heading(format!("Integrated review: {}", task.draft.title)))
                    .child(label(evidence.summary.clone()))
                    .child(label(format!(
                        "Covered criteria: {}",
                        evidence.criterion_ids.join(", ")
                    ))),
            );
        }
    }
    for t in &model.forge.snapshot.tasks {
        if t.draft.feature_id.as_ref() == Some(&f.id)
            && let Some(pulse) = t
                .active_pulsation_id
                .as_ref()
                .or(t.completed_pulsation_id.as_ref())
        {
            p = p.child(button(
                format!("delivery-evidence-{}", t.id),
                format!("Evidence: {}", t.draft.title),
                ForgeAction::OpenPulsation(pulse.clone()),
                cx,
            ));
        }
    }
    p = p
        .child(heading("Final review of feature criteria"))
        .children(
            f.brief
                .criteria
                .iter()
                .map(|c| label(format!("□ {}", c.description))),
        )
        .child(label(
            "Confirm delivery after reviewing evidence and publications from linked pulsations.",
        ))
        .child(button(
            "complete-feature",
            "Confirm criteria and complete feature",
            ForgeAction::Complete,
            cx,
        ));
    p.into_any_element()
}
pub(crate) fn capture_overlay(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut content = card()
        .debug_selector(|| "forge-capture-content".into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .w(px(720.0))
        .max_w(relative(0.9))
        .max_h(relative(0.9))
        .child(heading("New work"))
        .child(
            div()
                .flex()
                .gap_2()
                .child(button(
                    "capture-tasks-option",
                    "Add tasks",
                    ForgeAction::New(false),
                    cx,
                ))
                .child(button(
                    "capture-feature-option",
                    "New feature",
                    ForgeAction::New(true),
                    cx,
                ))
                .child(button("close-capture", "Close", ForgeAction::Close, cx)),
        );
    let mut body = div()
        .id("capture-body")
        .debug_selector(|| "forge-capture-body".into())
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3();
    if model.forge.capture_feature {
        body = body
            .child(label("Discuss the feature before defining its execution."))
            .child(sized_input(model.forge.feature_title.clone(), INPUT_MEDIUM))
            .child(sized_input(model.forge.idea.clone(), INPUT_LONG))
            .child(button(
                "create-feature",
                "Create feature and start conversation",
                ForgeAction::CreateFeature,
                cx,
            ));
    } else {
        for (i, draft) in model.forge.drafts.iter().enumerate() {
            body = body.child(
                card()
                    .child(label(format!("Task {}", i + 1)))
                    .child(sized_input(draft.title.clone(), INPUT_MEDIUM))
                    .child(sized_input(draft.description.clone(), INPUT_LONG))
                    .child(sized_input(draft.criteria.clone(), INPUT_LONG))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button(
                                format!("draft-kind-{i}"),
                                format!("Kind: {:?}", draft.kind),
                                ForgeAction::Kind(i),
                                cx,
                            ))
                            .child(button(
                                format!("remove-draft-{i}"),
                                "Remove",
                                ForgeAction::RemoveDraft(i),
                                cx,
                            )),
                    ),
            );
        }
        for (i, draft) in model.forge.drafts.iter().enumerate() {
            body = body.child(label(format!("Task {} prerequisites", i + 1)));
            for t in model.forge.snapshot.tasks.iter().filter(|t| {
                t.state != "superseded"
                    && model
                        .forge
                        .editing_task
                        .as_ref()
                        .is_none_or(|(id, _)| id != &t.id)
            }) {
                body = body.child(button(
                    format!("task-dependency-{i}-{}", t.id),
                    format!(
                        "{} {}",
                        if draft.dependencies.contains(&t.id) {
                            "✓"
                        } else {
                            "+"
                        },
                        t.draft.title
                    ),
                    ForgeAction::TaskDependency(i, t.id.clone()),
                    cx,
                ));
            }
        }
        if model.forge.editing_task.is_none() {
            body = body.child(button(
                "add-task-draft",
                "+ Another task",
                ForgeAction::AddDraft,
                cx,
            ));
        }
        body = body.child(button(
            "save-task-drafts",
            "Save tasks to backlog",
            ForgeAction::SaveTasks,
            cx,
        ));
    }
    content = content.child(body);
    deferred(
        div()
            .debug_selector(|| "forge-capture-overlay".into())
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|model, _, _, cx| {
                    model.forge_action(ForgeAction::Close, cx);
                    cx.stop_propagation();
                }),
            )
            .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    model.forge_action(ForgeAction::Close, cx);
                    cx.stop_propagation();
                }
            }))
            .bg(gpui::rgba(0x000000aa))
            .child(content),
    )
    .with_priority(20)
    .into_any_element()
}
