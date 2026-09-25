//! Settings pages that do not edit the provider configuration: General, Usage, Keyboard
//! shortcuts and About, plus `page_content`, which picks the renderer for the current page.

use gpui::{AnyElement, ClipboardItem, Context, SharedString, div, prelude::*};

use crate::app_model::AppModel;
use crate::settings::SettingsPage;
use crate::settings_ui::{
    PageContent, keycaps, note, readout, row, row_with, section, stacked_row, switch,
};
use crate::settings_view::action_with;
use crate::theme;
use crate::widgets::ButtonVariant;

/// The body of the modal's content pane for `page`. Pages that edit the provider configuration
/// wait for its snapshot and offer Reload when it could not be read.
pub(crate) fn page_content(
    model: &AppModel,
    page: SettingsPage,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    if page.needs_snapshot() && settings.snapshot.is_none() {
        let loading = settings.loading;
        return PageContent::new(page.label())
            .description(page.description())
            .action(crate::settings_view::reload_action(model, cx))
            .section(
                "Status",
                section(
                    "Status",
                    None,
                    vec![note(if loading {
                        "Loading the LLM configuration…"
                    } else {
                        "The configuration could not be read. Use Reload to try again."
                    })],
                ),
            );
    }
    match page {
        SettingsPage::General => general_page(model, hl, cx),
        SettingsPage::Usage => usage_page(model, cx),
        SettingsPage::Shortcuts => shortcuts_page(),
        SettingsPage::Connections => crate::settings_view::connections_page(model, hl, cx),
        SettingsPage::Routes => crate::settings_view::routes_page(model, hl, cx),
        SettingsPage::Embeddings => crate::settings_view::embeddings_page(model, hl, cx),
        SettingsPage::SecondBrain => crate::settings_view::second_brain_page(model, hl, cx),
        SettingsPage::Preparation => crate::settings_view::preparation_page(model, hl, cx),
        SettingsPage::Composition => crate::composition_ui::composition_page(model, hl, cx),
        SettingsPage::Timeouts => crate::settings_view::timeouts_page(hl),
        SettingsPage::Labs => crate::composition_ui::labs_page(model, hl, cx),
        SettingsPage::About => about_page(model, hl, cx),
    }
}

// ---------------------------------------------------------------------------------------------
// General

fn general_page(model: &AppModel, hl: Option<&str>, cx: &mut Context<AppModel>) -> PageContent {
    let rail_visible = !model.context_rail_collapsed(cx);
    let reduced_motion = model.settings_modal.reduced_motion();
    PageContent::new(SettingsPage::General.label())
        .description(SettingsPage::General.description())
        .section(
            "Workflow",
            section(
                "Workflow",
                None,
                vec![row(
                    "auto-open-review",
                    hl,
                    switch(
                        "settings-auto-open-review",
                        model.auto_open_review,
                        true,
                        cx,
                        |model, cx| model.set_auto_open_review(!model.auto_open_review, cx),
                    ),
                )],
            ),
        )
        .section(
            "Layout",
            section(
                "Layout",
                None,
                vec![row(
                    "context-rail",
                    hl,
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(keycaps("ctrl-b"))
                        .child(switch(
                            "settings-context-rail",
                            rail_visible,
                            true,
                            cx,
                            |model, cx| model.toggle_context_rail(cx),
                        )),
                )],
            ),
        )
        .section(
            "Accessibility",
            section(
                "Accessibility",
                None,
                vec![row(
                    "reduced-motion",
                    hl,
                    readout(if reduced_motion { "On" } else { "Off" }),
                )],
            ),
        )
}

// ---------------------------------------------------------------------------------------------
// Usage

fn usage_page(model: &AppModel, cx: &mut Context<AppModel>) -> PageContent {
    let loading = model.settings.read(cx).usage.loading;
    PageContent::new(SettingsPage::Usage.label())
        .description(crate::settings_search::entry_by_id("usage-dashboard").description)
        .action(action_with(
            "usage-refresh",
            if loading { "Refreshing…" } else { "Refresh" },
            ButtonVariant::Secondary,
            !loading,
            cx,
            |model, cx| model.refresh_usage(cx),
        ))
        // No heading of its own: the page title already says Usage.
        .section(
            "Token usage",
            div()
                .debug_selector(|| "settings-section:Token usage".into())
                .child(crate::usage::usage_panel(model, cx)),
        )
}

// ---------------------------------------------------------------------------------------------
// Keyboard shortcuts

/// One line of the shortcuts page. `bound` shortcuts are key bindings registered with GPUI (a
/// test checks each one exists); the others are handled by the overlays themselves.
#[derive(Debug, Clone)]
pub(crate) struct Shortcut {
    pub(crate) section: &'static str,
    pub(crate) keys: String,
    pub(crate) action: String,
    /// Read by the catalog test, which checks every bound shortcut is registered.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) bound: bool,
}

fn shortcut(section: &'static str, keys: &str, action: &str, bound: bool) -> Shortcut {
    Shortcut {
        section,
        keys: keys.to_owned(),
        action: action.to_owned(),
        bound,
    }
}

/// The catalog, in page order. Section titles match the search index entries.
pub(crate) fn shortcut_catalog() -> Vec<Shortcut> {
    const NAV: &str = "Navigation";
    const REVIEW: &str = "Review & failure";
    const EDIT: &str = "Editor & text";
    const OVERLAYS: &str = "Overlays & menus";
    let mut catalog: Vec<Shortcut> = crate::shell::RAIL_VIEWS
        .iter()
        .enumerate()
        .map(|(index, view)| {
            shortcut(
                NAV,
                &format!("ctrl-{}", index + 1),
                &format!("Go to {}", view.short_label()),
                true,
            )
        })
        .collect();
    catalog.extend([
        shortcut(NAV, "ctrl-,", "Open or close Settings", true),
        shortcut(
            NAV,
            "ctrl-shift-[",
            "Previous section of the view, or previous Settings page",
            true,
        ),
        shortcut(
            NAV,
            "ctrl-shift-]",
            "Next section of the view, or next Settings page",
            true,
        ),
        shortcut(NAV, "ctrl-enter", "Run the suggested next step", true),
        shortcut(NAV, "ctrl-b", "Show or hide the context rail", true),
        shortcut(
            REVIEW,
            "enter",
            "Approve the checkpoint open in Review",
            true,
        ),
        shortcut(REVIEW, "r", "Request a revision of the checkpoint", true),
        shortcut(REVIEW, "d", "Reject the checkpoint", true),
        shortcut(REVIEW, "enter", "Retry a failed stage", true),
        shortcut(REVIEW, "r", "Revise a failed stage", true),
        shortcut(REVIEW, "d", "Abort a failed stage", true),
        shortcut(EDIT, "ctrl-z", "Undo in the editor", true),
        shortcut(EDIT, "ctrl-y", "Redo in the editor", true),
        shortcut(EDIT, "ctrl-a", "Select all", true),
        shortcut(EDIT, "ctrl-c", "Copy", true),
        shortcut(EDIT, "ctrl-x", "Cut", true),
        shortcut(EDIT, "ctrl-v", "Paste", true),
        shortcut(
            EDIT,
            "ctrl-backspace",
            "Delete the previous word in a field",
            true,
        ),
        shortcut(EDIT, "enter", "Send in the Companion composer", true),
        shortcut(
            EDIT,
            "shift-enter",
            "New line in the Companion composer",
            true,
        ),
        shortcut(
            OVERLAYS,
            "escape",
            "Close a menu, an overlay or Settings",
            false,
        ),
        shortcut(
            OVERLAYS,
            "escape",
            "In Settings: clear the search first, then close",
            false,
        ),
        shortcut(
            OVERLAYS,
            "enter",
            "In Settings search: open the first result",
            false,
        ),
        shortcut(OVERLAYS, "down", "Open a focused picker", false),
    ]);
    catalog
}

fn shortcuts_page() -> PageContent {
    let catalog = shortcut_catalog();
    let mut page = PageContent::new(SettingsPage::Shortcuts.label())
        .description(SettingsPage::Shortcuts.description());
    let mut sections: Vec<&'static str> = Vec::new();
    for shortcut in &catalog {
        if !sections.contains(&shortcut.section) {
            sections.push(shortcut.section);
        }
    }
    for title in sections {
        let description = crate::settings_search::ENTRIES
            .iter()
            .find(|entry| entry.page == SettingsPage::Shortcuts && entry.section == title)
            .map(|entry| entry.description);
        let rows = catalog
            .iter()
            .enumerate()
            .filter(|(_, shortcut)| shortcut.section == title)
            .map(|(index, shortcut)| {
                row_with(
                    &format!("shortcut-{index}"),
                    shortcut.action.clone().into(),
                    None,
                    None,
                    false,
                    keycaps(&shortcut.keys),
                )
            })
            .collect();
        page = page.section(title, section(title, description, rows));
    }
    page
}

// ---------------------------------------------------------------------------------------------
// About

fn about_page(model: &AppModel, hl: Option<&str>, cx: &mut Context<AppModel>) -> PageContent {
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let (data_dir, database) = match model.core.as_ref() {
        Some(core) => (
            crate::widgets::display_path(&core.paths().data_dir().to_string_lossy()),
            crate::widgets::display_path(&core.paths().database_path().to_string_lossy()),
        ),
        None => (
            "Not available in this session".to_string(),
            "Not available in this session".to_string(),
        ),
    };
    let copied = |key: &str| model.notifications.copied.contains_key(key);
    let data_copied = copied("about-data-dir");
    let diagnostics_copied = copied("about-diagnostics");
    let health: Vec<AnyElement> = crate::context_rail::health_lines(model, cx)
        .into_iter()
        .map(|(_, color, text)| {
            // The rail paints healthy lines in plain text colour; here the dot says "fine".
            let dot = if color == theme::text() {
                theme::success()
            } else {
                color
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::widgets::status_dot(dot))
                .child(readout(text))
                .into_any_element()
        })
        .collect();
    let copy_path = data_dir.clone();
    PageContent::new(SettingsPage::About.label())
        .description(SettingsPage::About.description())
        .section(
            "Application",
            section(
                "Application",
                None,
                vec![
                    row(
                        "about-version",
                        hl,
                        readout(format!("v{} · {build}", env!("CARGO_PKG_VERSION"))),
                    ),
                    stacked_row(
                        "about-data-dir",
                        hl,
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .font_family(theme::FONT_MONO)
                                    .text_sm()
                                    .child(SharedString::from(data_dir)),
                            )
                            .child(action_with(
                                "about-copy-data-dir",
                                if data_copied { "Copied" } else { "Copy" },
                                ButtonVariant::Ghost,
                                model.core.is_some(),
                                cx,
                                move |model, cx| {
                                    model.copy_to_clipboard("about-data-dir", copy_path.clone(), cx)
                                },
                            )),
                    ),
                    stacked_row(
                        "about-database",
                        hl,
                        div()
                            .font_family(theme::FONT_MONO)
                            .text_sm()
                            .child(SharedString::from(database)),
                    ),
                ],
            ),
        )
        .section(
            "Runtime",
            section(
                "Runtime",
                None,
                vec![stacked_row(
                    "about-runtime",
                    hl,
                    div().flex().flex_col().gap_1().children(health),
                )],
            ),
        )
        .section(
            "Diagnostics",
            section(
                "Diagnostics",
                None,
                vec![row(
                    "about-diagnostics",
                    hl,
                    action_with(
                        "about-copy-diagnostics",
                        if diagnostics_copied { "Copied" } else { "Copy" },
                        ButtonVariant::Secondary,
                        true,
                        cx,
                        |model, cx| {
                            let text = model.diagnostics_text(cx);
                            model.copy_to_clipboard("about-diagnostics", text, cx)
                        },
                    ),
                )],
            ),
        )
}

impl AppModel {
    /// Plain-text facts for a bug report: version, build, OS, data directory and the runtime
    /// counts the context rail shows. No keys, prompts or project content.
    pub(crate) fn diagnostics_text(&self, cx: &gpui::App) -> String {
        let data_dir = self
            .core
            .as_ref()
            .map(|core| crate::widgets::display_path(&core.paths().data_dir().to_string_lossy()))
            .unwrap_or_else(|| "unavailable".into());
        let mut lines = vec![
            format!(
                "Hard Vibe Code v{} ({})",
                env!("CARGO_PKG_VERSION"),
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                }
            ),
            format!("OS: {} {}", std::env::consts::OS, std::env::consts::ARCH),
            format!("Data directory: {data_dir}"),
        ];
        lines.extend(
            crate::context_rail::health_lines(self, cx)
                .into_iter()
                .map(|(_, _, text)| text.split_whitespace().collect::<Vec<_>>().join(" ")),
        );
        lines.join("\n")
    }

    /// Writes `text` to the clipboard and marks `key` as copied for three seconds (the notice
    /// clock already wakes the window to clear it).
    pub(crate) fn copy_to_clipboard(&mut self, key: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notifications
            .copied
            .insert(key.to_owned(), std::time::Instant::now());
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::shortcut_catalog;

    #[gpui::test]
    fn every_bound_shortcut_on_the_page_is_a_registered_key_binding(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            crate::native_editor::register_editor_keybindings(cx);
            crate::text_input::register_text_input_keybindings(cx);
            crate::hitl::register_hitl_keybindings(cx);
            crate::failure_checkpoint::register_failure_keybindings(cx);
            crate::shell::register_navigation_keybindings(cx);
            for shortcut in shortcut_catalog().into_iter().filter(|s| s.bound) {
                let keystroke = gpui::Keystroke::parse(&shortcut.keys).unwrap();
                assert!(
                    !cx.all_bindings_for_input(&[keystroke]).is_empty(),
                    "`{}` ({}) is listed but not bound",
                    shortcut.keys,
                    shortcut.action
                );
            }
        });
        // ctrl-N follows the rail order.
        let catalog = shortcut_catalog();
        for (index, view) in crate::shell::RAIL_VIEWS.iter().enumerate() {
            assert_eq!(catalog[index].keys, format!("ctrl-{}", index + 1));
            assert!(catalog[index].action.ends_with(view.short_label()));
        }
    }
}
