use gpui::{AnyElement, Context, IntoElement, PathBuilder, canvas, div, point, prelude::*, px};
use hvc_core::workspace_service::{SearchMatch, SearchMode};
use hvc_editor::parse_markdown;

use crate::app_model::{AppModel, AppView};
use crate::sizing::StyledExt;
use crate::theme;
use crate::vault::VaultGraphLayout;
use crate::workspace_view::render_markdown_block;

pub(crate) fn vault_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let search_input = model.vault_search_input.clone();
    let rename_input = model.vault_rename_input.clone();
    let (
        root,
        (tree_rows, tree_capped),
        index,
        selected_path,
        (selected_document, preview_loading),
        search_mode,
        case_sensitive,
        search_results,
        searching,
        indexing,
        renaming,
        graph_expanded,
        _notice,
        error,
    ) = {
        let vault = model.vault.read(cx);
        (
            vault
                .workspace_root
                .as_ref()
                .map(|root| crate::widgets::display_path(root)),
            (
                crate::file_tree::flatten(&vault.tree, &vault.folders),
                crate::file_tree::is_capped(&vault.tree),
            ),
            vault.index.clone(),
            vault.selected_path.as_ref().map(ToString::to_string),
            (
                vault.selected_document().map(str::to_owned),
                vault
                    .selected_path
                    .as_ref()
                    .is_some_and(|path| vault.loading_documents.contains(path.as_ref())),
            ),
            vault.search_mode,
            vault.search_case_sensitive,
            vault.search_response.as_ref().map(|response| {
                (
                    response.matches.clone(),
                    response.scanned_files,
                    response.truncated,
                )
            }),
            vault.searching,
            vault.indexing,
            vault.renaming,
            vault.graph_expanded,
            vault.notice.clone(),
            vault.error.clone(),
        )
    };
    let Some(root) = root else {
        return empty_panel(
            "No Vault Open",
            "Open a workspace to explore files, frontmatter, and wikilinks.",
        )
        .into_any_element();
    };
    let selected_note = selected_path.as_ref().and_then(|selected| {
        index
            .as_ref()
            .and_then(|index| {
                index
                    .notes
                    .iter()
                    .find(|note| &note.relative_path == selected)
            })
            .cloned()
    });
    let graph = {
        let vault = model.vault.read(cx);
        vault.graph_layout(vault.selected_path.as_ref().map(AsRef::as_ref))
    };
    let note_count = index.as_ref().map_or(0, |index| index.notes.len());
    let edge_count = index.as_ref().map_or(0, |index| index.edges.len());
    let broken_links = index
        .as_ref()
        .map(|index| index.broken_links.clone())
        .unwrap_or_default();
    let broken_file_references = index
        .as_ref()
        .map(|index| index.broken_file_references.clone())
        .unwrap_or_default();

    let files_column = div()
        .size_full()
        .min_w_0()
        .min_h_0()
        .p_3()
        .rounded_lg()
        .flex()
        .flex_col()
        .gap_3()
        .child(section_heading(
            if indexing { "INDEXING" } else { "WORKSPACE" },
            "Files",
            note_count,
        ))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .overflow_hidden()
                .child(root),
        )
        .child(crate::sizing::sized_input(
            search_input,
            crate::sizing::INPUT_MEDIUM,
        ))
        .child(
            crate::widgets::segment_track(div())
                .self_start()
                .child(search_mode_button(
                    "vault-substring",
                    "Substring",
                    matches!(search_mode, SearchMode::Substring),
                    SearchMode::Substring,
                    cx,
                ))
                .child(search_mode_button(
                    "vault-regex",
                    "Regex",
                    matches!(search_mode, SearchMode::Regex),
                    SearchMode::Regex,
                    cx,
                )),
        )
        .child(
            div()
                .id("vault-case-sensitive")
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .bg(if case_sensitive {
                    theme::accent_soft()
                } else {
                    theme::surface_raised()
                })
                .text_color(if case_sensitive {
                    theme::accent()
                } else {
                    theme::text_muted()
                })
                .on_click(cx.listener(|model, _, _, cx| {
                    model.toggle_vault_search_case(cx);
                }))
                .child(if case_sensitive {
                    "✓ Match case"
                } else {
                    "Match case"
                }),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    crate::widgets::button_frame(
                        div().id("vault-search"),
                        crate::widgets::ButtonVariant::Tinted,
                        !searching,
                    )
                    .when(!searching, |button| {
                        button.on_click(cx.listener(|model, _, _, cx| model.search_vault(cx)))
                    })
                    .child(if searching { "Searching…" } else { "Search" }),
                )
                .when(search_results.is_some(), |row| {
                    row.child(
                        crate::widgets::button_frame(
                            div().id("vault-clear-search"),
                            crate::widgets::ButtonVariant::Ghost,
                            true,
                        )
                        .on_click(cx.listener(|model, _, _, cx| {
                            model.clear_vault_search(cx);
                        }))
                        .child("Clear"),
                    )
                }),
        )
        .child(
            div()
                .id("vault-browser-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when_some(search_results.clone(), |browser, results| {
                    browser.child(search_results_panel(results, cx))
                })
                .when(search_results.is_none(), |browser| {
                    browser
                        .children(
                            tree_rows
                                .into_iter()
                                .map(|row| vault_tree_row(row, selected_path.as_deref(), cx)),
                        )
                        .when(tree_capped, |browser| {
                            browser.child(crate::widgets::tree_capped_notice())
                        })
                }),
        )
        .into_any_element();
    let graph_panel = div()
        .id("vault-graph-scroll")
        .debug_selector(|| "vault-graph-scroll".into())
        .overflow_y_scroll()
        .size_full()
        .min_w_0()
        .min_h_0()
        .p_3()
        .rounded_lg()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section_heading(
                    "KNOWLEDGE TOPOLOGY",
                    "Wikilink graph",
                    edge_count,
                ))
                .child(
                    div()
                        .id("vault-expand-graph")
                        .debug_selector(|| "vault-expand-graph".into())
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(theme::surface_raised())
                        .cursor_pointer()
                        .on_click(cx.listener(|model, _, _, cx| {
                            model.toggle_vault_graph(cx);
                        }))
                        .child(if graph_expanded { "Collapse" } else { "Expand" }),
                ),
        )
        .child({
            let camera = model.vault.read(cx).camera.snapshot();
            vault_graph(graph, selected_path.as_deref(), camera, cx)
        })
        .when(broken_links.is_empty(), |panel| {
            panel.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme::accent_soft())
                    .text_color(theme::accent())
                    .child("No broken wikilinks detected."),
            )
        })
        .when(!broken_links.is_empty(), |panel| {
            panel.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(theme::warning_soft())
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::warning())
                            .child(format!("{} broken link(s)", broken_links.len())),
                    )
                    .children(broken_links.into_iter().map(|link| {
                        let source = link.source_path.clone();
                        div()
                            .id(gpui::SharedString::from(format!(
                                "broken-link:{}:{}",
                                source, link.line
                            )))
                            .mt_1()
                            .text_sm()
                            .cursor_pointer()
                            .on_click(cx.listener(move |model, _, _, cx| {
                                model.select_vault_path(source.clone(), cx);
                            }))
                            .child(format!(
                                "{}:{} → [[{}]]",
                                link.source_path, link.line, link.target
                            ))
                    })),
            )
        })
        .when_some(
            crate::doc_drift_view::drift_card(model, cx),
            |panel, card| panel.child(card),
        )
        .when(!broken_file_references.is_empty(), |panel| {
            panel.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(theme::warning_soft())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::warning())
                                    .child(format!(
                                        "File reference issues ({})",
                                        broken_file_references.len()
                                    )),
                            )
                            .child(crate::widgets::info_tip(
                                "broken-file-references",
                                "Checks referenced files, not test execution.",
                            )),
                    )
                    .children(broken_file_references.into_iter().map(|reference| {
                        let source = reference.source_path.clone();
                        div()
                            .id(gpui::SharedString::from(format!(
                                "broken-file:{}:{}:{}",
                                source, reference.field, reference.target
                            )))
                            .mt_1()
                            .text_sm()
                            .cursor_pointer()
                            .on_click(cx.listener(move |model, _, _, cx| {
                                model.select_vault_path(source.clone(), cx);
                            }))
                            .child(format!(
                                "{} · {} → {}",
                                reference.source_path, reference.field, reference.target
                            ))
                            .child(div().text_xs().child(reference.reason))
                    })),
            )
        })
        .into_any_element();

    // Files | (graph | inspector): the same resizable columns Workspace and Forge use. Expanding
    // the graph drops both side columns and their sashes instead of squeezing them.
    // ADR-034 M5: a note's reconciliation takes the graph's column.
    let graph_panel = match crate::doc_drift_view::comparison(model, cx) {
        Some(comparison) => comparison,
        None => graph_panel,
    };
    let columns = if graph_expanded {
        graph_panel
    } else {
        let inspector = inspector(
            selected_path,
            selected_note,
            selected_document,
            preview_loading,
            rename_input,
            renaming,
            _notice,
            error,
            cx,
        )
        .into_any_element();
        let detail = crate::split_pane::split_pane(
            "vault-detail-columns",
            &model.vault_detail_split,
            graph_panel,
            inspector,
            cx,
        );
        crate::split_pane::split_pane(
            "vault-columns",
            &model.vault_files_split,
            files_column,
            detail,
            cx,
        )
    };
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .child(columns)
        .into_any_element()
}

fn vault_tree_row(
    row: crate::file_tree::TreeRow,
    selected_path: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let path = row.relative_path.clone();
    let selected = !row.is_dir && selected_path == Some(path.as_str());
    let is_dir = row.is_dir;
    div()
        .id(gpui::SharedString::from(format!(
            "vault-tree:{}:{path}",
            if is_dir { "Directory" } else { "File" }
        )))
        .tooltip(crate::widgets::text_tooltip(path.clone()))
        .pl(px(4.0 + row.depth as f32 * 12.0))
        .pr_2()
        .py_1()
        .rounded_md()
        .flex()
        .items_center()
        .text_sm()
        .cursor_pointer()
        .when(selected, |item| {
            item.bg(theme::accent_soft()).text_color(theme::accent())
        })
        .when(!selected, |item| {
            item.hover(|style| style.bg(theme::surface_raised()))
        })
        .when(is_dir, |item| item.text_color(theme::text_muted()))
        .on_click(cx.listener(move |model, _, _, cx| {
            if is_dir {
                model.toggle_vault_folder(&path, cx);
            } else {
                model.select_vault_path(path.clone(), cx);
            }
        }))
        .child(crate::widgets::tree_row_label(
            is_dir.then_some(row.expanded),
            row.name,
            if selected {
                theme::accent()
            } else {
                theme::text_muted()
            },
        ))
        .into_any_element()
}

fn search_results_panel(
    results: (Vec<SearchMatch>, usize, bool),
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let (matches, scanned_files, truncated) = results;
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "{} result(s) in {} file(s){}",
                    matches.len(),
                    scanned_files,
                    if truncated { " · truncated" } else { "" }
                )),
        )
        .children(matches.into_iter().enumerate().map(|(position, matched)| {
            let path = matched.relative_path.clone();
            let cursor = hvc_editor::TextPosition {
                line: matched.line.saturating_sub(1),
                column: matched.column.saturating_sub(1),
            };
            div()
                .id(gpui::SharedString::from(format!(
                    "vault-result:{path}:{}:{position}",
                    matched.line
                )))
                .p_2()
                .rounded_md()
                .bg(theme::surface_raised())
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.open_workspace_file_at(path.clone(), Some(cursor), cx);
                }))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(matched.relative_path),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("Line {}, column {}", matched.line, matched.column)),
                )
                .child(div().mt_1().text_sm().child(matched.line_text))
        }))
}

fn search_mode_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    mode: SearchMode,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    crate::widgets::segment(div().id(id), selected)
        .on_click(cx.listener(move |model, _, _, cx| {
            model.set_vault_search_mode(mode, cx);
        }))
        .child(label)
}

/// The wikilink graph as a pannable, zoomable canvas. Until the developer drags or zooms, the
/// camera fits the whole layout (a larger vault is simply shown further out). Dragging the
/// background pans (hand tool); only a click on a node's dot selects it; the wheel zooms around
/// the pointer. Nodes outside the panel are not built at all.
fn vault_graph(
    graph: VaultGraphLayout,
    selected_path: Option<&str>,
    camera: crate::vault::GraphCamera,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    if graph.nodes.is_empty() {
        return div()
            .flex_1()
            .min_h(px(300.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::text_muted())
            .child("Open a Markdown Vault to render the graph.")
            .into_any_element();
    }
    let measured = camera.bounds.clone();
    let panel = measured.get().size;
    // First frame: nothing measured yet; fit to a nominal panel and correct on the next frame.
    let (width, height) = if panel.width > px(0.0) && panel.height > px(0.0) {
        (f32::from(panel.width), f32::from(panel.height))
    } else {
        (600.0, 460.0)
    };
    let transform = camera.transform(&graph, width, height);
    camera.last.set(Some((transform, graph.nodes.len())));
    let dragging = camera.drag.is_some();
    let radius = crate::vault::graph_node_radius(transform.zoom);
    // Labels crowd a far-out view; keep them for the selection only.
    let labels = transform.zoom >= 0.55;
    let hidden = graph.hidden;
    let shown = graph.nodes.len();
    let edges: Vec<((f32, f32), (f32, f32))> = graph
        .edges
        .iter()
        .map(|edge| {
            (
                transform.to_panel(edge.source.x, edge.source.y),
                transform.to_panel(edge.target.x, edge.target.y),
            )
        })
        .collect();
    let visible = |x: f32, y: f32| x > -90.0 && x < width + 90.0 && y > -50.0 && y < height + 50.0;
    // The hovered note's file name, drawn after every node so no neighbouring dot covers it.
    let hover_chip = camera.hovered.as_ref().and_then(|path| {
        let node = graph
            .nodes
            .iter()
            .find(|node| &node.relative_path == path)?;
        let (x, y) = transform.to_panel(node.layout.x, node.layout.y);
        let size = if selected_path == Some(path.as_str()) {
            radius * 2.6
        } else {
            radius * 2.0
        };
        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
        Some((x, y - size / 2.0, name))
    });

    div()
        .id("vault-graph")
        .debug_selector(|| "vault-graph".into())
        .relative()
        .flex_1()
        .min_h(px(360.0))
        .overflow_hidden()
        .rounded_lg()
        .bg(theme::canvas())
        .cursor(if dragging {
            gpui::CursorStyle::ClosedHand
        } else {
            gpui::CursorStyle::OpenHand
        })
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|model, event: &gpui::MouseDownEvent, _, cx| {
                model.vault_graph_press(event.position, cx);
            }),
        )
        .on_mouse_move(cx.listener(|model, event: &gpui::MouseMoveEvent, _, cx| {
            if event.pressed_button == Some(gpui::MouseButton::Left) {
                model.vault_graph_drag(event.position, cx);
            }
        }))
        .on_mouse_up(
            gpui::MouseButton::Left,
            cx.listener(|model, _: &gpui::MouseUpEvent, _, cx| model.vault_graph_release(cx)),
        )
        .on_mouse_up_out(
            gpui::MouseButton::Left,
            cx.listener(|model, _: &gpui::MouseUpEvent, _, cx| model.vault_graph_release(cx)),
        )
        .on_scroll_wheel(cx.listener(|model, event: &gpui::ScrollWheelEvent, window, cx| {
            let delta = f32::from(event.delta.pixel_delta(window.line_height()).y);
            model.vault_graph_zoom((delta * 0.002).exp(), Some(event.position), cx);
            // The panel around the graph scrolls too; the wheel belongs to the graph here.
            cx.stop_propagation();
        }))
        .child(
            canvas(
                move |bounds, _, _| {
                    measured.set(bounds);
                    edges
                },
                |bounds, edges, window, _| {
                    for ((sx, sy), (tx, ty)) in edges {
                        let mut path = PathBuilder::stroke(px(1.0));
                        path.move_to(point(bounds.left() + px(sx), bounds.top() + px(sy)));
                        path.line_to(point(bounds.left() + px(tx), bounds.top() + px(ty)));
                        if let Ok(path) = path.build() {
                            window.paint_path(path, theme::border_strong());
                        }
                    }
                },
            )
            .absolute()
            .size_full(),
        )
        .children(graph.nodes.into_iter().filter_map(|node| {
            let (x, y) = transform.to_panel(node.layout.x, node.layout.y);
            if !visible(x, y) {
                return None;
            }
            let selected = selected_path == Some(node.relative_path.as_str());
            let path = node.relative_path.clone();
            let hover_path = node.relative_path.clone();
            let selector = format!("vault-node:{}", node.relative_path);
            let size = if selected { radius * 2.6 } else { radius * 2.0 };
            let label_width = 120.0;
            let label = if node.title.chars().count() > 22 {
                format!("{}…", node.title.chars().take(22).collect::<String>())
            } else {
                node.title
            };
            Some(
                div()
                    .absolute()
                    .left(px(x - label_width / 2.0))
                    .top(px(y - size / 2.0))
                    .w(px(label_width))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        // Only the dot is the click target: the label never steals a pan.
                        div()
                            .id(gpui::SharedString::from(format!("vault-node:{path}")))
                            .debug_selector(move || selector.clone())
                            .size(px(size))
                            .rounded_full()
                            .border_1()
                            .border_color(if selected {
                                theme::accent()
                            } else {
                                theme::border_strong()
                            })
                            .bg(if selected {
                                theme::accent_soft()
                            } else {
                                theme::surface_raised()
                            })
                            .cursor_pointer()
                            .hover(|style| style.border_color(theme::accent()))
                            .on_hover(cx.listener(move |model, hovered: &bool, _, cx| {
                                model.vault_graph_hover(hover_path.clone(), *hovered, cx);
                            }))
                            .on_click(cx.listener(move |model, _, _, cx| {
                                model.select_vault_path(path.clone(), cx);
                            })),
                    )
                    .when(labels || selected, |node_view| {
                        node_view.child(
                            div()
                                .text_xs()
                                .text_center()
                                .text_color(if selected {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                })
                                .child(label),
                        )
                    }),
            )
        }))
        .when_some(hover_chip, |graph_view, (x, dot_top, name)| {
            graph_view.child(
                div()
                    .absolute()
                    .left(px(x - 160.0))
                    .top(px(dot_top - 30.0))
                    .w(px(320.0))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .debug_selector(|| "vault-node-hover".into())
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::surface_raised())
                            .shadow_md()
                            .text_xs()
                            .text_color(theme::text())
                            .whitespace_nowrap()
                            .child(name),
                    ),
            )
        })
        .child(
            div()
                .absolute()
                .top_2()
                .right_2()
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .rounded_lg()
                .bg(theme::surface())
                .border_1()
                .border_color(theme::border())
                .child(crate::widgets::info_tip(
                    "vault-graph",
                    "Drag the background to pan and scroll to zoom around the pointer. Hover a dot to see its file name; click it to open the note.",
                ))
                .child(crate::widgets::icon_button(
                    "vault-graph-zoom-in",
                    crate::icons::Icon::Plus,
                    "Zoom in",
                    false,
                    None,
                    true,
                    cx,
                    |model, _, cx| model.vault_graph_zoom(1.25, None, cx),
                ))
                .child(crate::widgets::icon_button(
                    "vault-graph-zoom-out",
                    crate::icons::Icon::Minus,
                    "Zoom out",
                    false,
                    None,
                    true,
                    cx,
                    |model, _, cx| model.vault_graph_zoom(0.8, None, cx),
                ))
                .child(crate::widgets::icon_button(
                    "vault-graph-fit",
                    crate::icons::Icon::Maximize,
                    "Fit graph",
                    false,
                    None,
                    true,
                    cx,
                    |model, _, cx| model.vault_graph_fit(cx),
                )),
        )
        // How to use the canvas lives in the toolbar's "i"; only the cap is worth a line.
        .when(hidden > 0, |graph_view| {
            graph_view.child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_3()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .child(format!(
                        "Showing the {shown} most connected of {} notes",
                        shown + hidden
                    )),
            )
        })
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn inspector(
    selected_path: Option<String>,
    note: Option<hvc_core::workspace_service::VaultNoteSummary>,
    document: Option<String>,
    preview_loading: bool,
    rename_input: gpui::Entity<crate::text_input::NativeTextInput>,
    renaming: bool,
    _notice: Option<String>,
    _error: Option<String>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let title = selected_path
        .as_deref()
        .and_then(|path| path.rsplit('/').next())
        .unwrap_or("Inspector")
        .to_owned();
    let open_path = selected_path.clone();
    let has_document = document.is_some();
    div()
        .id("vault-inspector-scroll")
        .size_full()
        .min_w_0()
        .min_h_0()
        .p_3()
        .rounded_lg()
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("SELECTED NOTE"),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(title),
                        ),
                )
                .when_some(open_path, |header, path| {
                    header.child(
                        div()
                            .id("open-vault-note")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(theme::surface_raised())
                            .cursor_pointer()
                            .on_click(cx.listener(move |model, _, _, cx| {
                                model.open_workspace_file(path.clone(), cx);
                                model.select_view(AppView::Workspace, cx);
                            }))
                            .child("Open"),
                    )
                }),
        )
        .when(selected_path.is_none(), |panel| {
            panel.child(
                div()
                    .mt_4()
                    .text_color(theme::text_muted())
                    .child("Select a node or file to inspect it."),
            )
        })
        .when(
            selected_path
                .as_deref()
                .is_some_and(|path| path.to_ascii_lowercase().ends_with(".md")),
            |panel| {
                panel.child(
                    div()
                        .mt_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("RENAME NOTE"),
                        )
                        .child(crate::sizing::sized_input(
                            rename_input,
                            crate::sizing::INPUT_MEDIUM,
                        ))
                        .child(
                            crate::widgets::button_frame(
                                div().id("rename-vault-note"),
                                crate::widgets::ButtonVariant::Tinted,
                                !renaming,
                            )
                            .when(!renaming, |button| {
                                button.on_click(cx.listener(|model, _, _, cx| {
                                    model.rename_selected_vault_note(cx)
                                }))
                            })
                            .child(if renaming {
                                "Renaming…"
                            } else {
                                "Rename"
                            }),
                        ),
                )
            },
        )
        .when_some(note, |panel, note| {
            let frontmatter = note
                .frontmatter
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "No frontmatter properties.".into());
            panel
                .child(
                    div()
                        .mt_4()
                        .grid()
                        .grid_cols(2)
                        .gap_2()
                        .child(stat("Outgoing", note.links.len()))
                        .child(stat("Backlinks", note.backlink_paths.len())),
                )
                .child(
                    div()
                        .mt_3()
                        .p_3()
                        .rounded_md()
                        .bg(theme::surface_raised())
                        .text_sm()
                        .child(frontmatter),
                )
                .when(!note.schema_warnings.is_empty(), |panel| {
                    panel.child(
                        div()
                            .mt_3()
                            .p_3()
                            .rounded_md()
                            .bg(theme::warning_soft())
                            .text_color(theme::warning())
                            .children(
                                note.schema_warnings
                                    .into_iter()
                                    .map(|warning| div().child(format!("• {warning}"))),
                            ),
                    )
                })
        })
        .when_some(document, |panel, document| {
            panel.child(
                div()
                    .id("vault-document-preview")
                    .max_h(px(400.0))
                    .overflow_y_scroll()
                    .mt_4()
                    .p_3()
                    .rounded_md()
                    .bg(theme::canvas())
                    .children(
                        parse_markdown(&document)
                            .into_iter()
                            .map(render_markdown_block),
                    ),
            )
        })
        .when(selected_path.is_some() && !has_document, |panel| {
            // A failed read (too large, not UTF-8) already raised a notice; say so here instead
            // of promising a preview that will never arrive.
            panel.child(
                div()
                    .mt_4()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child(if preview_loading {
                        "Loading preview…"
                    } else {
                        "Preview unavailable for this file."
                    }),
            )
        })
}

fn stat(label: &'static str, value: usize) -> impl IntoElement {
    div()
        .p_2()
        .rounded_md()
        .bg(theme::surface_raised())
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().text_lg().child(value.to_string()))
}

fn section_heading(eyebrow: &'static str, title: &'static str, count: usize) -> impl IntoElement {
    crate::widgets::section_header(eyebrow, title, count)
}

fn empty_panel(title: &'static str, description: &'static str) -> impl IntoElement {
    crate::widgets::empty_panel(crate::icons::Icon::Vault, title, description)
}
