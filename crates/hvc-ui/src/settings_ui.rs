//! Building blocks of the Settings modal's pages: a page is a title block plus sections; a
//! section is a heading and rows separated by hairlines; a row is a title on the left and exactly
//! one control on the right (or a wide body below). Explanations live in an "i" beside the
//! title (`widgets::info_tip`); only short live values (a current setting, a default) stay
//! visible under it.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Bounds, Context, IntoElement, Pixels, SharedString, canvas, div, prelude::*, px,
};

use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::select_menu::Choice;
use crate::settings_search::{SettingsEntry, entry_by_id};
use crate::theme;

/// One page as the modal lays it out: the title block, then each section as a direct child of
/// the scroll container, so a deep link can scroll to a section by its position.
pub(crate) struct PageContent {
    pub(crate) back: Option<AnyElement>,
    pub(crate) title: SharedString,
    /// Shown in the "i" beside the title.
    pub(crate) description: Option<SharedString>,
    /// A short visible line under the title (the project a page applies to, a key's state).
    pub(crate) meta: Option<SharedString>,
    pub(crate) actions: Vec<AnyElement>,
    pub(crate) sections: Vec<(&'static str, AnyElement)>,
}

impl PageContent {
    pub(crate) fn new(title: impl Into<SharedString>) -> Self {
        Self {
            back: None,
            title: title.into(),
            description: None,
            meta: None,
            actions: Vec::new(),
            sections: Vec::new(),
        }
    }

    pub(crate) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub(crate) fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta = Some(meta.into());
        self
    }

    pub(crate) fn back(mut self, back: impl IntoElement) -> Self {
        self.back = Some(back.into_any_element());
        self
    }

    pub(crate) fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub(crate) fn section(mut self, title: &'static str, body: impl IntoElement) -> Self {
        self.sections.push((title, body.into_any_element()));
        self
    }
}

/// A section: bold heading (with its explanation in an "i"), then its rows.
pub(crate) fn section(
    title: &'static str,
    description: Option<&str>,
    rows: Vec<AnyElement>,
) -> AnyElement {
    let anchor = title;
    div()
        .debug_selector(move || format!("settings-section:{anchor}"))
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                )
                .when_some(description.map(str::to_owned), |heading, description| {
                    heading.child(crate::widgets::info_tip(
                        format!("section:{title}"),
                        description,
                    ))
                }),
        )
        .child(
            div()
                .mt_2()
                .flex()
                .flex_col()
                .children(rows.into_iter().enumerate().map(|(index, row)| {
                    // Flex, not block: a block wrapper let a row with short text take the
                    // scroll pane's width instead of the section's.
                    div()
                        .flex()
                        .flex_col()
                        .when(index > 0, |divider| {
                            divider.border_t_1().border_color(theme::border())
                        })
                        .child(row)
                })),
        )
        .into_any_element()
}

fn row_frame(id: &str, highlighted: bool) -> gpui::Stateful<gpui::Div> {
    let row_id = format!("settings-row:{id}");
    let selector = row_id.clone();
    div()
        .id(SharedString::from(row_id))
        .debug_selector(move || selector.clone())
        .px_3()
        .py_3()
        .rounded_md()
        .when(highlighted, |row| row.bg(theme::accent_soft()))
}

fn row_text(
    id: &str,
    title: SharedString,
    info: Option<SharedString>,
    meta: Option<SharedString>,
) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(title),
                )
                .when_some(info, |line, info| {
                    line.child(crate::widgets::info_tip(id.to_owned(), info))
                }),
        )
        .when_some(meta, |text, meta| {
            text.child(div().text_sm().text_color(theme::text_muted()).child(meta))
        })
}

/// A setting from the index: its title (with the description in an "i") on the left, `control`
/// on the right.
pub(crate) fn row(
    id: &'static str,
    highlight: Option<&str>,
    control: impl IntoElement,
) -> AnyElement {
    let entry = entry_by_id(id);
    row_with(
        entry.id,
        entry.title.into(),
        Some(entry.description.into()),
        None,
        highlight == Some(entry.id),
        control,
    )
}

/// Same as `row`, with the texts given: `info` goes in the "i", `meta` is a short visible value
/// under the title ("Current: 40").
pub(crate) fn row_with(
    id: &str,
    title: SharedString,
    info: Option<SharedString>,
    meta: Option<SharedString>,
    highlighted: bool,
    control: impl IntoElement,
) -> AnyElement {
    row_frame(id, highlighted)
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(row_text(id, title, info, meta))
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

/// A setting whose editor needs the full width (inputs, lists, tables): text on top, body below.
pub(crate) fn stacked_row(
    id: &'static str,
    highlight: Option<&str>,
    body: impl IntoElement,
) -> AnyElement {
    let entry: &SettingsEntry = entry_by_id(id);
    row_frame(entry.id, highlight == Some(entry.id))
        .flex()
        .flex_col()
        .gap_3()
        .child(row_text(
            entry.id,
            entry.title.into(),
            Some(entry.description.into()),
            None,
        ))
        .child(div().w_full().min_w_0().child(body))
        .into_any_element()
}

/// A plain line inside a section (a status, an empty state, a note).
pub(crate) fn note(text: impl Into<SharedString>) -> AnyElement {
    div()
        .px_3()
        .py_2()
        .text_sm()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// The switch on the right of a row. Only the switch toggles, so a click on the row's text
/// never flips a setting by accident.
pub(crate) fn switch(
    id: &'static str,
    on: bool,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .rounded_full()
        .border_1()
        .border_color(gpui::transparent_black())
        .when(enabled, |switch| {
            switch
                .cursor_pointer()
                .tab_index(0)
                .focus_visible(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(crate::widgets::switch(on, enabled))
        .into_any_element()
}

/// A compact picker for the right of a row: the current value and a chevron; opens the shared
/// select menu, which draws above the modal.
pub(crate) fn select(
    id: impl Into<String>,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    if crate::choices::segments(&id).is_some() {
        let value = value.into();
        return segmented(id, value.as_ref(), enabled, cx);
    }
    let debug = id.clone();
    let key_id = id.clone();
    let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
    let measured = bounds.clone();
    let key_bounds = bounds.clone();
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || debug.clone())
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme::accent()))
        .relative()
        .min_w(px(140.0))
        .max_w(px(280.0))
        .h(px(34.0))
        .px_3()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::canvas())
        .text_sm()
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
            Icon::ChevronDown,
            px(14.0),
            theme::text_muted(),
        ))
        .when(enabled, |control| {
            control
                .cursor_pointer()
                .hover(|style| style.border_color(theme::accent()))
                .on_click(cx.listener(move |model, _, window, cx| {
                    model.open_select(&id, bounds.get(), window, cx);
                }))
                .on_key_down(
                    cx.listener(move |model, event: &gpui::KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space" | "down") {
                            model.open_select(&key_id, key_bounds.get(), window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
        })
        .into_any_element()
}

/// Two to four exclusive choices as a segmented track without a label (the row is the label).
/// Segment ids stay `{id}:{value}`.
pub(crate) fn segmented(
    id: String,
    value: &str,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let segments: Vec<Choice> = crate::choices::segments(&id)
        .unwrap_or_default()
        .into_iter()
        .map(|(v, l)| {
            Choice::new(
                v,
                l,
                value.eq_ignore_ascii_case(v) || value.eq_ignore_ascii_case(l),
            )
        })
        .collect();
    let debug = id.clone();
    crate::widgets::segment_track(
        div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || debug.clone()),
    )
    .children(segments.into_iter().map(|choice| {
        let target = id.clone();
        let chosen = choice.value.clone();
        let segment_id = SharedString::from(format!("{id}:{}", choice.value));
        let selector = segment_id.clone();
        crate::widgets::segment(
            div()
                .id(segment_id)
                .debug_selector(move || selector.to_string()),
            choice.selected,
        )
        .when(!enabled, |segment| {
            segment.cursor_default().text_color(theme::text_subtle())
        })
        .when(enabled && !choice.selected, |segment| {
            segment.on_click(
                cx.listener(move |model, _, _, cx| model.apply_choice(&target, &chosen, cx)),
            )
        })
        .child(choice.label)
    }))
    .into_any_element()
}

/// A drill-in row for a list item: status dot, name, a muted summary and a chevron.
pub(crate) fn nav_row(
    id: SharedString,
    dot: gpui::Rgba,
    title: impl Into<SharedString>,
    meta: impl Into<SharedString>,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .w_full()
        .px_3()
        .py_3()
        .rounded_md()
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        .child(crate::widgets::status_dot(dot))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(meta.into()),
                ),
        )
        .child(crate::widgets::icon(
            Icon::ChevronRight,
            px(16.0),
            theme::text_subtle(),
        ))
        .into_any_element()
}

/// "← Connections": leaves a drill-in detail for its list.
pub(crate) fn back_link(
    id: &'static str,
    label: &'static str,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .gap_1()
        .text_sm()
        .text_color(theme::text_muted())
        .cursor_pointer()
        .hover(|style| style.text_color(theme::text()))
        .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        .child(crate::widgets::icon(
            Icon::ChevronLeft,
            px(16.0),
            theme::text_muted(),
        ))
        .child(label)
        .into_any_element()
}

/// Key names as they read on a keyboard: `ctrl-shift-]` → Ctrl, Shift, ].
pub(crate) fn key_names(keys: &str) -> Vec<String> {
    let mut parts: Vec<&str> = Vec::new();
    let mut rest = keys;
    while let Some(index) = rest.find('-') {
        // A trailing "-" is the minus key itself ("ctrl--").
        if index == 0 {
            break;
        }
        parts.push(&rest[..index]);
        rest = &rest[index + 1..];
    }
    parts.push(if rest.is_empty() { "-" } else { rest });
    parts
        .into_iter()
        .map(|part| match part {
            "ctrl" => "Ctrl".to_string(),
            "shift" => "Shift".to_string(),
            "alt" => "Alt".to_string(),
            "enter" => "Enter".to_string(),
            "escape" => "Esc".to_string(),
            "backspace" => "Backspace".to_string(),
            "delete" => "Delete".to_string(),
            "home" => "Home".to_string(),
            "end" => "End".to_string(),
            "space" => "Space".to_string(),
            "tab" => "Tab".to_string(),
            "left" => "←".to_string(),
            "right" => "→".to_string(),
            "up" => "↑".to_string(),
            "down" => "↓".to_string(),
            other => other.to_uppercase(),
        })
        .collect()
}

pub(crate) fn keycaps(keys: &str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(key_names(keys).into_iter().map(|name| {
            div()
                .min_w(px(22.0))
                .px_1p5()
                .py_0p5()
                .flex()
                .justify_center()
                .rounded_md()
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::surface_raised())
                .font_family(theme::FONT_MONO)
                .text_xs()
                .child(name)
        }))
        .into_any_element()
}

/// A short sentence inside a row body (a state, an empty list): muted text, not monospace.
pub(crate) fn hint(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// A monospace value on the right of a row.
pub(crate) fn readout(text: impl Into<SharedString>) -> AnyElement {
    div()
        .font_family(theme::FONT_MONO)
        .text_sm()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::key_names;

    #[test]
    fn key_names_read_like_a_keyboard() {
        assert_eq!(key_names("ctrl-shift-]"), ["Ctrl", "Shift", "]"]);
        assert_eq!(key_names("ctrl-,"), ["Ctrl", ","]);
        assert_eq!(key_names("enter"), ["Enter"]);
        assert_eq!(key_names("ctrl--"), ["Ctrl", "-"]);
        assert_eq!(key_names("shift-left"), ["Shift", "←"]);
        assert_eq!(key_names("d"), ["D"]);
    }
}
