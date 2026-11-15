//! The agent console (ADR-040): one view of an agent's run, shared by the
//! Agents page and, from M3, the bottom dock.
//!
//! - `projection`: a run's events folded into blocks (pure).
//! - `model`: each surface's list, following, selection and grouping.
//! - `view`: the rendering.
//!
//! The `AppModel` methods below are the console's actions.

pub(crate) mod model;
pub(crate) mod projection;
#[cfg(test)]
mod projection_tests;
pub(crate) mod view;

use gpui::{Context, KeyDownEvent, Window};

use crate::app_model::AppModel;
use model::RunDisplay;
use projection::ConsoleFilter;

impl AppModel {
    /// Resumes following the newest row of `key`'s console.
    pub(crate) fn console_follow(&mut self, key: &str, cx: &mut Context<Self>) {
        self.agents
            .update(cx, |agents, _| agents.consoles.surface_mut(key).follow());
        cx.notify();
    }

    /// Resumes following the newest line of an API viewer.
    pub(crate) fn console_follow_lines(&mut self, key: &str, cx: &mut Context<Self>) {
        self.agents
            .update(cx, |agents, _| agents.consoles.follow_lines(key));
        cx.notify();
    }

    pub(crate) fn console_set_filter(
        &mut self,
        key: &str,
        filter: ConsoleFilter,
        cx: &mut Context<Self>,
    ) {
        self.agents.update(cx, |agents, _| {
            agents.event_filters.insert(key.to_owned(), filter);
        });
        cx.notify();
    }

    /// Opens or closes a result, an output or a group of actions.
    pub(crate) fn console_toggle_row(&mut self, key: &str, row_key: &str, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.consoles.surface_mut(key).toggle_expanded(row_key);
        });
        cx.notify();
    }

    pub(crate) fn console_select(&mut self, key: &str, index: usize, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.consoles.surface_mut(key).selected = Some(index);
        });
        cx.notify();
    }

    pub(crate) fn console_set_display(
        &mut self,
        key: &str,
        display: RunDisplay,
        cx: &mut Context<Self>,
    ) {
        self.agents.update(cx, |agents, _| {
            agents.consoles.surface_mut(key).set_display(display);
        });
        cx.notify();
    }

    /// A row was copied: its `Copied` chip shows for 1.5 s.
    pub(crate) fn console_copied(&mut self, key: &str, row_key: String, cx: &mut Context<Self>) {
        self.agents.update(cx, |agents, _| {
            agents.consoles.surface_mut(key).copied = Some((row_key, std::time::Instant::now()));
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(model::COPIED_FOR).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// Copies the rows on screen; reasoning never goes with them.
    pub(crate) fn console_copy_all(&mut self, key: &str, cx: &mut Context<Self>) {
        let text = self.agents.update(cx, |agents, _| {
            agents
                .consoles
                .surface_mut(key)
                .rows()
                .rows
                .iter()
                .map(model::ConsoleRow::text)
                .collect::<Vec<_>>()
                .join("\n\n")
        });
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.console_copied(key, "*".into(), cx);
    }

    /// Copies the selected row.
    fn console_copy_selected(&mut self, key: &str, cx: &mut Context<Self>) {
        let copied = self.agents.update(cx, |agents, _| {
            let surface = agents.consoles.surface_mut(key);
            let rows = surface.rows();
            surface
                .selected
                .and_then(|index| rows.rows.get(index))
                .map(|row| (row.key().to_owned(), row.text()))
        });
        if let Some((row_key, text)) = copied {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
            self.console_copied(key, row_key, cx);
        }
    }

    /// The search open on `key`'s console, if any, and what it looks for.
    pub(crate) fn console_search_query(&self, key: &str, cx: &gpui::App) -> Option<String> {
        self.agent_console_search
            .as_ref()
            .filter(|(open, _)| open == key)
            .map(|_| self.agent_console_search_input.read(cx).value().to_owned())
    }

    /// The current match, wrapped into `0..count`.
    pub(crate) fn console_search_position(&self, key: &str, count: usize) -> Option<usize> {
        self.agent_console_search
            .as_ref()
            .filter(|(open, _)| open == key && count > 0)
            .map(|(_, position)| position % count)
    }

    pub(crate) fn console_open_search(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .agent_console_search
            .as_ref()
            .is_none_or(|(open, _)| open != key)
        {
            self.agent_console_search = Some((key.to_owned(), 0));
            self.agent_console_search_input
                .update(cx, |input, cx| input.clear(cx));
        }
        let input = gpui::Focusable::focus_handle(self.agent_console_search_input.read(cx), cx);
        window.focus(&input);
        cx.notify();
    }

    pub(crate) fn console_close_search(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .agent_console_search
            .as_ref()
            .is_some_and(|(open, _)| open == key)
        {
            self.agent_console_search = None;
            let focus = self.agents.read(cx).consoles.focus(key, cx);
            window.focus(&focus);
            cx.notify();
        }
    }

    /// Moves to the next or previous match and brings it into view.
    pub(crate) fn console_search_step(&mut self, key: &str, step: isize, cx: &mut Context<Self>) {
        let Some(query) = self.console_search_query(key, cx) else {
            return;
        };
        let matches = self.agents.update(cx, |agents, _| {
            model::search_matches(&agents.consoles.surface_mut(key).rows().rows, Some(&query))
        });
        if matches.is_empty() {
            return;
        }
        let Some((_, position)) = self.agent_console_search.as_mut() else {
            return;
        };
        let count = matches.len();
        *position = (*position % count)
            .checked_add_signed(step)
            .map_or(count - 1, |next| next % count);
        let row = matches[*position];
        self.agents.update(cx, |agents, _| {
            agents.consoles.surface_mut(key).reveal(row);
        });
        cx.notify();
    }

    /// The console's keys (specification 8.4). Returns whether it handled one.
    pub(crate) fn console_key(
        &mut self,
        key: &str,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let control = keystroke.modifiers.control;
        let searching = self
            .agent_console_search
            .as_ref()
            .is_some_and(|(open, _)| open == key);
        match keystroke.key.as_str() {
            "f" if control => self.console_open_search(key, window, cx),
            "c" if control && !searching => self.console_copy_selected(key, cx),
            "escape" if searching => self.console_close_search(key, window, cx),
            "enter" if searching => {
                let step = if keystroke.modifiers.shift { -1 } else { 1 };
                self.console_search_step(key, step, cx);
            }
            "enter" if !searching => {
                let row_key = self.agents.update(cx, |agents, _| {
                    let surface = agents.consoles.surface_mut(key);
                    let rows = surface.rows();
                    surface
                        .selected
                        .and_then(|index| rows.rows.get(index))
                        .map(|row| row.key().to_owned())
                });
                match row_key {
                    Some(row_key) => self.console_toggle_row(key, &row_key, cx),
                    None => return false,
                }
            }
            "up" | "down" if !searching => {
                let step = if keystroke.key == "up" { -1 } else { 1 };
                self.agents.update(cx, |agents, _| {
                    agents.consoles.surface_mut(key).select_step(step);
                });
                cx.notify();
            }
            "end" if !searching => self.console_follow(key, cx),
            _ => return false,
        }
        true
    }
}
