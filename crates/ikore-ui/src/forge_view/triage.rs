use super::{
    agent_message_card, button, button_variant, card, eyebrow, feature_state_badge, gated_button,
    gated_button_variant, heading, heading_info, incubation::feature_start_card, label, mono_pill,
    pill, pill_icon, recessed_box, turn_failure_notice,
};
use crate::{
    AppModel,
    forge::ForgeAction,
    icons::Icon,
    sizing::{StyledExt, text_area},
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{AnyElement, Context, Div, SharedString, div, prelude::*, px};
use ikore_core::feature_forge::{Feature, FeatureState, TurnKind, failure_actions};
use ikore_core::llm::AgentRole;

fn ice_scale_card(
    metric_index: usize,
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    value: u8,
    anchors: [&'static str; 5],
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let clamped = value.clamp(1, 5);
    let anchor = anchors[(clamped - 1) as usize];
    let mut buttons = div().flex().items_center().gap_1();
    for n in 1u8..=5 {
        let active = clamped == n;
        let btn_id = SharedString::from(format!("{id}-{n}"));
        let debug_id = btn_id.clone();
        buttons = buttons.child(
            div()
                .id(btn_id)
                .debug_selector(move || debug_id.to_string())
                .tab_index(0)
                .flex_1()
                .h(px(30.0))
                .rounded_md()
                .border_1()
                .border_color(if active {
                    theme::accent()
                } else {
                    theme::border_strong()
                })
                .bg(if active {
                    theme::accent()
                } else {
                    theme::surface_raised()
                })
                .text_color(if active {
                    theme::canvas()
                } else {
                    theme::text_muted()
                })
                .font_family(theme::FONT_MONO)
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .when(!active, |btn| {
                    btn.hover(|s| s.bg(theme::surface_hover()).text_color(theme::text()))
                })
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.forge_action(ForgeAction::SetRating(metric_index, n), cx)
                }))
                .child(n.to_string()),
        );
    }

    recessed_box()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex_1()
        .min_w(px(190.0))
        .p_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .child(mono_pill(
                    format!("{clamped} - {anchor}"),
                    theme::accent_soft(),
                    theme::accent(),
                )),
        )
        .child(buttons)
        .child(
            div()
                .text_xs()
                .text_color(theme::text_subtle())
                .child(subtitle),
        )
        .into_any_element()
}

/// The control that asks for an analysis. Stays in the main column, because the
/// analysis column is what it opens.
fn triage_analysis_trigger(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> Div {
    let blocked = model.forge.analysis_blocked(f);
    let has_triage_turn = model
        .forge
        .history
        .iter()
        .any(|turn| turn.kind == TurnKind::Triage);
    let agent_fg = theme::agent_color(AgentRole::FeatureAgent);
    let agent_bg = theme::agent_soft_color(AgentRole::FeatureAgent);

    let mut agent = card()
        .border_1()
        .border_color(agent_bg)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(widgets::icon(Icon::Bot, px(16.0), agent_fg))
                .child(heading_info(
                    "Feature Agent analysis",
                    "triage-agent",
                    "The agent answers the four screens, sets the ratings and the structural level, then recommends approving or archiving.",
                )),
        );

    if model.forge.pending_triage_overwrite {
        agent = agent
            .child(
                div()
                    .text_sm()
                    .text_color(theme::warning())
                    .child("Your answers on screen differ from what is saved. Running the analysis replaces them."),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(button_variant(
                        "confirm-analyze-triage",
                        "Replace with the analysis",
                        ButtonVariant::Primary,
                        ForgeAction::ConfirmAnalyzeTriage,
                        Some(Icon::Check),
                        cx,
                    ))
                    .child(button_variant(
                        "cancel-analyze-triage",
                        "Keep my answers",
                        ButtonVariant::Secondary,
                        ForgeAction::CancelAnalyzeTriage,
                        None,
                        cx,
                    )),
            );
    } else {
        // Keep `analyze-triage` anchored in its row above any blocker explanation
        // so toggling `blocked` never moves its bounds (`forge_analysis_button_keeps_its_geometry_when_gated`).
        agent = agent.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(gated_button_variant(
                    "analyze-triage",
                    "Analyze with Feature Agent",
                    ButtonVariant::Tinted,
                    ForgeAction::AnalyzeTriage,
                    blocked.is_none(),
                    Some(Icon::Sparkles),
                    cx,
                ))
                .when(has_triage_turn && !model.forge.triage_aside_open, |row| {
                    row.child(button_variant(
                        "open-triage-aside",
                        "Show verdict",
                        ButtonVariant::Secondary,
                        ForgeAction::OpenTriageAside,
                        Some(Icon::Eye),
                        cx,
                    ))
                }),
        );
        if let Some(reason) = blocked {
            agent = agent.child(label(reason));
        }
    }
    agent
}

/// The analysis column: whatever the last triage turn produced.
pub(super) fn triage_aside(
    model: &AppModel,
    f: &Feature,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agent_fg = theme::agent_color(AgentRole::FeatureAgent);
    let mut agent = card()
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(widgets::icon(Icon::Bot, px(16.0), agent_fg))
                        .child(heading("Feature Agent analysis")),
                )
                .child(button_variant(
                    "close-triage-aside",
                    "Close",
                    ButtonVariant::Ghost,
                    ForgeAction::CloseTriageAside,
                    Some(Icon::X),
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(theme::running())
                            .child(widgets::status_dot(theme::running()))
                            .child("Feature Agent is evaluating this feature..."),
                    )
                    .child(button_variant(
                        format!("cancel-triage-turn-{}", turn.id),
                        "Cancel analysis",
                        ButtonVariant::Secondary,
                        ForgeAction::CancelTurn(turn.id.clone()),
                        Some(Icon::Square),
                        cx,
                    ));
            } else if let Some(message) = &turn.assistant_message {
                agent = agent.child(agent_message_card(
                    format!("forge-triage:{}", turn.id),
                    message,
                ));
            }
            let actions = failure_actions(&model.forge.history, TurnKind::Triage)
                .filter(|(id, _)| id == &turn.id)
                .map(|(_, actions)| actions);
            if let Some(notice) =
                turn_failure_notice(model, turn, actions, ForgeAction::AnalyzeTriage, cx)
            {
                agent = agent.child(notice);
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

pub(super) fn triage_main(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let (state_label, state_bg, state_fg) = feature_state_badge(f.state);
    let live_score = f64::from(model.forge.impact) * f64::from(model.forge.confidence)
        / f64::from(model.forge.effort.max(1));
    let summary = if !f.brief.objective.trim().is_empty() {
        f.brief.objective.clone()
    } else {
        f.original_idea.clone()
    };

    let mut level_track = widgets::segment_track(
        div()
            .id("structural-level")
            .debug_selector(|| "structural-level".into())
            .self_start()
            .flex_wrap(),
    );
    for idx in 0u8..4 {
        let level_name = super::layer_label(idx);
        let selected = model.forge.level == idx;
        let seg_id = SharedString::from(format!("structural-level-{idx}"));
        let seg_debug = seg_id.clone();
        level_track = level_track.child(
            widgets::segment(
                div()
                    .id(seg_id)
                    .debug_selector(move || seg_debug.to_string())
                    .tab_index(0),
                selected,
            )
            .text_xs()
            .on_click(cx.listener(move |model, _, _, cx| {
                model.forge_action(ForgeAction::SetLevel(idx), cx)
            }))
            .child(level_name),
        );
    }

    let header_card = card()
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(220.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(pill(state_label, state_bg, state_fg))
                                .child(mono_pill(
                                    format!("rev {}", f.revision),
                                    theme::surface(),
                                    theme::text_muted(),
                                )),
                        )
                        .child(heading_info(
                            f.title.clone(),
                            "triage",
                            "Evaluation guides priority. You decide whether the feature moves forward.",
                        ))
                        .child(label(summary)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(eyebrow("STRUCTURAL LEVEL"))
                        .child(level_track),
                ),
        )
        .child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(theme::border())
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_3()
                        .child(mono_pill(
                            format!("{live_score:.1} ICE"),
                            theme::accent_soft(),
                            theme::accent(),
                        ))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!(
                                    "Formula: (Impact {} x Confidence {}) / Effort {} = {:.2}",
                                    model.forge.impact,
                                    model.forge.confidence,
                                    model.forge.effort,
                                    live_score
                                )),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_subtle())
                        .child("Relative priority guide -- your decision is final"),
                ),
        );

    let ice_card = card()
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(widgets::icon(
                    Icon::SlidersHorizontal,
                    px(15.0),
                    theme::accent(),
                ))
                .child(heading_info(
                    "ICE parameters",
                    "ratings",
                    "Select 1 to 5 directly. Score = impact x confidence / effort.",
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .child(ice_scale_card(
                    0,
                    "rating-impact",
                    "Impact",
                    "Direct value for users and workflow",
                    model.forge.impact,
                    ["Minimal", "Low", "Moderate", "High", "Maximum"],
                    cx,
                ))
                .child(ice_scale_card(
                    1,
                    "rating-confidence",
                    "Confidence",
                    "Clarity of scope and supporting evidence",
                    model.forge.confidence,
                    [
                        "Speculative",
                        "Hypothesis",
                        "Estimated",
                        "Validated",
                        "Proven",
                    ],
                    cx,
                ))
                .child(ice_scale_card(
                    2,
                    "rating-effort",
                    "Effort",
                    "Implementation complexity and regression risk",
                    model.forge.effort,
                    ["Trivial", "Simple", "Moderate", "Complex", "Heavy"],
                    cx,
                )),
        );

    let dimension_specs = [
        ("1. Value and need", "Impact"),
        ("2. Product alignment", "Strategy"),
        ("3. Feasibility and engineering", "Engineering"),
        ("4. Usability and adoption", "Adoption"),
    ];
    let mut dimensions_grid = div().flex().flex_wrap().gap_3();
    for (i, (name, tag)) in dimension_specs.into_iter().enumerate() {
        dimensions_grid = dimensions_grid.child(
            card()
                .flex_1()
                .min_w(px(280.0))
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(mono_pill(tag, theme::surface(), theme::text_subtle())),
                )
                .child(text_area(model.forge.filters[i].clone())),
        );
    }

    let rationale_card = card()
        .border_1()
        .border_color(theme::border())
        .child(eyebrow("DECISION RATIONALE"))
        .child(label(
            "Used when archiving this feature or dismissing a blocking conformance warning.",
        ))
        .child(text_area(model.forge.rationale.clone()));

    let mut decision_bar = card().border_1().border_color(theme::border());
    if f.state == FeatureState::Archived {
        decision_bar = decision_bar.child(button_variant(
            "reopen-feature",
            "Reopen feature",
            ButtonVariant::Primary,
            ForgeAction::Reopen,
            Some(Icon::RotateCw),
            cx,
        ));
    } else {
        // Approval is once per revision: the tasks it created already exist and
        // may be in delivery. The core declines the repeat, so the control says
        // so instead of offering an action that does nothing.
        let already_approved = f.approved_revision == Some(f.revision);
        // ADR-034 M6: the host refuses an approval the pre-flight blocks.
        let preflight_block = model.forge.preflight_block(f);
        decision_bar = decision_bar
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(button_variant(
                        "archive-feature",
                        "Archive with rationale",
                        ButtonVariant::Danger,
                        ForgeAction::Archive,
                        Some(Icon::Archive),
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(button_variant(
                                "save-evaluation",
                                "Save evaluation",
                                ButtonVariant::Secondary,
                                ForgeAction::Evaluate,
                                Some(Icon::Save),
                                cx,
                            ))
                            .child(gated_button_variant(
                                "triage-approve",
                                "Approve scope and tasks",
                                ButtonVariant::Primary,
                                ForgeAction::Approve,
                                !already_approved && preflight_block.is_none(),
                                Some(Icon::Check),
                                cx,
                            )),
                    ),
            )
            .child(feature_start_card(model, f, cx));
        if let Some(reason) = preflight_block {
            decision_bar = decision_bar.child(
                div()
                    .debug_selector(|| "preflight-approval-blocked".into())
                    .p_2()
                    .rounded_md()
                    .bg(theme::warning_soft())
                    .border_1()
                    .border_color(theme::warning())
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(div().mt(px(2.0)).child(widgets::icon(
                        Icon::TriangleAlert,
                        px(12.0),
                        theme::warning(),
                    )))
                    .child(div().flex_1().min_w_0().child(reason)),
            );
        }
        if already_approved {
            decision_bar = decision_bar.child(label(
                "This revision is already approved and its tasks exist. Revise the scope in Incubation to produce a new revision.",
            ));
        }
    }

    let mut root = div().flex().flex_col().gap_3().child(header_card);
    if let Some(card) = preflight_card(model, f, cx) {
        root = root.child(card);
    }
    root.child(ice_card)
        .child(triage_analysis_trigger(model, f, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(eyebrow("FOUR EVALUATION DIMENSIONS"))
                .child(dimensions_grid),
        )
        .child(rationale_card)
        .child(decision_bar)
        .into_any_element()
}

/// ADR-034 M6: what the triage found when it compared the feature with the
/// project's accepted decisions, and the three decisions on a blocking one.
fn preflight_card(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> Option<Div> {
    use ikore_core::feature_forge::preflight::{ConflictSeverity, PreflightResolution};
    let finding = model.forge.preflight_for(f)?;
    let current = finding.feature_revision == f.revision;
    let blocking_active =
        finding.blocking > 0 && finding.resolution != PreflightResolution::Dismissed;
    let (bg, border, icon_glyph, icon_color, summary_title) = if blocking_active {
        (
            theme::danger_soft(),
            theme::danger(),
            Icon::TriangleAlert,
            theme::danger(),
            format!(
                "Architecture conformance: {} blocking conflict(s)",
                finding.blocking
            ),
        )
    } else if !finding.conflicts.is_empty() && finding.resolution != PreflightResolution::Dismissed
    {
        (
            theme::surface_raised(),
            theme::warning(),
            Icon::TriangleAlert,
            theme::warning(),
            "Architecture conformance: Advisory notes".to_owned(),
        )
    } else {
        (
            theme::success_soft(),
            theme::success(),
            Icon::ShieldCheck,
            theme::success(),
            if finding.resolution == PreflightResolution::Dismissed {
                "Architecture conformance: Dismissed".to_owned()
            } else {
                "Architecture conformance: Clear".to_owned()
            },
        )
    };

    let mut card_el = div()
        .debug_selector(|| "preflight-card".into())
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_lg()
        .bg(bg)
        .border_1()
        .border_color(border)
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(widgets::icon(icon_glyph, px(16.0), icon_color))
                        .child(heading_info(
                            summary_title,
                            "preflight",
                            "The triage compares the feature with PROJECT_AGENT.md and the accepted decision notes. A blocking conflict stops approval until you decide.",
                        )),
                )
                .child(mono_pill(
                    format!(
                        "{} · {} file(s)",
                        finding.inputs.state.as_str(),
                        finding.inputs.delivered().count()
                    ),
                    theme::surface(),
                    icon_color,
                )),
        );

    if !current {
        card_el = card_el.child(pill_icon(
            Icon::TriangleAlert,
            format!(
                "From revision {}, run the triage again",
                finding.feature_revision
            ),
            theme::warning_soft(),
            theme::warning(),
        ));
    }
    if finding.conflicts.is_empty() {
        card_el = card_el.child(
            div()
                .text_sm()
                .text_color(theme::text())
                .child("No conflict with the delivered decisions."),
        );
    }
    for (index, conflict) in finding.conflicts.iter().enumerate() {
        let selector = format!("preflight-conflict-{index}");
        let is_blocking = conflict.severity == ConflictSeverity::Blocking;
        let path_str = finding
            .decision_path(conflict)
            .unwrap_or(&conflict.decision_ref)
            .to_owned();
        card_el = card_el.child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .p_2()
                .rounded_md()
                .bg(theme::surface())
                .border_1()
                .border_color(if is_blocking {
                    theme::danger()
                } else {
                    theme::warning()
                })
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(pill(
                            if is_blocking { "Blocking" } else { "Advisory" },
                            if is_blocking {
                                theme::danger_soft()
                            } else {
                                theme::warning_soft()
                            },
                            if is_blocking {
                                theme::danger()
                            } else {
                                theme::warning()
                            },
                        ))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(path_str),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text())
                        .child(conflict.conflict.clone()),
                ),
        );
    }
    match finding.resolution {
        PreflightResolution::Dismissed => {
            card_el = card_el.child(div().text_sm().text_color(theme::text()).child(format!(
                "Dismissed: {}",
                finding.reason.as_deref().unwrap_or("")
            )));
        }
        PreflightResolution::Adjusted => {
            card_el = card_el.child(label(
                "You chose to adjust the feature; triage the new revision.",
            ));
        }
        PreflightResolution::AdrProposed => {
            card_el = card_el.child(label("A feature proposing the decision was created."));
        }
        PreflightResolution::None | PreflightResolution::Pending => {}
    }
    if current && finding.blocking > 0 && finding.resolution != PreflightResolution::Dismissed {
        card_el = card_el.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(button(
                    "preflight-adjust",
                    "Adjust the feature",
                    ForgeAction::PreflightAdjust,
                    cx,
                ))
                .child(gated_button(
                    "preflight-propose-adr",
                    "Propose a decision",
                    ForgeAction::PreflightProposeAdr,
                    finding.resolution != PreflightResolution::AdrProposed,
                    cx,
                ))
                .child(button(
                    "preflight-dismiss",
                    "Dismiss with the rationale below",
                    ForgeAction::PreflightDismiss,
                    cx,
                )),
        );
    }
    Some(card_el)
}
