use crate::app_model::AppModel;
use crate::theme;
use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
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
            })
        }
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
