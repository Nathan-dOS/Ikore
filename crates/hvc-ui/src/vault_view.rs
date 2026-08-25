use gpui::{AnyElement, Context, IntoElement, PathBuilder, canvas, div, point, prelude::*, px};
use hvc_core::workspace_service::{FileNode, FileNodeKind, SearchMatch, SearchMode};
use hvc_editor::parse_markdown;

use crate::app_model::{AppModel, AppView};
use crate::shell::render_markdown_block;
use crate::theme;
use crate::vault::{VaultGraphLayout, build_vault_graph_layout};

#[derive(Clone)]
struct VaultTreeRow {
    depth: usize,
    node: FileNode,
}

pub(crate) fn vault_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let search_input = model.vault_search_input.clone();
    let rename_input = model.vault_rename_input.clone();
    let (
        root,
        tree,
        index,
        selected_path,
        selected_document,
        search_mode,
        case_sensitive,
        search_results,
        searching,
        indexing,
        renaming,
        graph_expanded,
        notice,
        error,
    ) = {
        let vault = model.vault.read(cx);
        (
            vault.workspace_root.as_ref().map(ToString::to_string),
            vault.tree.clone(),
            vault.index.clone(),
            vault.selected_path.as_ref().map(ToString::to_string),
            vault.selected_document().map(str::to_owned),
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
            "Nenhum Vault aberto",
            "Abra um workspace para explorar arquivos, frontmatter e wikilinks.",
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
    let graph = build_vault_graph_layout(index.as_ref());
    let mut tree_rows = Vec::new();
    flatten_tree(&tree, 0, &mut tree_rows);
    let note_count = index.as_ref().map_or(0, |index| index.notes.len());
    let edge_count = index.as_ref().map_or(0, |index| index.edges.len());
    let broken_links = index
        .as_ref()
        .map(|index| index.broken_links.clone())
        .unwrap_or_default();

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .gap_3()
        .when(!graph_expanded, |layout| {
            layout.child(
                div()
                    .w(px(264.0))
                    .min_h_0()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::surface())
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section_heading(
                        if indexing { "INDEXANDO" } else { "WORKSPACE" },
                        "Arquivos",
                        note_count,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .overflow_hidden()
                            .child(root),
                    )
                    .child(search_input)
                    .child(
                        div()
                            .flex()
                            .gap_2()
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
                                "✓ Diferenciar maiúsculas"
                            } else {
                                "Diferenciar maiúsculas"
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("vault-search")
                                    .flex_1()
                                    .h(px(34.0))
                                    .rounded_md()
                                    .bg(if searching {
                                        theme::surface_raised()
                                    } else {
                                        theme::accent()
                                    })
                                    .text_color(if searching {
                                        theme::text_muted()
                                    } else {
                                        theme::canvas()
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(!searching, |button| {
                                        button.cursor_pointer().on_click(
                                            cx.listener(|model, _, _, cx| model.search_vault(cx)),
                                        )
                                    })
                                    .child(if searching { "Buscando…" } else { "Buscar" }),
                            )
                            .when(search_results.is_some(), |row| {
                                row.child(
                                    div()
                                        .id("vault-clear-search")
                                        .px_3()
                                        .h(px(34.0))
                                        .rounded_md()
                                        .bg(theme::surface_raised())
                                        .cursor_pointer()
                                        .flex()
                                        .items_center()
                                        .on_click(cx.listener(|model, _, _, cx| {
                                            model.clear_vault_search(cx);
                                        }))
                                        .child("Limpar"),
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
                                browser.children(
                                    tree_rows.into_iter().map(|row| {
                                        vault_tree_row(row, selected_path.as_deref(), cx)
                                    }),
                                )
                            }),
                    ),
            )
        })
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
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
                            "Grafo de wikilinks",
                            edge_count,
                        ))
                        .child(
                            div()
                                .id("vault-expand-graph")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .bg(theme::surface_raised())
                                .cursor_pointer()
                                .on_click(cx.listener(|model, _, _, cx| {
                                    model.toggle_vault_graph(cx);
                                }))
                                .child(if graph_expanded {
                                    "Sair da expansão"
                                } else {
                                    "Expandir"
                                }),
                        ),
                )
                .child(vault_graph(graph, selected_path.as_deref(), cx))
                .when(broken_links.is_empty(), |panel| {
                    panel.child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                            .child("Nenhum wikilink quebrado detectado."),
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
                                    .child(format!("{} link(s) quebrado(s)", broken_links.len())),
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
                }),
        )
        .when(!graph_expanded, |layout| {
            layout.child(inspector(
                selected_path,
                selected_note,
                selected_document,
                rename_input,
                renaming,
                notice,
                error,
                cx,
            ))
        })
        .into_any_element()
}

fn flatten_tree(nodes: &[FileNode], depth: usize, rows: &mut Vec<VaultTreeRow>) {
    for node in nodes {
        rows.push(VaultTreeRow {
            depth,
            node: node.clone(),
        });
        flatten_tree(&node.children, depth.saturating_add(1), rows);
    }
}

fn vault_tree_row(
    row: VaultTreeRow,
    selected_path: Option<&str>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let path = row.node.relative_path.clone();
    let selectable = matches!(row.node.kind, FileNodeKind::File);
    let selected = selected_path == Some(path.as_str());
    div()
        .id(gpui::SharedString::from(format!(
            "vault-tree:{:?}:{path}",
            row.node.kind
        )))
        .ml(px(row.depth as f32 * 12.0))
        .px_2()
        .py_1()
        .rounded_sm()
        .flex()
        .items_center()
        .gap_2()
        .when(selected, |item| {
            item.bg(theme::accent_soft()).text_color(theme::accent())
        })
        .when(selectable, |item| {
            item.cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.select_vault_path(path.clone(), cx);
                }))
        })
        .when(!selectable, |item| item.text_color(theme::text_muted()))
        .child(if selectable { "◇" } else { "▾" })
        .child(row.node.name)
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
                    "{} resultado(s) em {} arquivo(s){}",
                    matches.len(),
                    scanned_files,
                    if truncated { " · truncado" } else { "" }
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
                        .child(format!("Linha {}, coluna {}", matched.line, matched.column)),
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
    div()
        .id(id)
        .flex_1()
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .bg(if selected {
            theme::accent_soft()
        } else {
            theme::surface_raised()
        })
        .text_color(if selected {
            theme::accent()
        } else {
            theme::text_muted()
        })
        .on_click(cx.listener(move |model, _, _, cx| {
            model.set_vault_search_mode(mode, cx);
        }))
        .child(label)
}

fn vault_graph(
    graph: VaultGraphLayout,
    selected_path: Option<&str>,
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
            .child("Abra um Vault Markdown para renderizar o grafo.")
            .into_any_element();
    }
    let edges = graph.edges.clone();
    div()
        .id("vault-graph")
        .flex_1()
        .min_h(px(460.0))
        .overflow_scroll()
        .child(
            div()
                .relative()
                .w(px(760.0))
                .h(px(460.0))
                .child(
                    canvas(
                        move |_, _, _| edges,
                        |bounds, edges, window, _| {
                            for edge in edges {
                                let mut path = PathBuilder::stroke(px(1.5));
                                path.move_to(point(
                                    bounds.left() + px(edge.source.x),
                                    bounds.top() + px(edge.source.y),
                                ));
                                path.line_to(point(
                                    bounds.left() + px(edge.target.x),
                                    bounds.top() + px(edge.target.y),
                                ));
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, theme::border());
                                }
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .children(graph.nodes.into_iter().map(|node| {
                    let selected = selected_path == Some(node.relative_path.as_str());
                    let path = node.relative_path.clone();
                    let label = if node.title.chars().count() > 24 {
                        format!("{}…", node.title.chars().take(24).collect::<String>())
                    } else {
                        node.title
                    };
                    div()
                        .id(gpui::SharedString::from(format!("vault-node:{path}")))
                        .absolute()
                        .left(px(node.layout.x - 50.0))
                        .top(px(node.layout.y - 14.0))
                        .w(px(100.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .on_click(cx.listener(move |model, _, _, cx| {
                            model.select_vault_path(path.clone(), cx);
                        }))
                        .child(
                            div()
                                .size(if selected { px(28.0) } else { px(20.0) })
                                .rounded_full()
                                .border_1()
                                .border_color(if selected {
                                    theme::accent()
                                } else {
                                    theme::border()
                                })
                                .bg(if selected {
                                    theme::accent_soft()
                                } else {
                                    theme::surface_raised()
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if selected {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                })
                                .child(label),
                        )
                })),
        )
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn inspector(
    selected_path: Option<String>,
    note: Option<hvc_core::workspace_service::VaultNoteSummary>,
    document: Option<String>,
    rename_input: gpui::Entity<crate::text_input::NativeTextInput>,
    renaming: bool,
    notice: Option<String>,
    error: Option<String>,
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
        .w(px(300.0))
        .min_h_0()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
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
                                .child("NOTA SELECIONADA"),
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
                            .child("Abrir"),
                    )
                }),
        )
        .when(selected_path.is_none(), |panel| {
            panel.child(
                div()
                    .mt_4()
                    .text_color(theme::text_muted())
                    .child("Selecione um nó ou arquivo para inspecioná-lo."),
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
                                .child("RENOMEAR NOTA"),
                        )
                        .child(rename_input)
                        .child(
                            div()
                                .id("rename-vault-note")
                                .h(px(34.0))
                                .rounded_md()
                                .bg(if renaming {
                                    theme::surface_raised()
                                } else {
                                    theme::accent_soft()
                                })
                                .text_color(if renaming {
                                    theme::text_muted()
                                } else {
                                    theme::accent()
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(!renaming, |button| {
                                    button.cursor_pointer().on_click(cx.listener(
                                        |model, _, _, cx| model.rename_selected_vault_note(cx),
                                    ))
                                })
                                .child(if renaming {
                                    "Renomeando…"
                                } else {
                                    "Renomear"
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
                .unwrap_or_else(|| "Sem propriedades de frontmatter.".into());
            panel
                .child(
                    div()
                        .mt_4()
                        .grid()
                        .grid_cols(2)
                        .gap_2()
                        .child(stat("Saídas", note.links.len()))
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
            panel.child(
                div()
                    .mt_4()
                    .text_color(theme::text_muted())
                    .child("Carregando preview…"),
            )
        })
        .when_some(notice, |panel, notice| {
            panel.child(
                div()
                    .mt_3()
                    .p_2()
                    .rounded_md()
                    .bg(theme::accent_soft())
                    .text_color(theme::accent())
                    .child(notice),
            )
        })
        .when_some(error, |panel, error| {
            panel.child(
                div()
                    .mt_3()
                    .p_2()
                    .rounded_md()
                    .bg(theme::danger_soft())
                    .text_color(theme::danger())
                    .child(error),
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
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(eyebrow),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
        .child(
            div()
                .min_w(px(24.0))
                .h(px(24.0))
                .px_2()
                .rounded_full()
                .bg(theme::surface_raised())
                .text_xs()
                .flex()
                .items_center()
                .justify_center()
                .child(count.to_string()),
        )
}

fn empty_panel(title: &'static str, description: &'static str) -> impl IntoElement {
    div()
        .flex_1()
        .p_5()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(description),
        )
}
