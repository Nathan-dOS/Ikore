mod backlog;
mod delivery;
mod dependencies;
mod incubation;
mod triage;
mod waves;

use crate::{
    AppModel,
    forge::{FEATURE_STATE_GROUPS, ForgeAction, ForgeSection, feature_state_group_id},
    icons::Icon,
    sizing::{INPUT_LONG, INPUT_MEDIUM, StyledExt, sized_input},
    text_input::NativeTextInput,
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{
    AnyElement, Context, Div, Entity, Focusable, Rgba, SharedString, Window, deferred, div,
    prelude::*, px, relative,
};
use ikore_core::feature_forge::{Feature, FeatureState, FeatureTurn, TaskKind, TurnFailureKind};

/// ADR-034 D1: shown in the batch composer while the project runs batches
/// without stage composition, which earlier pipelines do not get.
pub(crate) const REVIEW_POLICY_NEEDS_COMPOSITION: &str = "Review policy, risk scrutiny and coverage need stage composition (Settings -> Stage composition).";

pub(super) fn copy_message_button(id: String, message: String) -> impl IntoElement {
    let copy_id = SharedString::from(format!("{id}:copy"));
    let debug_id = copy_id.clone();
    div()
        .id(copy_id)
        .debug_selector(move || debug_id.to_string())
        .cursor_pointer()
        .text_xs()
        .text_color(theme::text_subtle())
        .hover(|style| style.text_color(theme::text()))
        .tooltip(widgets::text_tooltip("Copy message"))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(message.clone()));
        })
        .child("Copy")
}

pub(super) fn user_message_card(id: String, message: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_muted())
                        .child("You"),
                )
                .child(copy_message_button(id.clone(), message.to_owned())),
        )
        .child(crate::selectable_message::SelectableMessage::new(
            id, message,
        ))
}

pub(super) fn agent_message_card(id: String, message: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(theme::surface_raised())
        .border_1()
        .border_color(theme::border_strong())
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child("Feature Agent"),
                )
                .child(copy_message_button(id.clone(), message.to_owned())),
        )
        .child(crate::selectable_message::SelectableMessage::new(
            id, message,
        ))
}

pub(super) fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_lg()
        .bg(theme::surface_raised())
        .min_w_0()
}

pub(super) fn recessed_box() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_2()
        .rounded_md()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .min_w_0()
}

pub(super) fn label(value: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(value.into())
}

pub(super) fn eyebrow(value: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_subtle())
        .child(value.into())
}

pub(super) fn heading(value: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(value.into())
}

/// A heading with its explanation in an "i" instead of a paragraph under it.
pub(super) fn heading_info(
    value: impl Into<SharedString>,
    id: &'static str,
    info: impl Into<SharedString>,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(heading(value))
        .child(widgets::info_tip(id, info))
}

pub(super) fn pill(text: impl Into<SharedString>, bg: Rgba, fg: Rgba) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .bg(bg)
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(fg)
        .child(text.into())
        .into_any_element()
}

pub(super) fn mono_pill(text: impl Into<SharedString>, bg: Rgba, fg: Rgba) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .bg(bg)
        .font_family(theme::FONT_MONO)
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(fg)
        .child(text.into())
        .into_any_element()
}

pub(super) fn pill_icon(
    glyph: Icon,
    text: impl Into<SharedString>,
    bg: Rgba,
    fg: Rgba,
) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .bg(bg)
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(fg)
        .child(widgets::icon(glyph, px(12.0), fg))
        .child(text.into())
        .into_any_element()
}

pub(super) fn pill_dot(dot: Rgba, text: impl Into<SharedString>, bg: Rgba, fg: Rgba) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .bg(bg)
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(fg)
        .child(widgets::status_dot(dot))
        .child(text.into())
        .into_any_element()
}

pub(super) fn search_box(input: Entity<NativeTextInput>) -> Div {
    div().relative().w_full().min_w_0().child(input).child(
        div()
            .absolute()
            .left(px(10.0))
            .top(px(13.0))
            .child(widgets::icon(Icon::Search, px(14.0), theme::text_subtle())),
    )
}

pub(crate) fn button(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    action: ForgeAction,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    button_variant(id, title, ButtonVariant::Tinted, action, None, cx)
}

pub(super) fn button_variant(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    variant: ButtonVariant,
    action: ForgeAction,
    icon_override: Option<Icon>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let glyph = icon_override.or_else(|| widgets::action_icon(&id));
    widgets::button_frame(
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .self_start()
            .tab_index(0),
        variant,
        true,
    )
    // Enter and Space arrive here too: GPUI clicks the focused button on key up.
    .on_click(cx.listener(move |model, _, window, cx| {
        model.forge_action(action.clone(), cx);
        if matches!(action, ForgeAction::New(_)) {
            focus_capture(model, window, cx);
        }
    }))
    .gap_2()
    .when_some(glyph, |button, glyph| {
        button.child(widgets::icon(
            glyph,
            px(14.0),
            widgets::button_ink(variant, true),
        ))
    })
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
    gated_button_variant(id, title, ButtonVariant::Tinted, action, enabled, None, cx)
}

pub(super) fn gated_button_variant(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    variant: ButtonVariant,
    action: ForgeAction,
    enabled: bool,
    icon_override: Option<Icon>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let glyph = icon_override.or_else(|| widgets::action_icon(&id));
    widgets::button_frame(
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .self_start(),
        variant,
        enabled,
    )
    .when(enabled, |button| {
        button
            .tab_index(0)
            .on_click(cx.listener(move |model, _, _, cx| model.forge_action(action.clone(), cx)))
    })
    .gap_2()
    .when_some(glyph, |button, glyph| {
        button.child(widgets::icon(
            glyph,
            px(14.0),
            widgets::button_ink(variant, enabled),
        ))
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
            .child(heading("Open a project to organize tasks and features"))
            .into_any_element();
    }
    let mut tabs = widgets::segment_track(div()).self_start().flex_wrap();
    for section in ForgeSection::ALL {
        let id = SharedString::from(format!("forge-section-{}", section.id()));
        let selector = id.clone();
        tabs = tabs.child(
            widgets::segment(
                div()
                    .id(id)
                    .debug_selector(move || selector.to_string())
                    .tab_index(0),
                model.forge.section == section,
            )
            .on_click(cx.listener(move |model, _, _, cx| {
                model.forge_action(ForgeAction::Section(section), cx);
            }))
            .child(section.label()),
        );
    }
    let body = if model.forge.section == ForgeSection::Backlog {
        backlog::backlog_body(model, cx)
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
                        .child(heading("Forge")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_shrink_0()
                        .gap_2()
                        .child(button(
                            "capture-feature",
                            "New feature",
                            ForgeAction::New(true),
                            cx,
                        ))
                        .child(button(
                            "new-tasks",
                            "Add tasks",
                            ForgeAction::New(false),
                            cx,
                        ))
                        .child(button("refresh-forge", "Refresh", ForgeAction::Refresh, cx)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(tabs)
                // Explains whichever section is open, so the "i" follows the selection.
                .child(widgets::info_tip(
                    "forge-section",
                    model.forge.section.description(),
                )),
        )
        .child(body)
        .into_any_element()
}

#[derive(Clone)]
pub(super) struct FeatureDrag {
    pub id: String,
    pub title: String,
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
pub(super) fn scroll_column(id: &'static str, content: AnyElement) -> AnyElement {
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
                    .hover(|style| style.bg(theme::surface_hover()))
                    .on_click(cx.listener(move |m, _, _, cx| {
                        m.forge_action(ForgeAction::ToggleFeatureGroup(group_state), cx)
                    }))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(widgets::icon(
                        if expanded {
                            Icon::ChevronDown
                        } else {
                            Icon::ChevronRight
                        },
                        px(12.0),
                        theme::text_subtle(),
                    ))
                    .child(format!(
                        "{} · {}{}",
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
        return scroll_column("forge-detail", waves::waves(model, cx));
    }
    let Some(f) = model.forge.feature() else {
        return scroll_column(
            "forge-detail",
            card()
                .child(heading_info(
                    "Turn an idea into an executable plan",
                    "forge-first-feature",
                    "Capture the idea to discuss with the Feature Agent, evaluate alternatives, and prepare tasks.",
                ))
                .child(button(
                    "forge-first-feature",
                    "New feature",
                    ForgeAction::New(true),
                    cx,
                ))
                .into_any_element(),
        );
    };
    match model.forge.section {
        ForgeSection::Incubation => crate::split_pane::split_pane(
            "forge-incubation-columns",
            &model.forge_incubation_split,
            incubation::incubation_main(model, f, cx),
            incubation::incubation_aside(model, f, cx),
            cx,
        ),
        ForgeSection::Triage => {
            let main = scroll_column("forge-detail", triage::triage_main(model, f, cx));
            // Conditional, following the workspace context drawer: the analysis
            // column earns its width only once there is a verdict to read.
            if model.forge.triage_aside_open {
                crate::split_pane::split_pane(
                    "forge-triage-columns",
                    &model.forge_triage_split,
                    main,
                    scroll_column("forge-triage-aside", triage::triage_aside(model, f, cx)),
                    cx,
                )
            } else {
                main
            }
        }
        ForgeSection::Dependencies => {
            scroll_column("forge-detail", dependencies::dependencies(model, f, cx))
        }
        _ => scroll_column("forge-detail", delivery::delivery(model, f, cx)),
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

pub(super) fn state(s: FeatureState) -> &'static str {
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

pub(super) fn layer_label(level: u8) -> &'static str {
    match level {
        0 => "Foundation",
        1 => "Domain",
        2 => "Interface",
        _ => "Optimization",
    }
}

pub(super) fn layer_short(level: u8) -> &'static str {
    match level {
        0 => "FND",
        1 => "DOM",
        2 => "UI",
        _ => "OPT",
    }
}

/// The dot colour of a feature by where it is in the flow.
pub(super) fn feature_state_color(state: FeatureState) -> Rgba {
    match state {
        FeatureState::Done => theme::success(),
        FeatureState::ReadyForReview => theme::purple(),
        FeatureState::Executing => theme::running(),
        FeatureState::Planned | FeatureState::Approved => theme::accent(),
        FeatureState::Incubating | FeatureState::InTriage | FeatureState::Archived => {
            theme::text_subtle()
        }
    }
}

pub(super) fn feature_state_badge(s: FeatureState) -> (&'static str, Rgba, Rgba) {
    let label = state(s);
    match s {
        FeatureState::Done => (label, theme::success_soft(), theme::success()),
        FeatureState::ReadyForReview => (label, theme::purple_soft(), theme::purple()),
        FeatureState::Executing => (label, theme::running_soft(), theme::running()),
        FeatureState::Planned | FeatureState::Approved => {
            (label, theme::accent_soft(), theme::accent())
        }
        FeatureState::Incubating | FeatureState::InTriage | FeatureState::Archived => {
            (label, theme::surface(), theme::text_muted())
        }
    }
}

pub(super) fn task_kind_label(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Bug => "Bug",
        TaskKind::Feature => "Feature",
        TaskKind::Foundation => "Foundation",
        TaskKind::Adjustment => "Adjustment",
        TaskKind::Improvement => "Improvement",
        TaskKind::Refactor => "Refactor",
    }
}

pub(super) fn task_state_badge(task_state: &str) -> (Rgba, Rgba) {
    match task_state {
        "ready" => (theme::success_soft(), theme::success()),
        "queued" | "running" => (theme::running_soft(), theme::running()),
        "done" => (theme::success_soft(), theme::text_muted()),
        _ => (theme::surface(), theme::text_muted()),
    }
}

/// What the host changed in an accepted reply (a suggestion it repaired or
/// dropped), so the developer sees the difference between what the agent said
/// and what was applied.
pub(super) fn turn_notices(turn: &FeatureTurn) -> Option<AnyElement> {
    if turn.notices.is_empty() {
        return None;
    }
    let mut notes = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::surface())
        .border_1()
        .border_color(theme::border())
        .min_w_0()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::warning())
                .child("Host adjustment"),
        );
    for (index, note) in turn.notices.iter().enumerate() {
        notes = notes.child(div().text_xs().text_color(theme::text_muted()).child(
            crate::selectable_message::SelectableMessage::new(
                format!("forge-notice:{}:{index}", turn.id),
                note,
            ),
        ));
    }
    Some(notes.into_any_element())
}

/// Explains a turn that did not complete.
///
/// `actions` is `Some` only for the newest failed turn of its kind: that one
/// gets the full card and its buttons, older failures collapse to their
/// summary line so a run of failed attempts cannot bury the conversation.
/// `retry` is what "Try again" does on this surface: a chat message in
/// Incubation, re-running the analysis in the Triage and Dependencies columns.
pub(super) fn turn_failure_notice(
    model: &AppModel,
    turn: &FeatureTurn,
    actions: Option<ikore_core::feature_forge::FailureActions>,
    retry: ForgeAction,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let failure = turn.failure.as_ref()?;
    let explanation = failure.explain();
    let copy_text = format!(
        "{}\n\n{}\n\n{}\n\n{}",
        explanation.summary, explanation.cause, explanation.remedy, failure.technical
    );
    if failure.kind == TurnFailureKind::Cancelled {
        return Some(
            label("")
                .child(crate::selectable_message::SelectableMessage::new(
                    format!("forge-cancelled:{}", turn.id),
                    explanation.summary,
                ))
                .into_any_element(),
        );
    }
    let summary =
        div()
            .flex()
            .items_start()
            .gap_2()
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::danger())
            .child(div().mt(px(2.0)).child(widgets::icon(
                Icon::TriangleAlert,
                px(14.0),
                theme::danger(),
            )))
            .child(div().flex_1().min_w_0().child(
                crate::selectable_message::SelectableMessage::new(
                    format!("forge-summary:{}", turn.id),
                    explanation.summary,
                ),
            ));
    let Some(actions) = actions else {
        return Some(summary.into_any_element());
    };
    let turn_id = turn.id.clone();
    let mut notice = div()
        .debug_selector({
            let turn_id = turn_id.clone();
            move || format!("forge-failure-{turn_id}")
        })
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(theme::danger_soft())
        .border_1()
        .border_color(theme::danger())
        .min_w_0()
        .child(summary)
        .child(
            div()
                .text_sm()
                .child(crate::selectable_message::SelectableMessage::new(
                    format!("forge-cause:{}", turn.id),
                    explanation.cause,
                )),
        )
        .child(
            label("").child(crate::selectable_message::SelectableMessage::new(
                format!("forge-remedy:{}", turn.id),
                explanation.remedy,
            )),
        );
    let mut buttons = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(copy_message_button(
            format!("forge-failure:{}", turn.id),
            copy_text,
        ));
    if actions.retry {
        buttons = buttons.child(gated_button(
            format!("retry-feature-turn-{turn_id}"),
            "Try again",
            retry,
            !model.forge.busy,
            cx,
        ));
    }
    if actions.ask_agent {
        buttons = buttons.child(gated_button(
            format!("ask-agent-about-failure-{turn_id}"),
            "Ask the agent what it needs",
            ForgeAction::AskAgentAboutFailure,
            !model.forge.busy,
            cx,
        ));
    }
    let expanded = model.forge.expanded_failures.contains(&turn_id);
    buttons = buttons.child(button(
        format!("toggle-failure-detail-{turn_id}"),
        if expanded {
            "Hide technical detail"
        } else {
            "Technical detail"
        },
        ForgeAction::ToggleFailureDetail(turn_id),
        cx,
    ));
    notice = notice.child(buttons);
    if expanded {
        notice = notice.child(
            div()
                .text_xs()
                .text_color(theme::text_subtle())
                .min_w_0()
                .child(crate::selectable_message::SelectableMessage::new(
                    format!("forge-technical:{}", turn.id),
                    failure.technical.clone(),
                )),
        );
    }
    Some(notice.into_any_element())
}

pub(crate) fn capture_overlay(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut content = card()
        .debug_selector(|| "forge-capture-content".into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .w(px(720.0))
        .max_w(relative(0.9))
        .max_h(relative(0.9))
        .child(heading_info(
            "New work",
            "capture",
            if model.forge.capture_feature {
                "Discuss the feature with the Feature Agent before defining its execution."
            } else {
                "Loose tasks go straight to the backlog; a feature goes through discovery first."
            },
        ))
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
                            "Selected:"
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
