use crate::{AppModel, text_input::NativeTextInput, theme};
use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement, Pixels, Render,
    SharedString, Window, canvas, div, point, prelude::*, px, size,
};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Debug)]
pub(crate) struct Choice {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) selected: bool,
    pub(crate) reason: Option<String>,
}
impl Choice {
    pub(crate) fn new(value: impl ToString, label: impl ToString, selected: bool) -> Self {
        Self {
            value: value.to_string(),
            label: label.to_string(),
            selected,
            reason: None,
        }
    }
}

pub(crate) struct SelectMenu {
    owner: gpui::WeakEntity<AppModel>,
    target: String,
    project: Option<String>,
    agent: Option<crate::agents::AgentIdentifier>,
    choices: Vec<Choice>,
    anchor: Bounds<Pixels>,
    focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    search: Entity<NativeTextInput>,
    cursor: usize,
    scroll: gpui::ScrollHandle,
    _subscription: gpui::Subscription,
}

impl SelectMenu {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        owner: gpui::WeakEntity<AppModel>,
        target: String,
        project: Option<String>,
        agent: Option<crate::agents::AgentIdentifier>,
        choices: Vec<Choice>,
        anchor: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let return_focus = window.focused(cx);
        let search = cx.new(|cx| NativeTextInput::new("Search options", cx));
        let subscription = cx.observe(&search, |m, _, cx| {
            m.cursor = 0;
            m.scroll.scroll_to_item(0);
            cx.notify();
        });
        let focus = cx.focus_handle();
        focus.focus(window);
        if choices.len() > 8 {
            search.read(cx).focus_handle(cx).focus(window);
        }
        let cursor = choices.iter().position(|c| c.selected).unwrap_or(0);
        Self {
            owner,
            target,
            project,
            agent,
            choices,
            anchor,
            focus,
            return_focus,
            search,
            cursor,
            scroll: gpui::ScrollHandle::new(),
            _subscription: subscription,
        }
    }
    fn filtered(&self, cx: &gpui::App) -> Vec<Choice> {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.choices
            .iter()
            .filter(|c| c.label.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }
    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focus) = &self.return_focus {
            focus.focus(window);
        }
        let _ = self.owner.update(cx, |m, cx| m.close_dropdown(cx));
    }
    fn choose(&self, choice: &Choice, window: &mut Window, cx: &mut Context<Self>) {
        if choice.reason.is_some() {
            return;
        }
        if let Some(focus) = &self.return_focus {
            focus.focus(window);
        }
        let _ = self.owner.update(cx, |m, cx| {
            let project = m
                .workspace
                .read(cx)
                .project_id
                .as_ref()
                .map(ToString::to_string);
            if project == self.project
                && (!self.target.starts_with("agent") && !self.target.starts_with("free-form")
                    || m.agents.read(cx).selected_agent == self.agent)
            {
                m.apply_choice(&self.target, &choice.value, cx);
            }
            m.close_dropdown(cx);
        });
    }
}

pub(crate) fn menu_bounds(
    anchor: Bounds<Pixels>,
    viewport: gpui::Size<Pixels>,
    count: usize,
    search: bool,
) -> Bounds<Pixels> {
    let width = anchor
        .size
        .width
        .max(px(240.0))
        .min(px(360.0))
        .min(viewport.width - px(16.0));
    let desired = px(count.min(8) as f32 * 46.0 + if search { 66.0 } else { 16.0 });
    let below = viewport.height - anchor.bottom() - px(12.0);
    let above = anchor.top() - px(12.0);
    let upwards = below < desired && above > below;
    let height = desired
        .min(if upwards { above } else { below })
        .max(px(40.0));
    let top = if upwards {
        anchor.top() - height - px(4.0)
    } else {
        anchor.bottom() + px(4.0)
    };
    Bounds::new(
        point(
            anchor
                .left()
                .min(viewport.width - width - px(8.0))
                .max(px(8.0)),
            top.max(px(8.0)),
        ),
        size(width, height),
    )
}

impl Render for SelectMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let choices = self.filtered(cx);
        self.cursor = self.cursor.min(choices.len().saturating_sub(1));
        let bounds = menu_bounds(
            self.anchor,
            window.viewport_size(),
            choices.len(),
            self.choices.len() > 8,
        );
        div()
            .id("selection-menu")
            .debug_selector(|| "selection-menu".into())
            .absolute()
            .left(bounds.left())
            .top(bounds.top())
            .w(bounds.size.width)
            .h(bounds.size.height)
            .track_focus(&self.focus)
            .occlude()
            .p_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::surface_raised())
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down_out(cx.listener(|m, _, window, cx| {
                m.close(window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|m, event: &gpui::KeyDownEvent, window, cx| {
                let choices = m.filtered(cx);
                match event.keystroke.key.as_str() {
                    "escape" | "tab" => m.close(window, cx),
                    "down" => {
                        m.cursor = (m.cursor + 1).min(choices.len().saturating_sub(1));
                        m.scroll.scroll_to_item(m.cursor);
                        cx.notify();
                    }
                    "up" => {
                        m.cursor = m.cursor.saturating_sub(1);
                        m.scroll.scroll_to_item(m.cursor);
                        cx.notify();
                    }
                    "enter" => {
                        if let Some(choice) = choices.get(m.cursor) {
                            m.choose(choice, window, cx);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .when(self.choices.len() > 8, |d| d.child(self.search.clone()))
            .child(
                div()
                    .id("selection-options")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(choices.is_empty(), |d| {
                        d.child(div().p_2().child("No matching options"))
                    })
                    .children(choices.into_iter().enumerate().map(|(i, c)| {
                        let selected = c.selected;
                        div()
                            .id(SharedString::from(format!("choice:{}", c.value)))
                            .debug_selector({
                                let value = c.value.clone();
                                move || format!("choice:{value}")
                            })
                            .px_2()
                            .py_2()
                            .rounded_sm()
                            .cursor_pointer()
                            .text_sm()
                            .bg(if self.cursor == i {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            })
                            .hover(|s| s.bg(theme::hover_bg()))
                            .opacity(if c.reason.is_some() { 0.55 } else { 1.0 })
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(c.label.clone())
                                    .child(if selected { "✓" } else { "" }),
                            )
                            .when_some(c.reason.clone(), |d, r| d.child(div().text_xs().child(r)))
                            .on_click(cx.listener(move |m, _, window, cx| m.choose(&c, window, cx)))
                    })),
            )
    }
}

pub(crate) fn mode_control(
    id: String,
    value: &str,
    enabled: bool,
    cli_reason: Option<&'static str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let api_selected = value.eq_ignore_ascii_case("api")
        || value.eq_ignore_ascii_case("api key mode");
    let cli_selected = value.eq_ignore_ascii_case("cli")
        || value.eq_ignore_ascii_case("cli mode");
    let options = [("api", "API Key Mode", api_selected), ("cli", "CLI Mode", cli_selected)]
        .into_iter()
        .map(|(value, label, selected)| {
            let mut choice = Choice::new(value, label, selected);
            if value == "cli" {
                choice.reason = cli_reason.map(str::to_string);
            }
            choice
        })
        .collect();
    let display_value = if api_selected {
        "API Key Mode"
    } else {
        "CLI Mode"
    };
    control_options(
        id,
        "MODE".into(),
        display_value.into(),
        enabled,
        Some(options),
        cx,
    )
}

pub(crate) fn control(
    id: impl Into<String>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let label = label.into();
    let value = value.into();
    control_options(id, label, value, enabled, None, cx)
}

fn control_options(
    id: String,
    label: SharedString,
    value: SharedString,
    enabled: bool,
    options: Option<Vec<Choice>>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let bounds = Rc::new(Cell::new(Bounds::default()));
    let measured = bounds.clone();
    let key_bounds = bounds.clone();
    let key_id = id.clone();
    let debug = id.clone();
    let segmented = options.or_else(|| {
        crate::choices::segments(&id).map(|options| {
            options
                .into_iter()
                .map(|(v, l)| {
                    Choice::new(
                        v,
                        l,
                        value.as_ref().eq_ignore_ascii_case(v)
                            || value.as_ref().eq_ignore_ascii_case(l),
                    )
                })
                .collect()
        })
    });
    if let Some(segments) = segmented {
        return segmented_control(id, label, segments, enabled, cx);
    }
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || debug.clone())
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme::accent()))
        .relative()
        // A picker is as wide as its label and value: it neither fills a row
        // (`flex_1`) nor stretches across a column. Grids still size it to the cell.
        .flex_shrink_0()
        .min_w(px(160.0))
        .max_w(px(360.0))
        .p_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::surface_raised())
        .opacity(if enabled { 1.0 } else { 0.55 })
        .child(
            canvas(move |b, _, _| measured.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(if label.as_ref() == "MODO" {
                    SharedString::from("MODE")
                } else {
                    label
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_sm()
                .child(div().min_w_0().text_ellipsis().child(value))
                .child(crate::widgets::icon(
                    crate::icons::Icon::ChevronDown,
                    px(14.0),
                    theme::text_muted(),
                )),
        )
        .when(enabled, |d| {
            d.cursor_pointer()
                .on_click(cx.listener(move |m, _, window, cx| {
                    m.open_select(&id, bounds.get(), window, cx);
                }))
                .on_key_down(cx.listener(move |m, e: &gpui::KeyDownEvent, window, cx| {
                    if matches!(e.keystroke.key.as_str(), "enter" | "space" | "down") {
                        m.open_select(&key_id, key_bounds.get(), window, cx);
                        cx.stop_propagation();
                    }
                }))
        })
        .into_any_element()
}

/// Two to four exclusive choices as the shared segmented control, its label a quiet eyebrow above
/// the track instead of a box around buttons. Segment ids stay `{id}:{value}`.
fn segmented_control(
    id: String,
    label: SharedString,
    segments: Vec<Choice>,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let debug = id.clone();
    let reasons: Vec<String> = segments
        .iter()
        .filter_map(|choice| {
            choice
                .reason
                .as_ref()
                .map(|reason| format!("{}: {reason}", choice.label))
        })
        .collect();
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || debug.clone())
        .flex_shrink_0()
        .max_w(px(360.0))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_subtle())
                .child(if label.as_ref() == "MODO" {
                    SharedString::from("MODE")
                } else {
                    label
                }),
        )
        .child(
            crate::widgets::segment_track(div()).children(segments.into_iter().map(|choice| {
                let target = id.clone();
                let chosen = choice.value.clone();
                let segment_id = SharedString::from(format!("{id}:{}", choice.value));
                let selector = segment_id.clone();
                let available = enabled && choice.reason.is_none();
                let reason = choice.reason.clone();
                crate::widgets::segment(
                    div()
                        .id(segment_id)
                        .debug_selector(move || selector.to_string()),
                    choice.selected,
                )
                .when(!available, |segment| {
                    segment.cursor_default().text_color(theme::text_subtle())
                })
                .when_some(reason, |segment, reason| {
                    let tip = SharedString::from(reason);
                    segment.tooltip(move |_, cx| {
                        cx.new(|_| crate::widgets::Tooltip::new(tip.clone())).into()
                    })
                })
                .when(available && !choice.selected, |segment| {
                    segment.on_click(
                        cx.listener(move |m, _, _, cx| m.apply_choice(&target, &chosen, cx)),
                    )
                })
                .child(choice.label)
            })),
        )
        .children(reasons.into_iter().map(|reason| {
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(reason)
        }))
        .into_any_element()
}

/// A one-line picker the height of a button ("Filter: All ⌄") for toolbars, where the two-line
/// labelled `control` towered over the buttons beside it. Opens the same menu.
/// A picker that sits in a form grid next to text inputs: the same eyebrow label above and a
/// 40 px box drawn like `NativeTextInput`, filling its cell, so a row of fields reads as one form.
pub(crate) fn field_control(
    id: impl Into<String>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let debug = id.clone();
    let key_id = id.clone();
    let bounds = Rc::new(Cell::new(Bounds::default()));
    let measured = bounds.clone();
    let key_bounds = bounds.clone();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w_0()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(label.into()),
        )
        .child(
            div()
                .id(SharedString::from(id.clone()))
                .debug_selector(move || debug.clone())
                .tab_index(0)
                .relative()
                .w_full()
                .h(px(40.0))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::canvas())
                .opacity(if enabled { 1.0 } else { 0.55 })
                .child(
                    canvas(move |b, _, _| measured.set(b), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(value.into()),
                )
                .child(crate::widgets::icon(
                    crate::icons::Icon::ChevronDown,
                    px(14.0),
                    theme::text_muted(),
                ))
                .when(enabled, |control| {
                    control
                        .cursor_pointer()
                        .hover(|style| style.border_color(theme::accent()))
                        .on_click(cx.listener(move |m, _, window, cx| {
                            m.open_select(&id, bounds.get(), window, cx);
                        }))
                        .on_key_down(cx.listener(move |m, e: &gpui::KeyDownEvent, window, cx| {
                            if matches!(e.keystroke.key.as_str(), "enter" | "space" | "down") {
                                m.open_select(&key_id, key_bounds.get(), window, cx);
                                cx.stop_propagation();
                            }
                        }))
                }),
        )
        .into_any_element()
}

pub(crate) fn compact_control(
    id: impl Into<String>,
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let debug = id.clone();
    let key_id = id.clone();
    let bounds = Rc::new(Cell::new(Bounds::default()));
    let measured = bounds.clone();
    let key_bounds = bounds.clone();
    let label = label.into();
    let value = value.into();
    crate::widgets::button_frame(
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || debug.clone())
            .relative()
            .tab_index(0),
        crate::widgets::ButtonVariant::Secondary,
        enabled,
    )
    .gap_2()
    .child(
        canvas(move |b, _, _| measured.set(b), |_, _, _, _| {})
            .absolute()
            .size_full(),
    )
    .child(
        div()
            .text_color(theme::text_muted())
            .child(format!("{label}:")),
    )
    .child(value)
    .child(crate::widgets::icon(
        crate::icons::Icon::ChevronDown,
        px(12.0),
        theme::text_muted(),
    ))
    .when(enabled, |control| {
        control
            .on_click(cx.listener(move |m, _, window, cx| {
                m.open_select(&id, bounds.get(), window, cx);
            }))
            .on_key_down(cx.listener(move |m, e: &gpui::KeyDownEvent, window, cx| {
                if matches!(e.keystroke.key.as_str(), "enter" | "space" | "down") {
                    m.open_select(&key_id, key_bounds.get(), window, cx);
                    cx.stop_propagation();
                }
            }))
    })
    .into_any_element()
}

impl AppModel {
    pub(crate) fn open_select(
        &mut self,
        id: &str,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = self.choice_options(id, cx);
        if options.is_empty() {
            return;
        }
        let project = self
            .workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string);
        let agent = self.agents.read(cx).selected_agent.clone();
        let owner = cx.entity().downgrade();
        let target = id.to_string();
        self.notifications.history_open = false;
        self.active_dropdown = Some(format!("select:{id}"));
        self.choice_menu =
            Some(cx.new(|cx| {
                SelectMenu::new(owner, target, project, agent, options, bounds, window, cx)
            }));
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn searchable_menu_filters_without_mutation_and_rejects_a_changed_agent(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::text_input::register_text_input_keybindings);
        let owner = cx.new(crate::layout_tests::fixture);
        let selected = owner.read_with(cx, |m, cx| m.agents.read(cx).selected_agent.clone());
        let choices = (0..12)
            .map(|i| Choice::new(format!("value-{i}"), format!("Option {i}"), i == 0))
            .collect();
        let (menu, visual) = cx.add_window_view(|window, cx| {
            SelectMenu::new(
                owner.downgrade(),
                "agent-filter:task_specifier".into(),
                Some("layout-project".into()),
                selected,
                choices,
                Bounds::new(point(px(800.0), px(500.0)), size(px(120.0), px(40.0))),
                window,
                cx,
            )
        });
        visual.simulate_input("Option 11");
        visual.run_until_parked();
        menu.read_with(visual, |m, cx| {
            assert_eq!(m.filtered(cx).len(), 1);
            assert_eq!(m.filtered(cx)[0].value, "value-11");
        });
        assert!(owner.read_with(visual, |m, cx| m.agents.read(cx).event_filters.is_empty()));
        // Replace the synthetic search list with a valid live value, then change its owner.
        menu.update(visual, |m, cx| {
            m.choices = vec![Choice::new("error", "Option 11", false)];
            cx.notify();
        });
        owner.update(visual, |m, cx| {
            m.agents.update(cx, |a, _| {
                a.selected_agent = Some(crate::agents::AgentIdentifier::Canonical(
                    hvc_core::llm::AgentRole::BackendAgent,
                ));
            })
        });
        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert!(owner.read_with(visual, |m, cx| m.agents.read(cx).event_filters.is_empty()));
    }

    #[test]
    fn menu_flips_and_stays_inside_small_windows() {
        for (w, h) in [(960.0, 640.0), (1440.0, 900.0)] {
            let anchor = Bounds::new(
                point(px(w - 120.0), px(h - 80.0)),
                size(px(100.0), px(40.0)),
            );
            let b = menu_bounds(anchor, size(px(w), px(h)), 20, true);
            assert!(b.bottom() < anchor.top());
            assert!(b.right() <= px(w));
            assert!(b.left() >= px(0.0));
        }
    }
}
