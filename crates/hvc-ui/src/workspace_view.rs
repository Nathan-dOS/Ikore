use std::path::Path;

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_editor::{MarkdownBlock, parse_markdown};

use crate::app_model::AppModel;
use crate::file_tree::TreeRow;
use crate::theme;
use crate::workspace::{
    BrownfieldOnboardingModel, BrownfieldOnboardingState, TabSummary, WorkspaceTabModel,
};

pub(crate) fn workspace_content_panel(
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    // Only summaries leave the model: cloning every tab's text (twice) and the whole tree on
    // every frame used to cost tens of megabytes per frame with a few large files open.
    let (
        root_path,
        tree_rows,
        tree_capped,
        tabs,
        active_tab,
        preview,
        preview_mode,
        active_path,
        editor,
        context_drawer_open,
        brownfield,
    ) = {
        let workspace = model.workspace.read(cx);
        let active = workspace.active_tab();
        (
            workspace.root_path.clone(),
            crate::file_tree::flatten(&workspace.tree, &workspace.folders),
            crate::file_tree::is_capped(&workspace.tree),
            workspace
                .tabs
                .iter()
                .map(WorkspaceTabModel::summary)
                .collect::<Vec<_>>(),
            active.map(WorkspaceTabModel::summary),
            // Parsed only while the preview is what the developer is looking at.
            active
                .filter(|_| workspace.preview_mode)
                .map(|tab| parse_markdown(&tab.content)),
            workspace.preview_mode,
            workspace
                .active_relative_path
                .as_ref()
                .map(ToString::to_string),
            workspace.editor.clone(),
            workspace.context_drawer_open,
            workspace.brownfield.clone(),
        )
    };
    let root_label = root_path
        .as_ref()
        .map(|root| crate::widgets::display_path(root))
        .unwrap_or_else(|| "No workspace selected".into());
    let active_for_conflict = active_path.clone();
    let context_documents = {
        let vault = model.vault.read(cx);
        vault
            .context_paths(active_path.as_deref())
            .into_iter()
            .filter_map(|path| {
                vault
                    .documents
                    .get(&path)
                    .map(|content| (path, content.clone()))
            })
            .collect::<Vec<_>>()
    };

    let editor_panel = div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .rounded_lg()
        .overflow_hidden()
        .child(workspace_editor_surface(
            tabs,
            active_path.clone(),
            active_tab.clone(),
            preview,
            preview_mode,
            editor,
            cx,
        ))
        .into_any_element();
    let tree_panel =
        workspace_tree_panel(tree_rows, tree_capped, active_path.as_deref(), cx).into_any_element();
    let columns = crate::split_pane::split_pane(
        "workspace-columns",
        &model.workspace_tree_split,
        tree_panel,
        editor_panel,
        cx,
    );
    let columns = if context_drawer_open {
        crate::split_pane::split_pane(
            "workspace-context-columns",
            &model.workspace_context_split,
            columns,
            context_drawer(active_tab.as_ref(), context_documents).into_any_element(),
            cx,
        )
    } else {
        columns
    };

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .min_w_0()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("Workspace"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::text_muted())
                                .overflow_hidden()
                                .child(root_label),
                        ),
                )
                .child(
                    div().flex().items_center().gap_2().child(
                        crate::widgets::button_frame(
                            div().id("choose-workspace"),
                            crate::widgets::ButtonVariant::Secondary,
                            true,
                        )
                        .on_click(cx.listener(|model, _, window, cx| {
                            model.choose_workspace(window, cx);
                        }))
                        .child(crate::widgets::icon(
                            crate::icons::Icon::FolderOpen,
                            px(14.0),
                            theme::text_muted(),
                        ))
                        .gap_2()
                        .child("Open workspace"),
                    ),
                ),
        )
        .when(
            brownfield.state != BrownfieldOnboardingState::NotApplicable,
            |panel| {
                panel.child(brownfield_onboarding_card(
                    brownfield,
                    active_path.clone(),
                    model.confirm_brownfield_revoke,
                    cx,
                ))
            },
        )
        .when_some(
            active_tab.as_ref().and_then(|tab| tab.conflict.clone()),
            |panel, conflict| {
                let reload_path = active_for_conflict.clone().unwrap_or_default();
                let overwrite_path = reload_path.clone();
                let is_confirming =
                    model.confirm_workspace_overwrite.as_deref() == Some(overwrite_path.as_str());
                panel.child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::warning())
                        .bg(theme::warning_soft())
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .child(format!("Revision conflict: {conflict}")),
                        )
                        .child(
                            div()
                                .id("workspace-conflict-reload")
                                .px_3()
                                .py_1()
                                .rounded_sm()
                                .tab_index(0)
                                .bg(theme::surface_raised())
                                .cursor_pointer()
                                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.set_confirm_workspace_overwrite(None, cx);
                                    model.resolve_workspace_conflict_by_reload(
                                        reload_path.clone(),
                                        cx,
                                    );
                                }))
                                .child("Reload"),
                        )
                        .child(
                            div()
                                .id("workspace-conflict-overwrite")
                                .px_3()
                                .py_1()
                                .rounded_sm()
                                .tab_index(0)
                                .bg(if is_confirming {
                                    theme::danger()
                                } else {
                                    theme::danger_soft()
                                })
                                .text_color(if is_confirming {
                                    theme::canvas()
                                } else {
                                    theme::danger()
                                })
                                .cursor_pointer()
                                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    if is_confirming {
                                        model.resolve_workspace_conflict_by_overwrite(
                                            overwrite_path.clone(),
                                            cx,
                                        );
                                    } else {
                                        model.set_confirm_workspace_overwrite(
                                            Some(overwrite_path.clone()),
                                            cx,
                                        );
                                    }
                                }))
                                .child(if is_confirming {
                                    "Confirm overwrite"
                                } else {
                                    "Overwrite"
                                }),
                        ),
                )
            },
        )
        // Only takes room when a save failed; an always-reserved slot left a blank band above
        // the tree.
        .when_some(
            active_tab.as_ref().and_then(|tab| tab.save_error.clone()),
            |panel, save_error| {
                panel.child(
                    div()
                        .id("workspace-save-feedback")
                        .max_h(px(40.0))
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .px_2()
                        .text_sm()
                        .text_color(theme::danger())
                        .child(save_error),
                )
            },
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_hidden()
                .child(columns),
        )
}

fn brownfield_onboarding_card(
    onboarding: BrownfieldOnboardingModel,
    active_path: Option<String>,
    confirm_revoke: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let (title, description) = match onboarding.state {
        BrownfieldOnboardingState::NotApplicable => ("", ""),
        BrownfieldOnboardingState::Pending => (
            "Existing workspace detected",
            "Gradual mapping enumerates only safe metadata and respects .gitignore. No scripts, builds, or LLM calls will be executed.",
        ),
        BrownfieldOnboardingState::Disabled => (
            "Gradual mapping disabled",
            "The project remains normally available. No brownfield discovery will start without new authorization.",
        ),
        BrownfieldOnboardingState::Enabled => (
            "Gradual mapping active",
            "Consent can be revoked at any time. Revocation does not remove already authorized files.",
        ),
    };
    let inventory_summary = onboarding.inventory.as_ref().map(|inventory| {
        let languages = inventory
            .languages
            .iter()
            .map(|item| format!("{} ({})", item.language, item.file_count))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} sources · {} manifests · {} entry points · languages: {}{}",
            inventory.source_files,
            inventory.manifests.len(),
            inventory.entry_points.len(),
            if languages.is_empty() {
                "unidentified"
            } else {
                &languages
            },
            if inventory.coverage.truncated {
                " · partial coverage"
            } else {
                ""
            }
        )
    });
    let consent_pending = onboarding.state == BrownfieldOnboardingState::Pending;
    let active_source = active_path.filter(|path| is_brownfield_source_path(path));
    let active_is_ignored = active_source
        .as_ref()
        .is_some_and(|path| onboarding.ignored_paths.binary_search(path).is_ok());

    div()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                .when(!consent_pending, |row| {
                    row.child(crate::widgets::info_tip(
                        "brownfield-onboarding",
                        description,
                    ))
                }),
        )
        // What the user is consenting to stays readable before they answer.
        .when(consent_pending, |card| {
            card.child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child(description),
            )
        })
        .when_some(inventory_summary, |card, summary| {
            card.child(div().text_sm().child(summary))
        })
        .when_some(
            (onboarding.state == BrownfieldOnboardingState::Enabled)
                .then_some(active_source)
                .flatten(),
            |card, path| {
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme::text_muted())
                                .child(format!("Selected source: {path}")),
                        )
                        .child(brownfield_ignore_button(
                            active_is_ignored,
                            onboarding.busy,
                            cx,
                        )),
                )
            },
        )
        .child(
            div()
                .id("onboarding-feedback")
                .h(px(40.0))
                .flex_shrink_0()
                .overflow_y_scroll()
                .text_sm()
                .text_color(theme::danger())
                .child(onboarding.error.unwrap_or_default()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .when(
                    onboarding.state == BrownfieldOnboardingState::Pending,
                    |actions| {
                        actions
                            .child(brownfield_action_button(
                                "brownfield-enable",
                                if onboarding.busy {
                                    "Mapping…"
                                } else {
                                    "Start gradual mapping"
                                },
                                true,
                                onboarding.busy,
                                cx,
                            ))
                            .child(brownfield_action_button(
                                "brownfield-decline",
                                "Use as fresh project (no mapping)",
                                false,
                                onboarding.busy,
                                cx,
                            ))
                    },
                )
                .when(
                    onboarding.state == BrownfieldOnboardingState::Disabled,
                    |actions| {
                        actions.child(brownfield_action_button(
                            "brownfield-reenable",
                            if onboarding.busy {
                                "Mapping…"
                            } else {
                                "Enable gradual mapping"
                            },
                            true,
                            onboarding.busy,
                            cx,
                        ))
                    },
                )
                .when(
                    onboarding.state == BrownfieldOnboardingState::Enabled,
                    |actions| {
                        actions
                            .child(brownfield_action_button(
                                "brownfield-refresh",
                                if onboarding.busy {
                                    "Refreshing…"
                                } else {
                                    "Refresh inventory"
                                },
                                true,
                                onboarding.busy,
                                cx,
                            ))
                            .child(brownfield_revoke_button(
                                confirm_revoke,
                                onboarding.busy,
                                cx,
                            ))
                    },
                ),
        )
}

fn is_brownfield_source_path(path: &str) -> bool {
    if path.replace('\\', "/").starts_with("knowledge/") {
        return false;
    }
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "rs" | "ts"
                    | "tsx"
                    | "js"
                    | "jsx"
                    | "py"
                    | "go"
                    | "java"
                    | "kt"
                    | "kts"
                    | "cs"
                    | "c"
                    | "h"
                    | "cc"
                    | "cpp"
                    | "hpp"
                    | "rb"
                    | "php"
                    | "swift"
                    | "sql"
                    | "sh"
                    | "ps1"
            )
        })
}

fn brownfield_ignore_button(
    ignored: bool,
    disabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    crate::widgets::button_frame(
        div().id("brownfield-ignore-source"),
        crate::widgets::ButtonVariant::Secondary,
        !disabled,
    )
    .when(!disabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| {
            model.set_active_brownfield_ignored(!ignored, cx);
        }))
    })
    .child(if ignored {
        "Map it again"
    } else {
        "Ignore in map"
    })
    .into_any_element()
}

fn brownfield_action_button(
    id: &'static str,
    label: &'static str,
    consent: bool,
    disabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    crate::widgets::button_frame(
        div().id(id),
        if consent {
            crate::widgets::ButtonVariant::Primary
        } else {
            crate::widgets::ButtonVariant::Secondary
        },
        !disabled,
    )
    .when(!disabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| {
            model.set_confirm_brownfield_revoke(false, cx);
            model.set_brownfield_mapping_enabled(consent, cx);
        }))
    })
    .child(label)
    .into_any_element()
}

fn brownfield_revoke_button(
    confirming: bool,
    disabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    crate::widgets::button_frame(
        div().id("brownfield-revoke").tab_index(0),
        crate::widgets::ButtonVariant::Danger,
        !disabled,
    )
    .when(!disabled && confirming, |button| {
        button
            .bg(theme::danger())
            .border_color(theme::danger())
            .text_color(theme::canvas())
    })
    .when(!disabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| {
            if confirming {
                model.set_brownfield_mapping_enabled(false, cx);
            } else {
                model.set_confirm_brownfield_revoke(true, cx);
            }
        }))
    })
    .child(if confirming {
        "Confirm revocation"
    } else {
        "Revoke consent"
    })
    .into_any_element()
}

fn workspace_tree_panel(
    rows: Vec<TreeRow>,
    capped: bool,
    active_path: Option<&str>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id("workspace-tree-scroll")
        .size_full()
        .min_w_0()
        .min_h_0()
        .p_2()
        .rounded_lg()
        .overflow_y_scroll()
        .child(
            div()
                .px_2()
                .pb_2()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::text_subtle())
                .child("FILES"),
        )
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            let path = row.relative_path.clone();
            let is_active = !row.is_dir && active_path == Some(path.as_str());
            div()
                .id(SharedString::from(format!("workspace-tree-{index}")))
                .debug_selector({
                    let path = path.clone();
                    move || format!("workspace-tree:{path}")
                })
                .tooltip(crate::widgets::text_tooltip(path.clone()))
                .pl(px(4.0 + row.depth as f32 * 14.0))
                .pr_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .text_sm()
                .cursor_pointer()
                .text_color(if is_active {
                    theme::accent()
                } else if row.is_dir {
                    theme::text_muted()
                } else {
                    theme::text()
                })
                .when(is_active, |item| {
                    item.bg(theme::accent_soft())
                        .font_weight(gpui::FontWeight::MEDIUM)
                })
                .when(!is_active, |item| {
                    item.hover(|style| style.bg(theme::surface_raised()))
                })
                .on_click(cx.listener(move |model, _, _, cx| {
                    if row.is_dir {
                        model.toggle_workspace_folder(&path, cx);
                    } else {
                        model.open_workspace_file(path.clone(), cx);
                    }
                }))
                .child(crate::widgets::tree_row_label(
                    row.is_dir.then_some(row.expanded),
                    row.name,
                    if is_active {
                        theme::accent()
                    } else {
                        theme::text_muted()
                    },
                ))
        }))
        .when(capped, |panel| {
            panel.child(crate::widgets::tree_capped_notice())
        })
}

// Every piece is a plain value handed down from the panel's single read of the model.
#[allow(clippy::too_many_arguments)]
fn workspace_editor_surface(
    tabs: Vec<TabSummary>,
    active_path: Option<String>,
    active_tab: Option<TabSummary>,
    preview: Option<Vec<MarkdownBlock>>,
    preview_mode: bool,
    editor: gpui::Entity<crate::NativeEditor>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let has_file = active_tab.is_some();
    div()
        .size_full()
        .min_h_0()
        .min_w_0()
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .id("workspace-file-tabs")
                .h(px(38.0))
                .flex_shrink_0()
                .overflow_x_scroll()
                .flex()
                .items_end()
                .gap_1()
                .px_2()
                .pt_2()
                .border_b_1()
                .border_color(theme::border())
                .children(tabs.into_iter().enumerate().map(|(index, tab)| {
                    let path = tab.relative_path.to_string();
                    let close_path = path.clone();
                    let close_id = SharedString::from(format!("workspace-tab-close-{index}"));
                    let selected = active_path.as_deref() == Some(path.as_str());
                    div()
                        .id(SharedString::from(format!("workspace-tab-{index}")))
                        .h(px(30.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .rounded_t_md()
                        .cursor_pointer()
                        .when(selected, |item| item.bg(theme::surface_raised()))
                        .when(!selected, |item| item.text_color(theme::text_muted()))
                        .on_click(cx.listener(move |model, _, _, cx| {
                            model.select_workspace_tab(&path, cx);
                        }))
                        .child(tab.file_name.clone())
                        .when(tab.saving, |item| {
                            item.child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("saving"),
                            )
                        })
                        .when(tab.dirty && !tab.saving, |item| {
                            item.child(div().text_xs().text_color(theme::warning()).child("●"))
                        })
                        .when(tab.conflict.is_some(), |item| {
                            item.child(div().text_xs().text_color(theme::danger()).child("!"))
                        })
                        .child(
                            div()
                                .id(close_id)
                                .px_1()
                                .rounded_sm()
                                .hover(|style| style.bg(theme::danger_soft()))
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    cx.stop_propagation();
                                    model.close_workspace_tab(&close_path, cx);
                                }))
                                .child("×"),
                        )
                })),
        )
        .when(active_tab.is_none(), |surface| {
            surface.child(crate::widgets::empty_panel(
                crate::icons::Icon::FileText,
                "No file open",
                "Open a Markdown or text file in the tree to get started.",
            ))
        })
        .when(has_file, |surface| {
            surface.child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .px_3()
                    .pt_2()
                    .child(
                        crate::widgets::segment_track(
                            div()
                                .id("workspace-view-mode")
                                .debug_selector(|| "workspace-view-mode".into()),
                        )
                        .child(
                            crate::widgets::segment(
                                div()
                                    .id("workspace-mode-edit")
                                    .debug_selector(|| "workspace-mode-edit".into()),
                                !preview_mode,
                            )
                            .on_click(
                                cx.listener(|model, _, _, cx| {
                                    model.set_workspace_preview(false, cx)
                                }),
                            )
                            .child("Edit"),
                        )
                        .child(
                            crate::widgets::segment(
                                div()
                                    .id("workspace-mode-preview")
                                    .debug_selector(|| "workspace-mode-preview".into()),
                                preview_mode,
                            )
                            .on_click(
                                cx.listener(|model, _, _, cx| {
                                    model.set_workspace_preview(true, cx)
                                }),
                            )
                            .child("Preview"),
                        ),
                    ),
            )
        })
        .when(has_file && !preview_mode, |surface| {
            surface.child(
                div()
                    .id("workspace-editor-viewport")
                    .debug_selector(|| "workspace-editor-viewport".into())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    .p_3()
                    .child(editor),
            )
        })
        .when(has_file && preview_mode, |surface| {
            surface.child(
                div()
                    .id("workspace-markdown-preview")
                    .debug_selector(|| "workspace-markdown-preview".into())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .py_4()
                    .child(
                        // Prose reads best at a bounded measure on wide windows.
                        div().max_w(px(820.0)).children(
                            preview
                                .unwrap_or_default()
                                .into_iter()
                                .map(render_markdown_block),
                        ),
                    ),
            )
        })
}

pub(crate) fn render_markdown_block(block: MarkdownBlock) -> AnyElement {
    render_markdown_block_with(block, &mut |text| text.into_any_element())
}

pub(crate) fn render_markdown_block_with(
    block: MarkdownBlock,
    text_element: &mut impl FnMut(String) -> AnyElement,
) -> AnyElement {
    match block {
        MarkdownBlock::Table { headers, rows } => div()
            .mb_3()
            .min_w_0()
            .border_1()
            .border_color(theme::border())
            .children(
                std::iter::once((true, headers))
                    .chain(rows.into_iter().map(|row| (false, row)))
                    .map(|(header, row)| {
                        div()
                            .flex()
                            .min_w_0()
                            .when(header, |row| {
                                row.bg(theme::surface_raised())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                            })
                            .children(row.into_iter().map(|text| {
                                div()
                                    .flex_1()
                                    .min_w(px(100.0))
                                    .p_2()
                                    .text_sm()
                                    .border_b_1()
                                    .border_r_1()
                                    .border_color(theme::border())
                                    .child(text_element(text))
                            }))
                    }),
            )
            .into_any_element(),
        MarkdownBlock::Heading { level, text } => div()
            .mt_2()
            .mb_2()
            .text_size(px(22.0 - f32::from(level) * 1.5))
            .font_weight(gpui::FontWeight::BOLD)
            .child(text_element(text))
            .into_any_element(),
        MarkdownBlock::Paragraph(text) => div()
            .mb_3()
            .text_sm()
            .line_height(px(22.0))
            .child(text_element(text))
            .into_any_element(),
        MarkdownBlock::CodeBlock { language, code } => div()
            .mb_3()
            .p_3()
            .rounded_md()
            .bg(theme::canvas())
            .font_family(theme::FONT_MONO)
            .text_sm()
            .child(text_element(format!("{}\n{code}", language.unwrap_or_default())))
            .into_any_element(),
        MarkdownBlock::Quote(text) => div()
            .mb_2()
            .pl_3()
            .border_l_2()
            .border_color(theme::accent())
            .text_color(theme::text_muted())
            .child(text_element(text))
            .into_any_element(),
        MarkdownBlock::ListItem(text) => div()
            .mb_1()
            .text_sm()
            .child(text_element(format!("• {text}")))
            .into_any_element(),
        MarkdownBlock::Rule => div()
            .my_3()
            .h(px(1.0))
            .bg(theme::border())
            .into_any_element(),
    }
}

fn context_drawer(
    active_tab: Option<&TabSummary>,
    documents: Vec<(String, String)>,
) -> impl IntoElement {
    let (path, revision, state) = active_tab.map_or_else(
        || ("No file".into(), "—".into(), "idle".into()),
        |tab| {
            (
                tab.relative_path.to_string(),
                tab.revision.clone(),
                if tab.conflict.is_some() {
                    "conflict"
                } else if tab.saving {
                    "saving"
                } else if tab.dirty {
                    "modified"
                } else {
                    "in sync"
                }
                .into(),
            )
        },
    );
    let total_documents = documents.len();
    let display_limit = 4;
    let overflow_count = total_documents.saturating_sub(display_limit);

    div()
        .id("workspace-context-scroll")
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_y_scroll()
        .p_3()
        .rounded_lg()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Context"),
        )
        .child(context_item("File", path))
        .child(context_item("Revision", revision))
        .child(context_item("State", state))
        .when(documents.is_empty(), |drawer| {
            drawer.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Related context loading…"),
            )
        })
        .children(
            documents
                .into_iter()
                .take(display_limit)
                .map(|(path, content)| {
                    let excerpt = content
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("Document without content")
                        .chars()
                        .take(90)
                        .collect::<String>();
                    div()
                        .p_2()
                        .rounded_md()
                        .bg(theme::surface_raised())
                        .child(div().text_xs().text_color(theme::accent()).child(path))
                        .child(
                            div()
                                .mt_1()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(excerpt),
                        )
                }),
        )
        .when(overflow_count > 0, |drawer| {
            drawer.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme::surface_raised())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(format!("… and {overflow_count} more doc(s)"))
                    .child(
                        div()
                            .text_color(theme::accent())
                            .child(format!("Total: {total_documents}")),
                    ),
            )
        })
}

fn context_item(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().text_sm().child(value))
}
