use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::pipeline_status::{PipelineStatus, step_label};
use crate::pulsations::{PIPELINE_STAGES, stage_label};
use crate::sizing::StyledExt;
use crate::theme;
use gpui::{AnyElement, Context, IntoElement, Pixels, SharedString, div, prelude::*, px};
use std::sync::Arc;

// `btn`/`btn_confirm`/`inline_banner`/`loading_pill` are adopted progressively as each view is
// touched during the UI flow overhaul (see the navigation/flow plan); until a view is converted,
// these stay unused in a plain build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Danger,
    Ghost,
    Selected,
}

#[allow(dead_code)]
pub fn btn(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let label = label.into();
    let handler = Arc::new(handler);
    let key_handler = Arc::clone(&handler);

    let (bg, fg) = match variant {
        ButtonVariant::Primary => (theme::accent(), theme::canvas()),
        ButtonVariant::Secondary => (theme::surface_raised(), theme::text()),
        ButtonVariant::Danger => (theme::danger_soft(), theme::danger()),
        ButtonVariant::Ghost => (theme::surface(), theme::text_muted()),
        ButtonVariant::Selected => (theme::accent_soft(), theme::accent()),
    };

    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .self_start()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .rounded_md()
        .text_sm()
        .bg(bg)
        .text_color(fg)
        .opacity(if enabled { 1.0 } else { 0.45 })
        .tab_index(0)
        .when(enabled, |b| {
            b.cursor_pointer()
                .hover(|s| s.opacity(theme::HOVER_OPACITY))
                .on_key_down(
                    cx.listener(move |model, event: &gpui::KeyDownEvent, _window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            key_handler(model, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .on_click(cx.listener(move |model, _, _, cx| {
                    handler(model, cx);
                }))
        })
        .child(label)
        .into_any_element()
}

// Keep the existing view call sites explicit about both confirmation callbacks.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub fn btn_confirm(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    confirming: bool,
    enabled: bool,
    variant: ButtonVariant,
    cx: &mut Context<AppModel>,
    on_first_click: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
    on_confirm: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let current_label: SharedString = if confirming {
        confirm_label.into()
    } else {
        label.into()
    };
    let effective_variant = if confirming {
        ButtonVariant::Danger
    } else {
        variant
    };

    btn(
        id,
        current_label,
        effective_variant,
        enabled,
        cx,
        move |model, cx| {
            if confirming {
                on_confirm(model, cx);
            } else {
                on_first_click(model, cx);
            }
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BannerKind {
    Info,
    Warning,
    Error,
}

#[allow(dead_code)]
pub fn inline_banner(message: impl Into<SharedString>, kind: BannerKind) -> AnyElement {
    let message = message.into();
    let debug_key = message.clone();
    let (bg, fg) = match kind {
        BannerKind::Info => (theme::accent_soft(), theme::accent()),
        BannerKind::Warning => (theme::warning_soft(), theme::warning()),
        BannerKind::Error => (theme::danger_soft(), theme::danger()),
    };
    div()
        .debug_selector(move || debug_key.to_string())
        .p_3()
        .rounded_md()
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .child(message)
        .into_any_element()
}

#[allow(dead_code)]
pub fn loading_pill(label: impl Into<SharedString>) -> AnyElement {
    let label = label.into();
    let debug_key = label.clone();
    div()
        .debug_selector(move || debug_key.to_string())
        .px_3()
        .py_1()
        .rounded_full()
        .bg(theme::accent_soft())
        .text_color(theme::accent())
        .text_xs()
        .child(label)
        .into_any_element()
}

/// Shared `(eyebrow, title, count)` section heading used by detail panels. Adopted opportunistically
/// as each view is touched; existing per-view copies (Pulsations, Vault, Settings) are folded into
/// this one during the same pass that converts their panel.
#[allow(dead_code)]
pub fn section_header(
    eyebrow: impl Into<SharedString>,
    title: impl Into<SharedString>,
    count: usize,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(eyebrow.into()),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title.into()),
                ),
        )
        .child(
            div()
                .min_w(px(24.0))
                .h(px(24.0))
                .px_2()
                .rounded_full()
                .bg(theme::surface_raised())
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .child(count.to_string()),
        )
        .into_any_element()
}

/// Shared centered empty-state message, folded in from the near-identical copies in
/// Chat/Settings/Agents/Pulsations/Vault as each view is converted.
#[allow(dead_code)]
pub fn empty_state(message: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::text_muted())
        .child(message.into())
        .into_any_element()
}

/// Shared key/value fact box (STATUS, TOKENS, ...) used by detail panels.
#[allow(dead_code)]
pub fn fact(label: impl Into<SharedString>, value: impl Into<SharedString>) -> AnyElement {
    div()
        .p_3()
        .rounded_md()
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(label.into()),
        )
        .child(div().text_sm().child(value.into()))
        .into_any_element()
}

/// A tinted Lucide glyph. Always pass the colour explicitly: `Svg` paints with its own
/// `text_color`, not the inherited one.
pub(crate) fn icon(icon: Icon, size: Pixels, color: gpui::Rgba) -> gpui::Svg {
    gpui::svg()
        .path(icon.path())
        .size(size)
        .flex_shrink_0()
        .text_color(color)
}

/// Default glyph size inside a 36px `icon_button`.
pub(crate) const ICON_SIZE: Pixels = px(18.0);
/// Hit area of an icon-only control; matches the nav rail item size.
pub(crate) const ICON_BUTTON_SIZE: Pixels = px(36.0);

/// Hover tooltip for icon-only controls: a label and, optionally, the keyboard shortcut that
/// does the same thing. The value is the spec; GPUI builds the view lazily when the pointer
/// rests on the control, so it costs nothing until then.
#[derive(Clone)]
pub(crate) struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
}

impl Tooltip {
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            shortcut: None,
        }
    }

    pub(crate) fn with_shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }
}

impl From<&'static str> for Tooltip {
    fn from(text: &'static str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Tooltip {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<SharedString> for Tooltip {
    fn from(text: SharedString) -> Self {
        Self::new(text)
    }
}

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "tooltip".into())
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_raised())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::FONT_UI)
            .child(self.text.clone())
            .when_some(self.shortcut.clone(), |tip, shortcut| {
                tip.child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text_muted())
                        .child(shortcut),
                )
            })
    }
}

/// Icon-only control with the same focus/keyboard/disabled discipline as `btn`. `selected`
/// renders the accent-soft "current" state used by the nav rail; `badge_count` overlays a count
/// at the top-right corner. The handler receives the `Window` because flow steps need it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn icon_button(
    id: impl Into<SharedString>,
    glyph: Icon,
    tooltip: impl Into<Tooltip>,
    selected: bool,
    badge_count: Option<usize>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut gpui::Window, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let tooltip = tooltip.into();
    let handler = Arc::new(handler);
    let key_handler = Arc::clone(&handler);
    let ink = if selected {
        theme::accent()
    } else {
        theme::text_muted()
    };

    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .relative()
        .flex_shrink_0()
        .size(ICON_BUTTON_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .when(selected, |b| b.bg(theme::accent_soft()))
        .text_color(ink)
        .opacity(if enabled { 1.0 } else { 0.45 })
        .tab_index(0)
        .tooltip(move |_, cx| cx.new(|_| tooltip.clone()).into())
        .when(enabled, |b| {
            b.cursor_pointer()
                .hover(|s| s.bg(theme::surface_raised()))
                .on_key_down(
                    cx.listener(move |model, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            key_handler(model, window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .on_click(cx.listener(move |model, _, window, cx| {
                    handler(model, window, cx);
                }))
        })
        .child(icon(glyph, ICON_SIZE, ink))
        .when_some(badge_count.filter(|count| *count > 0), |button, count| {
            button.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .child(badge(count, theme::accent(), theme::canvas())),
            )
        })
        .into_any_element()
}

/// Small numeric count pill (bell, rail items, section counters).
pub(crate) fn badge(count: usize, bg: gpui::Rgba, fg: gpui::Rgba) -> AnyElement {
    div()
        .min_w(px(16.0))
        .h(px(16.0))
        .px_1()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .bg(bg)
        .text_color(fg)
        .child(count.to_string())
        .into_any_element()
}

/// 8px status dot; the shell's cheapest live indicator.
pub(crate) fn status_dot(color: gpui::Rgba) -> AnyElement {
    div()
        .flex_shrink_0()
        .size_2()
        .rounded_full()
        .bg(color)
        .into_any_element()
}

/// Outlined text chip in a semantic colour (agent role, run state).
#[allow(dead_code)]
pub(crate) fn chip(label: impl Into<SharedString>, color: gpui::Rgba) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(color)
        .text_xs()
        .text_color(color)
        .child(label.into())
        .into_any_element()
}

/// Horizontal hairline separating groups in a vertical rail.
#[allow(dead_code)]
pub(crate) fn rail_divider() -> AnyElement {
    div()
        .w(px(24.0))
        .h_px()
        .my_1()
        .bg(theme::border())
        .into_any_element()
}

/// The six-stage stepper for a pulsation. `id_prefix` scopes the debug selectors
/// (`{prefix}` and `{prefix}-stage-{stage}`); `vertical` stacks the stages for the context rail.
pub(crate) fn pipeline_stepper(
    status: &PipelineStatus,
    id_prefix: &'static str,
    vertical: bool,
) -> AnyElement {
    let stage_index = status.stage_index;
    let current_fill = status.current_fill();
    let completed: Vec<bool> = (0..PIPELINE_STAGES.len())
        .map(|index| status.stage_complete(index))
        .collect();
    div()
        .id(id_prefix)
        .debug_selector(move || id_prefix.to_string())
        .flex_shrink_0()
        .flex()
        .when(vertical, |stepper| stepper.flex_col().items_start())
        .when(!vertical, |stepper| stepper.items_center())
        .gap_1()
        .text_xs()
        .children(
            PIPELINE_STAGES
                .into_iter()
                .enumerate()
                .flat_map(move |(index, stage)| {
                    let current = index == stage_index;
                    let complete = completed[index];
                    let stage_id = SharedString::from(format!(
                        "{id_prefix}-stage-{}",
                        stage_label(stage).to_lowercase()
                    ));
                    let stage_selector = stage_id.clone();
                    let (fill, ink) = if current {
                        (current_fill, theme::canvas())
                    } else if complete {
                        (theme::accent_soft(), theme::accent())
                    } else {
                        (theme::surface_raised(), theme::text_muted())
                    };
                    let step = div()
                        .id(stage_id)
                        .debug_selector(move || stage_selector.to_string())
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size_5()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(fill)
                                .text_color(ink)
                                .when(current, |circle| {
                                    circle.font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .when(complete, |circle| {
                                    circle.child(icon(Icon::Check, px(12.0), ink))
                                })
                                .when(!complete, |circle| circle.child((index + 1).to_string())),
                        )
                        .child(
                            div()
                                .text_color(if current {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .when(current, |label| {
                                    label.font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .child(step_label(stage)),
                        );
                    let connector = (index + 1 < PIPELINE_STAGES.len()).then(|| {
                        let line = div().flex_shrink_0().bg(if complete {
                            theme::accent()
                        } else {
                            theme::border()
                        });
                        if vertical {
                            line.ml(px(9.5)).w_px().h_2()
                        } else {
                            line.w_4().h_px()
                        }
                    });
                    std::iter::once(step.into_any_element())
                        .chain(connector.map(IntoElement::into_any_element))
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Render, TestAppContext, Window};

    struct WidgetsHarness(Entity<AppModel>);

    impl Render for WidgetsHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |_, cx| {
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(btn(
                        "widgets-test-enabled",
                        "Clique",
                        ButtonVariant::Primary,
                        true,
                        cx,
                        |_, _| {},
                    ))
                    .child(btn(
                        "widgets-test-disabled",
                        "Clique",
                        ButtonVariant::Primary,
                        false,
                        cx,
                        |_, _| {},
                    ))
                    .child(btn_confirm(
                        "widgets-test-confirm-idle",
                        "Delete",
                        "Confirm deletion",
                        false,
                        true,
                        ButtonVariant::Danger,
                        cx,
                        |_, _| {},
                        |_, _| {},
                    ))
                    .child(btn_confirm(
                        "widgets-test-confirm-active",
                        "Delete",
                        "Confirm deletion",
                        true,
                        true,
                        ButtonVariant::Danger,
                        cx,
                        |_, _| {},
                        |_, _| {},
                    ))
                    .child(inline_banner("Informational message", BannerKind::Info))
                    .child(loading_pill("Loading…"))
                    .child(icon_button(
                        "widgets-test-icon",
                        Icon::House,
                        "Home",
                        false,
                        None,
                        true,
                        cx,
                        |model, _, cx| model.select_view(crate::app_model::AppView::Logs, cx),
                    ))
                    .child(icon_button(
                        "widgets-test-icon-selected",
                        Icon::Settings,
                        "Settings",
                        true,
                        Some(3),
                        true,
                        cx,
                        |_, _, _| {},
                    ))
                    .child(icon_button(
                        "widgets-test-icon-disabled",
                        Icon::Play,
                        "Run",
                        false,
                        None,
                        false,
                        cx,
                        |model, _, cx| model.select_view(crate::app_model::AppView::Vault, cx),
                    ))
                    .child(badge(12, theme::accent(), theme::canvas()))
                    .child(status_dot(theme::success()))
                    .child(chip("BackendAgent", theme::accent()))
                    .child(rail_divider())
            })
        }
    }

    struct TooltipHarness(Entity<Tooltip>);

    impl Render for TooltipHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.0.clone())
        }
    }

    struct StepperHarness;

    impl Render for StepperHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let status = PipelineStatus {
                stage: hvc_core::persistence_service::PulsationStage::Code,
                stage_index: 3,
                pulsation_status: hvc_core::persistence_service::PulsationStatus::Running,
                agent_name: "BackendAgent",
                state: "Running",
                tone: crate::pipeline_status::StatusTone::Active,
            };
            div()
                .flex()
                .flex_col()
                .items_start()
                .child(pipeline_stepper(&status, "stepper-h", false))
                .child(pipeline_stepper(&status, "stepper-v", true))
        }
    }

    #[gpui::test]
    fn icon_buttons_have_a_fixed_hit_area_and_only_enabled_ones_react(cx: &mut TestAppContext) {
        let (harness, visual) =
            cx.add_window_view(|_, cx| WidgetsHarness(cx.new(AppModel::new_for_test)));

        for id in [
            "widgets-test-icon",
            "widgets-test-icon-selected",
            "widgets-test-icon-disabled",
        ] {
            let bounds = visual.debug_bounds(id).unwrap();
            assert_eq!(bounds.size.width, ICON_BUTTON_SIZE, "{id}");
            assert_eq!(bounds.size.height, ICON_BUTTON_SIZE, "{id}");
        }

        let enabled = visual.debug_bounds("widgets-test-icon").unwrap();
        visual.simulate_click(enabled.center(), gpui::Modifiers::default());
        visual.run_until_parked();
        harness.read_with(visual, |harness, cx| {
            assert_eq!(
                harness.0.read(cx).active_view(),
                crate::app_model::AppView::Logs
            );
        });

        let disabled = visual.debug_bounds("widgets-test-icon-disabled").unwrap();
        visual.simulate_click(disabled.center(), gpui::Modifiers::default());
        visual.run_until_parked();
        harness.read_with(visual, |harness, cx| {
            assert_eq!(
                harness.0.read(cx).active_view(),
                crate::app_model::AppView::Logs
            );
        });
    }

    #[gpui::test]
    fn tooltip_and_stepper_render_with_real_geometry(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, cx| {
            TooltipHarness(cx.new(|_| Tooltip::new("Home").with_shortcut("ctrl-1")))
        });
        let tooltip = visual.debug_bounds("tooltip").unwrap();
        assert!(tooltip.size.width > px(0.0) && tooltip.size.height > px(0.0));

        let (_, visual) = cx.add_window_view(|_, _| StepperHarness);
        let horizontal = visual.debug_bounds("stepper-h").unwrap();
        let vertical = visual.debug_bounds("stepper-v").unwrap();
        assert!(horizontal.size.width > horizontal.size.height);
        assert!(vertical.size.height > vertical.size.width);
        for id in [
            "stepper-h-stage-intake",
            "stepper-h-stage-specification",
            "stepper-h-stage-contract",
            "stepper-h-stage-code",
            "stepper-h-stage-test",
            "stepper-h-stage-integrator",
            "stepper-v-stage-intake",
            "stepper-v-stage-integrator",
        ] {
            assert!(visual.debug_bounds(id).is_some(), "{id}");
        }
        let code_h = visual.debug_bounds("stepper-h-stage-code").unwrap();
        let test_h = visual.debug_bounds("stepper-h-stage-test").unwrap();
        assert!(test_h.left() > code_h.right());
        let code_v = visual.debug_bounds("stepper-v-stage-code").unwrap();
        let test_v = visual.debug_bounds("stepper-v-stage-test").unwrap();
        assert!(test_v.top() > code_v.bottom());
    }

    #[gpui::test]
    fn widgets_render_with_real_geometry_across_enabled_and_confirming_states(
        cx: &mut TestAppContext,
    ) {
        let (_, visual) =
            cx.add_window_view(|_, cx| WidgetsHarness(cx.new(AppModel::new_for_test)));

        let enabled = visual.debug_bounds("widgets-test-enabled").unwrap();
        let disabled = visual.debug_bounds("widgets-test-disabled").unwrap();
        let confirm_idle = visual.debug_bounds("widgets-test-confirm-idle").unwrap();
        let confirm_active = visual.debug_bounds("widgets-test-confirm-active").unwrap();
        let banner = visual.debug_bounds("Informational message").unwrap();
        let pill = visual.debug_bounds("Loading…").unwrap();

        // Every widget must actually mount with real layout, not an empty/zero-size element.
        for bounds in [
            enabled,
            disabled,
            confirm_idle,
            confirm_active,
            banner,
            pill,
        ] {
            assert!(bounds.size.width > px(0.0));
            assert!(bounds.size.height > px(0.0));
        }

        // "Confirm deletion" is longer than "Delete": the confirming state must actually
        // swap the rendered label, not silently keep showing the idle one.
        assert!(confirm_active.size.width > confirm_idle.size.width);
    }
}
