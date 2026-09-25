//! The Settings modal: a centred panel over the whole window with a searchable, grouped page
//! list on the left and the selected page on the right. It replaces the former Settings view;
//! `AppView::Settings` survives only as a navigation target that opens this modal.
//!
//! Layering: priority 18, below the shared select menu (20) so pickers inside a page open on
//! top of it. Focus: opening focuses the search box and remembers the previous focus; Escape
//! clears a search first and closes on the second press.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, MouseButton, Pixels, ScrollHandle, SharedString, Size, Window, deferred, div,
    ease_out_quint, point, prelude::*, px, size,
};

use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::settings::{SettingsGroup, SettingsPage, StatusTone};
use crate::settings_search::{EntryScope, MAX_ENTRIES_PER_PAGE, SearchHit, SettingsEntry};
use crate::settings_ui::PageContent;
use crate::text_input::NativeTextInput;
use crate::theme;

const NAV_WIDTH: f32 = 232.0;
/// Readable width of a page's sections; the pane itself may be wider.
const CONTENT_MAX_WIDTH: f32 = 720.0;

pub(crate) struct SettingsModalState {
    pub(crate) open: bool,
    pub(crate) search: Entity<NativeTextInput>,
    /// Last search value seen, so caret moves in the box do not re-run the search.
    query: String,
    results: Option<Vec<SearchHit>>,
    /// Row (or section) a deep link pointed at; painted with the accent tint.
    pub(crate) highlight: Option<&'static str>,
    pending_anchor: Option<&'static str>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    return_focus: Option<FocusHandle>,
    focus_pending: bool,
    restore_pending: bool,
    /// Sampled once per opening; reading it is a system call.
    reduced_motion: bool,
}

impl SettingsModalState {
    pub(crate) fn new(cx: &mut Context<AppModel>) -> Self {
        Self {
            open: false,
            search: cx
                .new(|cx| NativeTextInput::new("Search settings", cx).leading_inset(px(22.0))),
            query: String::new(),
            results: None,
            highlight: None,
            pending_anchor: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            return_focus: None,
            focus_pending: false,
            restore_pending: false,
            reduced_motion: false,
        }
    }

    pub(crate) fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    #[cfg(test)]
    pub(crate) fn results(&self) -> Option<&[SearchHit]> {
        self.results.as_deref()
    }
}

/// Panel size for a window: generous on a large screen, never touching the window edges.
pub(crate) fn modal_size(viewport: Size<Pixels>) -> Size<Pixels> {
    let width = f32::from(viewport.width);
    let height = f32::from(viewport.height);
    size(
        px((width - 64.0).clamp(320.0, 1080.0)),
        px((height - 48.0).clamp(320.0, 760.0)),
    )
}

impl AppModel {
    /// Opens Settings (on `page`, or where it was last left) over whatever view is active.
    pub(crate) fn open_settings(&mut self, page: Option<SettingsPage>, cx: &mut Context<Self>) {
        self.close_dropdown(cx);
        self.notifications.history_open = false;
        self.close_cli_model_picker(cx);
        if self.forge.new_open {
            self.forge_action(crate::forge::ForgeAction::Close, cx);
        }
        if let Some(expanded) = self.resolution.read(cx).expanded.clone() {
            self.toggle_resolution_chip(expanded, cx);
        }
        if let Some(page) = page {
            self.set_settings_page(page, cx);
        }
        if !self.settings_modal.open {
            let state = &mut self.settings_modal;
            state.open = true;
            state.focus_pending = true;
            state.restore_pending = false;
            state.reduced_motion = crate::motion::reduced_motion();
            state.highlight = None;
            state.results = None;
            state.query.clear();
            self.settings_modal
                .search
                .update(cx, |input, cx| input.clear(cx));
        }
        self.refresh_settings(cx);
        cx.notify();
    }

    /// Closes Settings. `restore_focus` gives focus back to what had it before opening (Escape,
    /// the X, the backdrop); navigating elsewhere does not.
    pub(crate) fn close_settings(&mut self, restore_focus: bool, cx: &mut Context<Self>) {
        if !self.settings_modal.open {
            return;
        }
        let state = &mut self.settings_modal;
        state.open = false;
        state.highlight = None;
        state.pending_anchor = None;
        state.restore_pending = restore_focus;
        if !restore_focus {
            state.return_focus = None;
        }
        if !state.query.is_empty() {
            self.settings_modal
                .search
                .update(cx, |input, cx| input.clear(cx));
        }
        self.settings
            .update(cx, |settings, _| settings.status_line = None);
        self.close_dropdown(cx);
        cx.notify();
    }

    pub(crate) fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_modal.open {
            self.close_settings(true, cx);
        } else {
            self.open_settings(None, cx);
        }
    }

    pub(crate) fn set_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        let previous = self.settings.read(cx).page;
        self.settings.update(cx, |settings, _| settings.page = page);
        let state = &mut self.settings_modal;
        state.highlight = None;
        state.pending_anchor = None;
        state.scroll.set_offset(point(px(0.0), px(0.0)));
        if page == SettingsPage::Usage && previous != page && self.settings_modal.open {
            self.refresh_usage(cx);
        }
        cx.notify();
    }

    /// A sidebar click. Another page keeps whatever detail (or unsaved "New …" form) this one
    /// had open; clicking the page already shown goes back to its list.
    pub(crate) fn select_settings_nav(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.settings.read(cx).page == page {
            if matches!(page, SettingsPage::Connections | SettingsPage::Routes) {
                self.close_settings_detail(cx);
            }
            return;
        }
        self.set_settings_page(page, cx);
    }

    /// A search result: its page, the list or detail it lives in, then scroll to its section
    /// with the row tinted.
    pub(crate) fn open_settings_entry(
        &mut self,
        entry: &'static SettingsEntry,
        cx: &mut Context<Self>,
    ) {
        if self.settings.read(cx).page != entry.page {
            self.set_settings_page(entry.page, cx);
        }
        self.settings.update(cx, |settings, _| match entry.scope {
            EntryScope::Page => match entry.page {
                SettingsPage::Connections => settings.connection_detail = false,
                SettingsPage::Routes => settings.route_detail = false,
                _ => {}
            },
            EntryScope::ConnectionDetail => {
                settings.connection_detail =
                    settings.selected_connection_id.is_some() || settings.creating_connection
            }
            EntryScope::RouteDetail => {
                settings.route_detail =
                    settings.selected_route_id.is_some() || settings.creating_route
            }
        });
        self.settings_modal.highlight = Some(entry.id);
        self.settings_modal.pending_anchor = Some(entry.section);
        cx.notify();
    }

    pub(crate) fn open_connection_detail(&mut self, id: String, cx: &mut Context<Self>) {
        self.select_settings_connection(id, cx);
        self.settings
            .update(cx, |settings, _| settings.connection_detail = true);
        self.settings_modal.highlight = None;
        self.settings_modal
            .scroll
            .set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// Opens a route; its first target becomes the target being edited, with its model.
    pub(crate) fn open_route_detail(&mut self, id: String, cx: &mut Context<Self>) {
        self.select_settings_route(id, cx);
        let first_target = self
            .settings
            .read(cx)
            .route()
            .and_then(|route| route.targets.first().cloned());
        if let Some(target) = first_target {
            self.select_settings_connection(target.connection_id, cx);
            self.settings_connection_model_input
                .update(cx, |input, cx| input.set_value(target.model_id, cx));
        }
        self.settings
            .update(cx, |settings, _| settings.route_detail = true);
        self.settings_modal.highlight = None;
        self.settings_modal
            .scroll
            .set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// Back to the list. A "New …" form left unsaved is dropped, and the first item is selected
    /// again so later detail rows have something to act on.
    pub(crate) fn close_settings_detail(&mut self, cx: &mut Context<Self>) {
        let (first_connection, first_route, connection, route) = {
            let settings = self.settings.read(cx);
            let snapshot = settings.snapshot.as_ref();
            (
                snapshot.and_then(|s| s.connections.first().map(|c| c.id.clone())),
                snapshot.and_then(|s| s.routes.first().map(|r| r.id.clone())),
                settings.selected_connection_id.clone(),
                settings.selected_route_id.clone(),
            )
        };
        self.settings.update(cx, |settings, _| {
            settings.connection_detail = false;
            settings.route_detail = false;
            settings.creating_connection = false;
            settings.creating_route = false;
        });
        if connection.is_none()
            && let Some(id) = first_connection
        {
            self.select_settings_connection(id, cx);
        }
        if route.is_none()
            && let Some(id) = first_route
        {
            self.select_settings_route(id, cx);
        }
        self.settings_modal.highlight = None;
        self.settings_modal
            .scroll
            .set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    pub(crate) fn settings_page_visible(&self, page: SettingsPage, cx: &App) -> bool {
        self.settings_modal.open && self.settings.read(cx).page == page
    }

    /// The page a runtime-health shortcut should open: embeddings trouble first, then provider
    /// cooldowns, else the connections list.
    pub(crate) fn settings_health_page(&self, cx: &App) -> SettingsPage {
        use hvc_core::embedding_service::ModelPullEvent;
        use hvc_core::orchestration::IndexingEvent;
        let settings = self.settings.read(cx);
        let embeddings_enabled = settings
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.config.embedding_enabled);
        let readiness_failed = settings.embedding_readiness.as_ref().is_some_and(|report| {
            !(report.ollama_reachable && report.model_installed && report.embedding_call_ok)
        });
        let indexing = matches!(
            self.indexing_status(),
            Some(
                IndexingEvent::Progress { .. }
                    | IndexingEvent::Paused { .. }
                    | IndexingEvent::Failed { .. }
            )
        );
        let pulling = matches!(
            self.model_pull_status(),
            Some(ModelPullEvent::Started { .. } | ModelPullEvent::Progress { .. })
        );
        if indexing || pulling || (embeddings_enabled && readiness_failed) {
            return SettingsPage::Embeddings;
        }
        let cooling = settings.snapshot.as_ref().is_some_and(|snapshot| {
            snapshot
                .resilience
                .states
                .iter()
                .any(|state| state.state != "closed")
        });
        if cooling {
            SettingsPage::Routes
        } else {
            SettingsPage::Connections
        }
    }

    pub(crate) fn settings_search_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.settings_modal.search.read(cx).value().to_owned();
        if value == self.settings_modal.query {
            return;
        }
        self.settings_modal.results = crate::settings_search::search(&value);
        self.settings_modal.query = value;
        cx.notify();
    }

    fn settings_modal_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                if self.settings_modal.query.is_empty() {
                    self.close_settings(true, cx);
                } else {
                    self.settings_modal
                        .search
                        .update(cx, |input, cx| input.clear(cx));
                }
                cx.stop_propagation();
            }
            "enter" if !self.settings_modal.query.is_empty() => {
                let first = self
                    .settings_modal
                    .results
                    .as_ref()
                    .and_then(|hits| hits.first())
                    .map(|hit| (hit.page, hit.entries.first().copied()));
                match first {
                    Some((_, Some(entry))) => self.open_settings_entry(entry, cx),
                    Some((page, None)) => self.select_settings_nav(page, cx),
                    None => {}
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    /// Tab / Shift-Tab: the next or previous control in paint order. While Settings is open the
    /// walk stays inside it (the view behind is covered, so its controls must not take focus).
    pub(crate) fn move_keyboard_focus(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let trap = self
            .settings_modal
            .open
            .then(|| self.settings_modal.focus.clone());
        // Bounded: a frame never has more stops than this, and a trap with no stop inside must
        // not spin.
        for _ in 0..256 {
            if forward {
                window.focus_next();
            } else {
                window.focus_prev();
            }
            match &trap {
                Some(modal) if !modal.contains_focused(window, cx) => continue,
                _ => break,
            }
        }
        cx.notify();
    }

    /// Whether keyboard focus is somewhere inside the Settings modal.
    #[cfg(test)]
    pub(crate) fn settings_focus_contains(&self, window: &Window, cx: &App) -> bool {
        self.settings_modal.focus.contains_focused(window, cx)
    }

    /// Mounted by the shell after its other overlays; also restores focus after a close.
    pub(crate) fn settings_modal_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.settings_modal.restore_pending && !self.settings_modal.open {
            self.settings_modal.restore_pending = false;
            if let Some(focus) = self.settings_modal.return_focus.take() {
                focus.focus(window);
            }
        }
        if !self.settings_modal.open {
            return None;
        }
        if self.settings_modal.focus_pending {
            self.settings_modal.focus_pending = false;
            self.settings_modal.return_focus = window.focused(cx);
            self.settings_modal
                .search
                .read(cx)
                .focus_handle(cx)
                .focus(window);
        }
        let panel_size = modal_size(window.viewport_size());
        let (page, status_line) = {
            let settings = self.settings.read(cx);
            (settings.page, settings.status_line.clone())
        };
        let highlight = self.settings_modal.highlight;
        let content = crate::settings_pages::page_content(self, page, highlight, cx);
        if let Some(anchor) = self.settings_modal.pending_anchor.take()
            && let Some(index) = content
                .sections
                .iter()
                .position(|(title, _)| *title == anchor)
        {
            // Child 0 of the scroll container is the title block.
            self.settings_modal.scroll.scroll_to_top_of_item(index + 1);
        }
        let nav = settings_nav(self, page, cx);
        let pane = content_pane(content, &self.settings_modal.scroll, status_line, cx);
        let focus = self.settings_modal.focus.clone();
        let panel = div()
            .id("settings-modal")
            .debug_selector(|| "settings-modal".into())
            .w(panel_size.width)
            .h(panel_size.height)
            .rounded_xl()
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::surface())
            .shadow_lg()
            .overflow_hidden()
            // Clicks inside the panel never reach the backdrop behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id("settings-modal-body")
                    .track_focus(&focus)
                    .size_full()
                    .flex()
                    .on_key_down(cx.listener(|model, event: &KeyDownEvent, _, cx| {
                        model.settings_modal_key_down(event, cx)
                    }))
                    .child(nav)
                    .child(pane),
            );
        let panel = if self.settings_modal.reduced_motion {
            panel.into_any_element()
        } else {
            panel
                .with_animation(
                    "settings-modal-fade",
                    Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                    |panel, delta| panel.opacity(delta),
                )
                .into_any_element()
        };
        Some(
            deferred(
                div()
                    .id("settings-modal-backdrop")
                    .debug_selector(|| "settings-modal-backdrop".into())
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .occlude()
                    .bg(gpui::rgba(0x000000aa))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|model, _, _, cx| {
                            model.close_settings(true, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(panel),
            )
            .with_priority(18)
            .into_any_element(),
        )
    }
}

fn settings_nav(model: &AppModel, current: SettingsPage, cx: &mut Context<AppModel>) -> AnyElement {
    let query = model.settings_modal.query.clone();
    let results = model.settings_modal.results.clone();
    let mut list = div()
        .id("settings-nav-list")
        .debug_selector(|| "settings-nav-list".into())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_2()
        .pb_2()
        .flex()
        .flex_col()
        .gap_0p5();
    match results {
        None => {
            let mut group: Option<SettingsGroup> = None;
            for page in SettingsPage::ALL {
                if group != Some(page.group()) {
                    group = Some(page.group());
                    list = list.child(
                        div()
                            .px_2()
                            .pt_3()
                            .pb_1()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text_subtle())
                            .child(page.group().label()),
                    );
                }
                list = list.child(nav_item(page, current, cx));
            }
        }
        Some(hits) if hits.is_empty() => {
            list = list.child(
                div()
                    .id("settings-search-empty")
                    .debug_selector(|| "settings-search-empty".into())
                    .px_2()
                    .py_3()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "No settings match \u{201c}{}\u{201d}.",
                        query.trim()
                    )),
            );
        }
        Some(hits) => {
            for hit in hits {
                list = list.child(nav_item(hit.page, current, cx));
                let more = hit.entries.len().saturating_sub(MAX_ENTRIES_PER_PAGE);
                for entry in hit.entries.into_iter().take(MAX_ENTRIES_PER_PAGE) {
                    list = list.child(result_item(entry, cx));
                }
                if more > 0 {
                    list = list.child(
                        div()
                            .pl(px(34.0))
                            .py_0p5()
                            .text_xs()
                            .text_color(theme::text_subtle())
                            .child(format!("{more} more on this page")),
                    );
                }
            }
        }
    }
    div()
        .id("settings-nav")
        .debug_selector(|| "settings-nav".into())
        .w(px(NAV_WIDTH))
        .flex_shrink_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(theme::canvas())
        .border_r_1()
        .border_color(theme::border())
        .child(
            div().p_3().child(
                div()
                    .debug_selector(|| "settings-search".into())
                    .relative()
                    .child(model.settings_modal.search.clone())
                    .child(div().absolute().left(px(12.0)).top(px(12.0)).child(
                        crate::widgets::icon(Icon::Search, px(16.0), theme::text_subtle()),
                    )),
            ),
        )
        .child(list)
        .child(
            div()
                .flex_shrink_0()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .id("settings-version-chip")
                        .debug_selector(|| "settings-version-chip".into())
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_xs()
                        .text_color(theme::text_subtle())
                        .hover(|style| style.bg(theme::surface_hover()).text_color(theme::text()))
                        .on_click(cx.listener(|model, _, _, cx| {
                            model.select_settings_nav(SettingsPage::About, cx)
                        }))
                        .child(crate::widgets::icon(
                            Icon::Info,
                            px(14.0),
                            theme::text_subtle(),
                        ))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .child(format!("Hard Vibe Code v{}", env!("CARGO_PKG_VERSION"))),
                        ),
                ),
        )
        .into_any_element()
}

fn nav_item(page: SettingsPage, current: SettingsPage, cx: &mut Context<AppModel>) -> AnyElement {
    let selected = page == current;
    let id = format!("settings-nav:{}", page.id());
    let selector = id.clone();
    let ink = if selected {
        theme::text()
    } else {
        theme::text_muted()
    };
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .text_color(ink)
        .tooltip(crate::widgets::text_tooltip(page.description()))
        .when(selected, |item| {
            item.bg(theme::surface_hover())
                .font_weight(gpui::FontWeight::MEDIUM)
        })
        .when(!selected, |item| {
            item.hover(|style| style.bg(theme::surface_raised()).text_color(theme::text()))
        })
        .on_click(cx.listener(move |model, _, _, cx| model.select_settings_nav(page, cx)))
        .child(crate::widgets::icon(page.icon(), px(16.0), ink))
        .child(page.label())
        .into_any_element()
}

fn result_item(entry: &'static SettingsEntry, cx: &mut Context<AppModel>) -> AnyElement {
    let id = format!("settings-result:{}", entry.id);
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .pl(px(34.0))
        .pr_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .text_color(theme::text_muted())
        .cursor_pointer()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .hover(|style| style.bg(theme::surface_raised()).text_color(theme::text()))
        .on_click(cx.listener(move |model, _, _, cx| model.open_settings_entry(entry, cx)))
        .child(entry.title)
        .into_any_element()
}

fn content_pane(
    content: PageContent,
    scroll: &ScrollHandle,
    status_line: Option<(StatusTone, SharedString)>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let PageContent {
        back,
        title,
        description,
        meta,
        actions,
        sections,
    } = content;
    let title_block = div()
        .w_full()
        .max_w(px(CONTENT_MAX_WIDTH))
        .flex()
        .flex_col()
        .gap_3()
        .when_some(back, |block, back| block.child(back))
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
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
                                .gap_2()
                                .child(
                                    div()
                                        .debug_selector(|| "settings-page-title".into())
                                        .text_2xl()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .when_some(description, |line, description| {
                                    line.child(crate::widgets::info_tip("page", description))
                                }),
                        )
                        .when_some(meta, |text, meta| {
                            text.child(div().text_sm().text_color(theme::text_muted()).child(meta))
                        }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .children(actions),
                ),
        );
    div()
        .relative()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id("settings-content-scroll")
                .debug_selector(|| "settings-content-scroll".into())
                .track_scroll(scroll)
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pl_8()
                .pr_12()
                .pt_6()
                .pb_10()
                .flex()
                .flex_col()
                .gap_8()
                .child(title_block)
                .children(
                    sections
                        .into_iter()
                        // A flex column, not a block: a block wrapper grew to its `w_full`
                        // child's available width and ignored the cap.
                        .map(|(_, body)| {
                            div()
                                .w_full()
                                .max_w(px(CONTENT_MAX_WIDTH))
                                .flex()
                                .flex_col()
                                .child(body)
                        }),
                ),
        )
        .when_some(status_line, |pane, (tone, text)| {
            let color = match tone {
                StatusTone::Working => theme::text_muted(),
                StatusTone::Saved => theme::success(),
                StatusTone::Failed => theme::danger(),
            };
            pane.child(
                div()
                    .id("settings-status-line")
                    .debug_selector(|| "settings-status-line".into())
                    .flex_shrink_0()
                    .px_8()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme::border())
                    .text_sm()
                    .text_color(color)
                    .child(crate::widgets::status_dot(color))
                    .child(div().min_w_0().overflow_hidden().child(text)),
            )
        })
        .child(
            div()
                .absolute()
                .top_3()
                .right_3()
                .child(crate::widgets::icon_button(
                    "settings-close",
                    Icon::X,
                    crate::widgets::Tooltip::new("Close").with_shortcut("escape"),
                    false,
                    None,
                    true,
                    cx,
                    |model, _, cx| model.close_settings(true, cx),
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::modal_size;
    use gpui::{px, size};

    #[test]
    fn the_modal_stays_inside_small_windows_and_stops_growing_on_large_ones() {
        assert_eq!(
            modal_size(size(px(960.0), px(640.0))),
            size(px(896.0), px(592.0))
        );
        assert_eq!(
            modal_size(size(px(1920.0), px(1080.0))),
            size(px(1080.0), px(760.0))
        );
    }
}
