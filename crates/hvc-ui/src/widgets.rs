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
    let (bg, fg) = match kind {
        BannerKind::Info => (theme::accent_soft(), theme::accent()),
        BannerKind::Warning => (theme::warning_soft(), theme::warning()),
        BannerKind::Error => (theme::danger_soft(), theme::danger()),
    };
    div()
        .p_3()
        .rounded_md()
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .child(message.into())
        .into_any_element()
}

#[allow(dead_code)]
pub fn loading_pill(label: impl Into<SharedString>) -> AnyElement {
    div()
        .px_3()
        .py_1()
        .rounded_full()
        .bg(theme::accent_soft())
        .text_color(theme::accent())
        .text_xs()
        .child(label.into())
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

// Labels, selection state and interaction are supplied independently by native views.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_select(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    current_value: impl Into<SharedString>,
    is_open: bool,
    enabled: bool,
    options_count: usize,
    cx: &mut Context<AppModel>,
    on_toggle: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let label = label.into();
    let current_value = current_value.into();

    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(if is_open {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(theme::surface_raised())
        .opacity(if enabled { 1.0 } else { 0.45 })
        .tab_index(0)
        .when(enabled, |item| {
            item.cursor_pointer()
                .hover(|s| s.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |model, _, _, cx| on_toggle(model, cx)))
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_xs().text_color(theme::text_muted()).child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("{options_count} opções")),
                ),
        )
        .child(
            div()
                .mt_1()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(current_value),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(if is_open {
                            theme::accent()
                        } else {
                            theme::text_muted()
                        })
                        .child(if is_open { "▴" } else { "▾" }),
                ),
        )
        .into_any_element()
}

pub fn dropdown_menu(
    id: impl Into<SharedString>,
    children: impl IntoIterator<Item = AnyElement>,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .mt_1()
        .p_1()
        .rounded_md()
        .border_1()
        .border_color(theme::accent())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_1()
        .children(children)
        .into_any_element()
}

pub fn dropdown_item(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    sublabel: Option<impl Into<SharedString>>,
    selected: bool,
    cx: &mut Context<AppModel>,
    on_select: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let label = label.into();
    let sublabel = sublabel.map(|s| s.into());
    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .px_2()
        .py_1()
        .rounded_sm()
        .tab_index(0)
        .cursor_pointer()
        .bg(if selected {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .text_color(if selected {
            theme::accent()
        } else {
            theme::text()
        })
        .hover(|s| s.bg(theme::hover_bg()))
        .flex()
        .items_center()
        .justify_between()
        .on_click(cx.listener(move |m, _, _, cx| {
            on_select(m, cx);
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_sm()
                        .font_weight(if selected {
                            gpui::FontWeight::SEMIBOLD
                        } else {
                            gpui::FontWeight::NORMAL
                        })
                        .child(label),
                )
                .when_some(sublabel, |item, sub| {
                    item.child(div().text_xs().text_color(theme::text_muted()).child(sub))
                }),
        )
        .when(selected, |item| {
            item.child(div().text_sm().text_color(theme::accent()).child("✓"))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn widgets_render_buttons_and_confirmations(cx: &mut TestAppContext) {
        let app = cx.new(AppModel::new_for_test);
        app.update(cx, |_model, cx| {
            let button_element = btn(
                "test-btn",
                "Clique",
                ButtonVariant::Primary,
                true,
                cx,
                |_, _| {},
            );
            let _ = button_element;

            let confirm_element = btn_confirm(
                "test-confirm",
                "Deletar",
                "Confirmar deleção",
                false,
                true,
                ButtonVariant::Danger,
                cx,
                |_, _| {},
                |_, _| {},
            );
            let _ = confirm_element;

            let banner = inline_banner("Mensagem informativa", BannerKind::Info);
            let _ = banner;

            let pill = loading_pill("Carregando…");
            let _ = pill;
        });
    }
}
