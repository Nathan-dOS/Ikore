use super::{
    button, button_variant, card, feature_state_color, heading_info, label, layer_short, mono_pill,
    pill, pill_dot, state,
};
use crate::{
    AppModel,
    forge::ForgeAction,
    icons::Icon,
    pulsations::{PIPELINE_STAGES, stage_label},
    sizing::{StyledExt, sized_input},
    theme,
    widgets::{self, ButtonVariant},
};
use gpui::{AnyElement, Context, SharedString, div, prelude::*, px};
use ikore_core::feature_forge::{Feature, FeatureState, Wave};
use ikore_core::persistence_service::PulsationStatus;

fn wave_stage_label(model: &AppModel, feature: &Feature, cx: &Context<AppModel>) -> Option<String> {
    let task_pulse_id = model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| t.draft.feature_id.as_ref() == Some(&feature.id))
        .find_map(|t| t.active_pulsation_id.as_ref())?;
    let pulse = model
        .pulsations
        .read(cx)
        .pulsations
        .iter()
        .find(|p| &p.id == task_pulse_id && p.status == PulsationStatus::Running)?;
    let stage_index = PIPELINE_STAGES
        .iter()
        .position(|s| *s == pulse.stage)
        .map(|i| i + 1)
        .unwrap_or(1);
    Some(format!("{} · {stage_index}/6", stage_label(pulse.stage)))
}

fn feature_wave_dependencies(model: &AppModel, f: &Feature) -> (Vec<String>, usize) {
    let mut waiting_on = Vec::new();
    for edge in model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.feature_id == f.id)
    {
        if let Some(pre) = model
            .forge
            .snapshot
            .features
            .iter()
            .find(|x| x.id == edge.prerequisite_id)
            && pre.state != FeatureState::Done
        {
            waiting_on.push(pre.title.clone());
        }
    }
    let unblocks = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.prerequisite_id == f.id)
        .filter(|d| {
            model
                .forge
                .snapshot
                .features
                .iter()
                .find(|x| x.id == d.feature_id)
                .is_some_and(|dep| dep.state != FeatureState::Archived)
        })
        .count();
    (waiting_on, unblocks)
}

pub(super) fn waves(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    const WAVES: [(Wave, &str); 3] = [
        (Wave::Current, "Current wave"),
        (Wave::Next, "Next wave"),
        (Wave::Horizon, "Horizon"),
    ];
    let limit = model.forge.snapshot.current_wave_limit;
    let planned_features: Vec<_> = model
        .forge
        .snapshot
        .features
        .iter()
        .filter(|f| f.wave.is_some() && f.state != FeatureState::Archived)
        .collect();
    let current_count = planned_features
        .iter()
        .filter(|f| f.wave == Some(Wave::Current))
        .count();
    let total_planned_effort: u64 = planned_features
        .iter()
        .filter_map(|f| f.evaluation.effort.map(u64::from))
        .sum();
    let has_any_effort = planned_features
        .iter()
        .any(|f| f.evaluation.effort.is_some());

    let (wip_label, wip_dot, wip_bg, wip_fg) = if current_count as u64 > u64::from(limit) {
        (
            format!("Over capacity (WIP {current_count}/{limit})"),
            theme::danger(),
            theme::danger_soft(),
            theme::danger(),
        )
    } else if current_count as u64 == u64::from(limit) {
        (
            format!("At capacity (WIP {current_count}/{limit})"),
            theme::accent(),
            theme::accent_soft(),
            theme::accent(),
        )
    } else {
        (
            format!("Available capacity (WIP {current_count}/{limit})"),
            theme::success(),
            theme::success_soft(),
            theme::success(),
        )
    };

    let mut columns = div().flex().items_start().gap_3().min_w_0();
    for (wave, title) in WAVES {
        let members: Vec<_> = model
            .forge
            .snapshot
            .features
            .iter()
            .filter(|f| f.wave == Some(wave) && f.state != FeatureState::Archived)
            .collect();
        let over = wave == Wave::Current && members.len() as u64 > u64::from(limit);
        let col_effort: u64 = members
            .iter()
            .filter_map(|f| f.evaluation.effort.map(u64::from))
            .sum();
        let col_has_effort = members.iter().any(|f| f.evaluation.effort.is_some());

        let mut header_left = div().flex().items_center().gap_2().min_w_0().child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        );
        if wave == Wave::Current {
            let slot_cap = (limit as usize).clamp(1, 8);
            let mut meter = div().flex().items_center().gap(px(2.0));
            for slot in 0..slot_cap {
                let filled = slot < members.len();
                let color = if filled && over {
                    theme::danger()
                } else if filled {
                    theme::accent()
                } else {
                    theme::border_strong()
                };
                meter = meter.child(div().w(px(10.0)).h(px(5.0)).rounded_sm().bg(color));
            }
            header_left = header_left.child(meter);
        } else if col_has_effort {
            header_left = header_left.child(
                div()
                    .font_family(theme::FONT_MONO)
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child(format!("Est. E:{col_effort}")),
            );
        }

        let mut column = div()
            .id(SharedString::from(format!("wave-{wave:?}")))
            .debug_selector(move || format!("wave-{wave:?}"))
            .flex_1()
            .min_w(px(160.0))
            .min_h(px(180.0))
            .p_2()
            .rounded_lg()
            .bg(theme::canvas())
            .border_1()
            .border_color(if over {
                theme::warning()
            } else {
                theme::border()
            })
            .flex()
            .flex_col()
            .gap_2()
            .on_drop(cx.listener(move |m, drag: &super::FeatureDrag, _, cx| {
                m.forge_action(ForgeAction::Move(drag.id.clone(), wave), cx)
            }))
            .child(
                div()
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(header_left)
                    .child(mono_pill(
                        if wave == Wave::Current {
                            format!("{} / {limit}", members.len())
                        } else {
                            members.len().to_string()
                        },
                        if over {
                            theme::warning_soft()
                        } else {
                            theme::surface_raised()
                        },
                        if over {
                            theme::warning()
                        } else {
                            theme::text_muted()
                        },
                    )),
            );
        if members.is_empty() {
            column = column.child(
                div()
                    .px_2()
                    .py_4()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::border())
                    .text_xs()
                    .text_center()
                    .text_color(theme::text_subtle())
                    .child("Drop a feature here"),
            );
        }
        for f in members {
            let mut mover = widgets::segment_track(div()).self_start();
            for (target, name) in [
                (Wave::Current, "Now"),
                (Wave::Next, "Next"),
                (Wave::Horizon, "Later"),
            ] {
                let id = f.id.clone();
                mover = mover.child(
                    widgets::segment(
                        div()
                            .id(SharedString::from(format!("move-{}-{target:?}", f.id)))
                            .debug_selector({
                                let id = format!("move-{}-{target:?}", f.id);
                                move || id.clone()
                            }),
                        target == wave,
                    )
                    .text_xs()
                    .px(px(6.0))
                    .py(px(2.0))
                    .when(target != wave, |segment| {
                        segment.on_click(cx.listener(move |model, _, _, cx| {
                            model.forge_action(ForgeAction::Move(id.clone(), target), cx)
                        }))
                    })
                    .child(name),
                );
            }

            let stage_readout = wave_stage_label(model, f, cx);
            let (waiting_on, unblocks) = feature_wave_dependencies(model, f);
            let state_dot_color = feature_state_color(f.state);
            let metrics_line = match (f.evaluation.score(), f.evaluation.effort) {
                (Some(s), Some(e)) => format!("ICE {s:.1} · E:{e}"),
                (Some(s), None) => format!("ICE {s:.1}"),
                (None, Some(e)) => format!("E:{e}"),
                (None, None) => "Not scored".into(),
            };

            column = column.child(
                div()
                    .id(SharedString::from(format!("wave-feature-{}", f.id)))
                    .on_drag(
                        super::FeatureDrag {
                            id: f.id.clone(),
                            title: f.title.clone(),
                        },
                        |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .p_2()
                    .rounded_md()
                    .bg(theme::surface_raised())
                    .border_1()
                    .border_color(if !waiting_on.is_empty() {
                        theme::warning()
                    } else {
                        theme::border()
                    })
                    .cursor_grab()
                    .hover(|style| style.bg(theme::surface_hover()))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(widgets::status_dot(state_dot_color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(f.title.clone()),
                            )
                            .child(mono_pill(
                                layer_short(f.structural_level),
                                theme::surface(),
                                theme::text_muted(),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .child(if f.state == FeatureState::Done {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_color(theme::success())
                                    .child(widgets::icon(Icon::Check, px(12.0), theme::success()))
                                    .child(state(f.state))
                                    .into_any_element()
                            } else {
                                div()
                                    .text_color(state_dot_color)
                                    .child(state(f.state))
                                    .into_any_element()
                            })
                            .when_some(stage_readout, |row, readout| {
                                row.child(mono_pill(
                                    readout,
                                    theme::running_soft(),
                                    theme::running(),
                                ))
                            }),
                    )
                    .when(!waiting_on.is_empty(), |card_el| {
                        let first = &waiting_on[0];
                        let msg = if waiting_on.len() > 1 {
                            format!("Waiting on: {first} (+{})", waiting_on.len() - 1)
                        } else {
                            format!("Waiting on: {first}")
                        };
                        card_el.child(
                            div()
                                .px_2()
                                .py(px(3.0))
                                .rounded_sm()
                                .bg(theme::warning_soft())
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(theme::warning())
                                .min_w_0()
                                .child(widgets::icon(Icon::Flag, px(11.0), theme::warning()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(msg),
                                ),
                        )
                    })
                    .when(unblocks > 0 && f.state != FeatureState::Done, |card_el| {
                        card_el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(theme::success())
                                .child(widgets::icon(Icon::ArrowRight, px(11.0), theme::success()))
                                .child(format!("Unblocks {unblocks}")),
                        )
                    })
                    .child(
                        div()
                            .pt_1()
                            .border_t_1()
                            .border_color(theme::border())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_family(theme::FONT_MONO)
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child(metrics_line),
                            )
                            // Its own line: beside the state it overflowed narrow columns.
                            .child(mover),
                    ),
            );
        }
        columns = columns.child(column);
    }

    let ready: Vec<_> = model
        .forge
        .snapshot
        .features
        .iter()
        .filter(|f| {
            f.wave.is_none()
                && f.approved_revision == Some(f.revision)
                && f.state != FeatureState::Archived
        })
        .collect();
    let mut ready_list = div().flex().flex_col().gap_2();
    for f in &ready {
        let (waiting_on, _) = feature_wave_dependencies(model, f);
        let ice_text = f
            .evaluation
            .score()
            .map(|s| format!("ICE {s:.1}"))
            .unwrap_or_else(|| "Unscored".into());
        ready_list = ready_list.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .child(widgets::status_dot(feature_state_color(f.state)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(120.0))
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_ellipsis()
                        .child(f.title.clone()),
                )
                .child(mono_pill(
                    layer_short(f.structural_level),
                    theme::surface_raised(),
                    theme::text_muted(),
                ))
                .child(mono_pill(
                    ice_text,
                    theme::surface_raised(),
                    theme::text_muted(),
                ))
                .when(!waiting_on.is_empty(), |row| {
                    row.child(pill(
                        format!("Blocked ({})", waiting_on.len()),
                        theme::warning_soft(),
                        theme::warning(),
                    ))
                })
                .child(button_variant(
                    format!("plan-current-{}", f.id),
                    if f.brief.foundation {
                        "Plan as foundation"
                    } else {
                        "Plan now"
                    },
                    ButtonVariant::Tinted,
                    ForgeAction::Move(f.id.clone(), Wave::Current),
                    None,
                    cx,
                ))
                .child(button_variant(
                    format!("plan-next-{}", f.id),
                    "Plan next",
                    ButtonVariant::Secondary,
                    ForgeAction::Move(f.id.clone(), Wave::Next),
                    None,
                    cx,
                )),
        );
    }

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            card()
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
                                .gap_3()
                                .child(heading_info(
                                    "Wave planning",
                                    "wave-planning",
                                    "Drag a feature between waves or use its Now / Next / Later switch. Current-wave capacity limits how much is committed at once.",
                                ))
                                .child(pill_dot(wip_dot, wip_label, wip_bg, wip_fg)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child(format!("{} planned", planned_features.len()))
                                        .when(has_any_effort, |meta| {
                                            meta.child(
                                                div()
                                                    .font_family(theme::FONT_MONO)
                                                    .child(format!(
                                                        "· Est. effort: {total_planned_effort} pts"
                                                    )),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(label("Current-wave capacity"))
                                        .child(button_variant(
                                            "wave-limit-dec",
                                            "-",
                                            ButtonVariant::Secondary,
                                            ForgeAction::AdjustWaveLimit(-1),
                                            None,
                                            cx,
                                        ))
                                        .child(sized_input(
                                            model.forge.wave_limit.clone(),
                                            px(56.0),
                                        ))
                                        .child(button_variant(
                                            "wave-limit-inc",
                                            "+",
                                            ButtonVariant::Secondary,
                                            ForgeAction::AdjustWaveLimit(1),
                                            None,
                                            cx,
                                        ))
                                        .child(button(
                                            "set-wave-limit",
                                            "Update",
                                            ForgeAction::SetLimit,
                                            cx,
                                        )),
                                ),
                        ),
                )
                .child(columns),
        )
        .when(!ready.is_empty(), |body| {
            body.child(
                card()
                    .border_1()
                    .border_color(theme::border())
                    .child(heading_info(
                        format!("Ready to plan · {}", ready.len()),
                        "ready-to-plan",
                        "Approved features that are not in a wave yet.",
                    ))
                    .child(ready_list),
            )
        })
        .into_any_element()
}
