use super::{
    agent_message_card, button, button_variant, card, eyebrow, feature_state_badge,
    feature_state_color, gated_button_variant, heading, heading_info, label, layer_short,
    mono_pill, pill, pill_icon, recessed_box, search_box, state, turn_failure_notice, turn_notices,
};
use crate::{
    AppModel,
    forge::{ForgeAction, has_dependency_cycle},
    icons::Icon,
    sizing::text_area,
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{AnyElement, Context, Div, PathBuilder, SharedString, canvas, div, point, prelude::*, px};
use ikore_core::feature_forge::{DependencyOrigin, Feature, FeatureState, TurnKind, failure_actions};
use ikore_core::llm::AgentRole;
use std::collections::HashSet;

fn dependency_analysis_card(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> Div {
    let blocked = model.forge.dependency_analysis_blocked(f);
    let agent_edges = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|edge| edge.feature_id == f.id && edge.origin == DependencyOrigin::Agent)
        .count();
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
                    "dependency-agent",
                    "The agent maps prerequisites and applies them directly. Relations it proposes are marked, and you can remove any of them.",
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(gated_button_variant(
                    "analyze-dependencies",
                    "Map prerequisites with Feature Agent",
                    ButtonVariant::Tinted,
                    ForgeAction::AnalyzeDependencies,
                    blocked.is_none(),
                    Some(Icon::Sparkles),
                    cx,
                ))
                .when(agent_edges > 0, |row| {
                    row.child(button_variant(
                        "clear-agent-dependencies",
                        format!("Remove all {agent_edges} agent-proposed relation(s)"),
                        ButtonVariant::Secondary,
                        ForgeAction::ClearAgentDependencies,
                        Some(Icon::Trash),
                        cx,
                    ))
                }),
        );
    if let Some(reason) = blocked {
        agent = agent.child(label(reason));
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
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .text_color(theme::running())
                        .child(widgets::status_dot(theme::running()))
                        .child("Feature Agent is mapping prerequisites..."),
                )
                .child(button_variant(
                    format!("cancel-dependency-turn-{}", turn.id),
                    "Cancel analysis",
                    ButtonVariant::Secondary,
                    ForgeAction::CancelTurn(turn.id.clone()),
                    Some(Icon::Square),
                    cx,
                ));
        } else if let Some(message) = &turn.assistant_message {
            agent = agent.child(agent_message_card(
                format!("forge-dependencies:{}", turn.id),
                message,
            ));
        }
        if let Some(notes) = turn_notices(turn) {
            agent = agent.child(notes);
        }
        let actions = failure_actions(&model.forge.history, TurnKind::Dependencies)
            .filter(|(id, _)| id == &turn.id)
            .map(|(_, actions)| actions);
        if let Some(notice) =
            turn_failure_notice(model, turn, actions, ForgeAction::AnalyzeDependencies, cx)
        {
            agent = agent.child(notice);
        }
    }
    agent
}

pub(super) fn dependencies(
    model: &AppModel,
    f: &Feature,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let features = &model.forge.snapshot.features;
    let feature_of = |id: &str| features.iter().find(|x| x.id == id);
    let title_of = |id: &str| {
        feature_of(id)
            .map(|x| x.title.clone())
            .unwrap_or_else(|| "Feature unavailable".into())
    };
    let prerequisites: Vec<_> = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.feature_id == f.id)
        .cloned()
        .collect();
    let dependents: Vec<_> = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.prerequisite_id == f.id)
        .filter_map(|d| feature_of(&d.feature_id))
        .filter(|dep| dep.state != FeatureState::Archived)
        .collect();

    let pending_prereqs = prerequisites
        .iter()
        .filter(|edge| {
            feature_of(&edge.prerequisite_id)
                .is_none_or(|pre| pre.state != FeatureState::Done)
        })
        .count();
    let has_cycle = has_dependency_cycle(&model.forge.snapshot.dependencies);

    let diagnostic_bar = card()
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(heading(f.title.clone()))
                        .child(if pending_prereqs > 0 {
                            pill_icon(
                                Icon::Ban,
                                format!("Blocked: {pending_prereqs} pending prerequisite(s)"),
                                theme::warning_soft(),
                                theme::warning(),
                            )
                        } else if !prerequisites.is_empty() {
                            pill_icon(
                                Icon::Check,
                                format!("Prerequisites satisfied ({})", prerequisites.len()),
                                theme::success_soft(),
                                theme::success(),
                            )
                        } else {
                            pill("No prerequisites", theme::surface(), theme::text_muted())
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(button_variant(
                            "dependency-focus-toggle",
                            if model.forge.dependencies_focus_mode {
                                "View: Focused path"
                            } else {
                                "View: Full graph"
                            },
                            if model.forge.dependencies_focus_mode {
                                ButtonVariant::Selected
                            } else {
                                ButtonVariant::Secondary
                            },
                            ForgeAction::ToggleDependencyFocusMode,
                            Some(Icon::Eye),
                            cx,
                        ))
                        .child(if has_cycle {
                            pill_icon(
                                Icon::TriangleAlert,
                                "Cycle detected",
                                theme::danger_soft(),
                                theme::danger(),
                            )
                        } else {
                            pill_icon(
                                Icon::Check,
                                "0 cycles",
                                theme::success_soft(),
                                theme::success(),
                            )
                        }),
                ),
        );

    let mut list = div().flex().flex_col().gap_2();
    if prerequisites.is_empty() {
        list = list.child(label(
            "No prerequisites yet. Add one on the right or ask the Feature Agent.",
        ));
    }
    let agent_fg = theme::agent_color(AgentRole::FeatureAgent);
    let agent_bg = theme::agent_soft_color(AgentRole::FeatureAgent);
    for edge in &prerequisites {
        let is_agent = edge.origin == DependencyOrigin::Agent;
        let prereq_feature = feature_of(&edge.prerequisite_id);
        let prereq_done = prereq_feature.is_some_and(|pre| pre.state == FeatureState::Done);
        let prereq_state_label = prereq_feature
            .map(|pre| {
                if pre.state == FeatureState::Done {
                    "Done".to_owned()
                } else {
                    format!("{} (Pending)", state(pre.state))
                }
            })
            .unwrap_or_else(|| "Unavailable".to_owned());

        list = list.child(
            recessed_box()
                .border_color(if prereq_done {
                    theme::border()
                } else if is_agent {
                    theme::warning()
                } else {
                    theme::border_strong()
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_start()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(title_of(&edge.prerequisite_id)),
                                )
                                .child(if prereq_done {
                                    pill_icon(
                                        Icon::Check,
                                        prereq_state_label,
                                        theme::success_soft(),
                                        theme::success(),
                                    )
                                } else {
                                    pill(
                                        prereq_state_label,
                                        theme::warning_soft(),
                                        theme::warning(),
                                    )
                                })
                                .child(if is_agent {
                                    pill("Feature Agent (Active)", agent_bg, agent_fg)
                                } else {
                                    pill("Human", theme::accent_soft(), theme::accent())
                                }),
                        )
                        .child(
                            widgets::button_frame(
                                div()
                                    .id(SharedString::from(format!(
                                        "remove-edge-{}",
                                        edge.prerequisite_id
                                    )))
                                    .debug_selector({
                                        let id = format!("remove-edge-{}", edge.prerequisite_id);
                                        move || id.clone()
                                    })
                                    .tab_index(0),
                                ButtonVariant::Ghost,
                                true,
                            )
                            .gap_2()
                            .child(widgets::icon(Icon::Trash, px(14.0), theme::text_muted()))
                            .child("Remove")
                            .on_click({
                                let prerequisite = edge.prerequisite_id.clone();
                                cx.listener(move |model, _, _, cx| {
                                    model.forge_action(
                                        ForgeAction::Dependency(prerequisite.clone(), false),
                                        cx,
                                    )
                                })
                            }),
                        ),
                )
                .when(!edge.rationale.trim().is_empty(), |row| {
                    row.child(label(edge.rationale.clone()))
                }),
        );
    }

    let mut suggestions = div().flex().flex_col().gap_2();
    let mut has_suggestions = false;
    let mut pending_suggestion_ids: Vec<String> = Vec::new();
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
            if let Some(pre) = features.iter().find(|x| x.id == edge.prerequisite_id) {
                has_suggestions = true;
                pending_suggestion_ids.push(pre.id.clone());
                suggestions = suggestions.child(
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(theme::warning_soft())
                        .border_1()
                        .border_color(theme::warning())
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::warning())
                                        .child(widgets::icon(
                                            Icon::Sparkles,
                                            px(12.0),
                                            theme::warning(),
                                        ))
                                        .child(format!("Suggested prerequisite: {}", pre.title)),
                                )
                                .child(label(edge.rationale.clone())),
                        )
                        .child(button_variant(
                            format!("accept-suggestion-{}", pre.id),
                            "Confirm",
                            ButtonVariant::Primary,
                            ForgeAction::ConfirmSuggestion(
                                edge.prerequisite_id.clone(),
                                edge.rationale.clone(),
                            ),
                            Some(Icon::Check),
                            cx,
                        )),
                );
            }
        }
    }

    let cand_query = model
        .forge
        .dependency_search
        .read(cx)
        .value()
        .trim()
        .to_ascii_lowercase();
    let candidates: Vec<_> = features
        .iter()
        .filter(|x| {
            x.id != f.id
                && x.state != FeatureState::Archived
                && !model
                    .forge
                    .snapshot
                    .dependencies
                    .iter()
                    .any(|e| e.feature_id == f.id && e.prerequisite_id == x.id)
                && (cand_query.is_empty()
                    || x.title.to_ascii_lowercase().contains(&cand_query)
                    || layer_short(x.structural_level)
                        .to_ascii_lowercase()
                        .contains(&cand_query)
                    || state(x.state).to_ascii_lowercase().contains(&cand_query))
        })
        .collect();
    let mut chips = div().flex().flex_wrap().gap_2();
    for other in &candidates {
        chips = chips.child(button(
            format!("add-edge-{}", other.id),
            format!(
                "+ [{}] {} · {}",
                layer_short(other.structural_level),
                other.title,
                state(other.state)
            ),
            ForgeAction::Dependency(other.id.clone(), true),
            cx,
        ));
    }
    if candidates.is_empty() {
        chips = chips.child(label(if cand_query.is_empty() {
            "No other candidate features available."
        } else {
            "No candidate features match your search."
        }));
    }

    let left_col = card()
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
                .child(heading(format!("{} waits for", f.title)))
                .child(mono_pill(
                    prerequisites.len().to_string(),
                    theme::surface(),
                    theme::text_muted(),
                )),
        )
        .child(list)
        .when(!dependents.is_empty(), |c| {
            let mut dep_chips = div().flex().flex_wrap().gap_2();
            for dep in &dependents {
                dep_chips = dep_chips.child(button_variant(
                    format!("unblocks-feature-{}", dep.id),
                    dep.title.clone(),
                    ButtonVariant::Secondary,
                    ForgeAction::SelectFeature(dep.id.clone()),
                    Some(Icon::ArrowRight),
                    cx,
                ));
            }
            c.child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(eyebrow(format!("UNBLOCKS ({})", dependents.len())))
                    .child(dep_chips),
            )
        });

    let right_col = card()
        .flex_1()
        .min_w(px(280.0))
        .border_1()
        .border_color(theme::border())
        .when(has_suggestions, |c| {
            c.child(eyebrow("PENDING AGENT SUGGESTIONS"))
                .child(suggestions)
        })
        .child(heading_info(
            "Add a prerequisite",
            "add-prerequisite",
            "Write why, then pick the feature that must ship first.",
        ))
        .child(search_box(model.forge.dependency_search.clone()))
        .child(text_area(model.forge.rationale.clone()))
        .child(chips);

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(diagnostic_bar)
        .child(
            card()
                .border_1()
                .border_color(theme::border())
                .child(feature_graph(model, f, &pending_suggestion_ids, cx)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_3()
                .child(left_col)
                .child(right_col),
        )
        .child(dependency_analysis_card(model, f, cx))
        .into_any_element()
}

/// Evaluate cubic Bezier at parameter `t`.
fn cubic_point(
    p0: gpui::Point<gpui::Pixels>,
    c1: gpui::Point<gpui::Pixels>,
    c2: gpui::Point<gpui::Pixels>,
    p1: gpui::Point<gpui::Pixels>,
    t: f32,
) -> gpui::Point<gpui::Pixels> {
    let u = 1.0 - t;
    let tt = t * t;
    let uu = u * u;
    let uuu = uu * u;
    let ttt = tt * t;
    let x = f32::from(p0.x) * uuu
        + 3.0 * f32::from(c1.x) * uu * t
        + 3.0 * f32::from(c2.x) * u * tt
        + f32::from(p1.x) * ttt;
    let y = f32::from(p0.y) * uuu
        + 3.0 * f32::from(c1.y) * uu * t
        + 3.0 * f32::from(c2.y) * u * tt
        + f32::from(p1.y) * ttt;
    point(px(x), px(y))
}

#[derive(Clone, Copy)]
enum GraphEdgeKind {
    Human,
    AgentActive,
    AgentSuggested,
}

/// The feature map: one column per structural level, each feature a 2-line node,
/// and curved connectors from a feature to every prerequisite.
fn feature_graph(
    model: &AppModel,
    selected: &Feature,
    pending_suggestion_ids: &[String],
    cx: &mut Context<AppModel>,
) -> AnyElement {
    const LEVELS: [&str; 4] = ["Foundation", "Domain", "Interface", "Optimization"];
    const NODE_H: f32 = 62.0;
    const ROW: f32 = 76.0;
    const TOP: f32 = 32.0;
    let measured = model.forge.graph_bounds.clone();
    let width = f32::from(measured.get().size.width);
    let pitch = if width > 0.0 {
        (width / 4.0).max(145.0)
    } else {
        180.0
    };
    let node_w = pitch - 24.0;
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
            (f.clone(), level as f32 * pitch, TOP + row as f32 * ROW)
        })
        .collect::<Vec<_>>();
    let height = counts.into_iter().max().unwrap_or(1).max(1) as f32 * ROW + TOP + 8.0;

    let mut related_ids: HashSet<String> = HashSet::new();
    related_ids.insert(selected.id.clone());
    for edge in &model.forge.snapshot.dependencies {
        if edge.feature_id == selected.id {
            related_ids.insert(edge.prerequisite_id.clone());
        }
        if edge.prerequisite_id == selected.id {
            related_ids.insert(edge.feature_id.clone());
        }
    }
    for pre_id in pending_suggestion_ids {
        related_ids.insert(pre_id.clone());
    }
    let focus_mode = model.forge.dependencies_focus_mode;

    let mut edges: Vec<(f32, f32, f32, f32, bool, GraphEdgeKind, bool)> = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter_map(|e| {
            let (_, x, y) = nodes.iter().find(|(f, _, _)| f.id == e.feature_id)?;
            let (_, tx, ty) = nodes.iter().find(|(f, _, _)| f.id == e.prerequisite_id)?;
            let same_column = (tx - x).abs() < 1.0;
            let (sx, ex) = if same_column {
                (*x + node_w, *tx + node_w)
            } else if tx < x {
                (*x, *tx + node_w)
            } else {
                (*x + node_w, *tx)
            };
            let highlighted = e.feature_id == selected.id || e.prerequisite_id == selected.id;
            let kind = match e.origin {
                DependencyOrigin::Human => GraphEdgeKind::Human,
                DependencyOrigin::Agent => GraphEdgeKind::AgentActive,
            };
            Some((
                sx,
                *y + NODE_H / 2.0,
                ex,
                *ty + NODE_H / 2.0,
                same_column,
                kind,
                highlighted,
            ))
        })
        .collect();

    if let Some((_, sx_node, sy_node)) = nodes.iter().find(|(f, _, _)| f.id == selected.id) {
        for pre_id in pending_suggestion_ids {
            if let Some((_, tx_node, ty_node)) = nodes.iter().find(|(f, _, _)| f.id == *pre_id) {
                let same_column = (tx_node - sx_node).abs() < 1.0;
                let (sx, ex) = if same_column {
                    (*sx_node + node_w, *tx_node + node_w)
                } else if tx_node < sx_node {
                    (*sx_node, *tx_node + node_w)
                } else {
                    (*sx_node + node_w, *tx_node)
                };
                edges.push((
                    sx,
                    *sy_node + NODE_H / 2.0,
                    ex,
                    *ty_node + NODE_H / 2.0,
                    same_column,
                    GraphEdgeKind::AgentSuggested,
                    true,
                ));
            }
        }
    }

    let has_pending_edges = !pending_suggestion_ids.is_empty();
    let agent_color = theme::agent_color(AgentRole::FeatureAgent);

    let mut graph = div().relative().w_full().h(px(height)).child(
        canvas(
            move |bounds, _, _| {
                measured.set(bounds);
                (edges, focus_mode)
            },
            move |bounds, (edges, focus_mode), window, _| {
                for (sx, sy, ex, ey, same_column, kind, highlighted) in edges {
                    let a = point(bounds.left() + px(sx), bounds.top() + px(sy));
                    let b = point(bounds.left() + px(ex), bounds.top() + px(ey));
                    let dir = if same_column || ex < sx { -1.0 } else { 1.0 };
                    let (c1, c2) = if same_column {
                        let out = 22.0 + (ey - sy).abs() * 0.15;
                        (point(a.x + px(out), a.y), point(b.x + px(out), b.y))
                    } else {
                        let bend = ((ex - sx).abs() * 0.5).max(24.0);
                        (
                            point(a.x + px(bend * dir), a.y),
                            point(b.x - px(bend * dir), b.y),
                        )
                    };
                    let mut path = PathBuilder::stroke(px(if highlighted { 1.75 } else { 1.25 }));
                    if matches!(kind, GraphEdgeKind::AgentSuggested) {
                        const SEGMENTS: usize = 16;
                        for step in (0..SEGMENTS).step_by(2) {
                            let t0 = step as f32 / SEGMENTS as f32;
                            let t1 = (step + 1) as f32 / SEGMENTS as f32;
                            path.move_to(cubic_point(a, c1, c2, b, t0));
                            path.line_to(cubic_point(a, c1, c2, b, t1));
                        }
                    } else {
                        path.move_to(a);
                        path.cubic_bezier_to(b, c1, c2);
                    }
                    // Arrowhead at the prerequisite.
                    path.move_to(b);
                    path.line_to(point(b.x - px(7.0 * dir), b.y - px(4.0)));
                    path.move_to(b);
                    path.line_to(point(b.x - px(7.0 * dir), b.y + px(4.0)));
                    let mut color = match (kind, highlighted) {
                        (GraphEdgeKind::AgentSuggested, _) => theme::warning(),
                        (GraphEdgeKind::AgentActive, _) => agent_color,
                        (GraphEdgeKind::Human, true) => theme::accent(),
                        (GraphEdgeKind::Human, false) => theme::border_strong(),
                    };
                    if focus_mode && !highlighted {
                        color.a = 0.28;
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
                    }
                }
            },
        )
        .absolute()
        .size_full(),
    );

    for (i, title) in LEVELS.iter().enumerate() {
        graph = graph.child(
            div()
                .absolute()
                .left(px(i as f32 * pitch))
                .top_0()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_subtle())
                .child(title.to_uppercase()),
        );
        if counts[i] == 0 {
            graph = graph.child(
                div()
                    .absolute()
                    .left(px(i as f32 * pitch))
                    .top(px(TOP))
                    .w(px(node_w))
                    .h(px(NODE_H))
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::surface())
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child("0 features"),
            );
        }
    }

    for (f, x, y) in nodes {
        let is_selected = f.id == selected.id;
        let dimmed = focus_mode && !related_ids.contains(&f.id);
        let (state_text, state_bg, state_fg) = feature_state_badge(f.state);
        let ice_text = f
            .evaluation
            .score()
            .map(|s| format!("ICE {s:.1}"))
            .unwrap_or_else(|| "Unscored".into());
        let id = f.id.clone();
        graph = graph.child(
            div()
                .id(SharedString::from(format!("graph-node-{}", f.id)))
                .debug_selector({
                    let id = format!("graph-node-{}", f.id);
                    move || id.clone()
                })
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(node_w))
                .h(px(NODE_H))
                .p_2()
                .overflow_hidden()
                .flex()
                .flex_col()
                .justify_between()
                .rounded_md()
                .border_1()
                .border_color(if is_selected {
                    theme::accent()
                } else {
                    theme::border_strong()
                })
                .bg(if is_selected {
                    theme::accent_soft()
                } else {
                    theme::surface_raised()
                })
                .when(dimmed, |node| node.opacity(0.42))
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.forge_action(ForgeAction::SelectFeature(id.clone()), cx)
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(widgets::status_dot(feature_state_color(f.state)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(f.title),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_1()
                        .min_w_0()
                        .child(pill(state_text, state_bg, state_fg))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .text_color(if is_selected {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                })
                                .child(ice_text),
                        ),
                ),
        );
    }

    div()
        .id("feature-graph")
        .debug_selector(|| "feature-graph".into())
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(graph)
        .child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(theme::border())
                .flex()
                .flex_wrap()
                .items_center()
                .gap_4()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Arrows point to prerequisites")
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(widgets::status_dot(theme::accent()))
                        .child("Human confirmed"),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(widgets::status_dot(agent_color))
                        .child("Feature Agent (active)"),
                )
                .when(has_pending_edges, |legend| {
                    legend.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(widgets::status_dot(theme::warning()))
                            .child("Agent suggestion (pending)"),
                    )
                }),
        )
        .into_any_element()
}
