use std::path::Path;

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::workspace_service::{FileNode, FileNodeKind};
use hvc_editor::{MarkdownBlock, parse_markdown};

use crate::app_model::AppModel;
use crate::theme;
use crate::workspace::{BrownfieldOnboardingModel, BrownfieldOnboardingState, WorkspaceTabModel};

#[derive(Clone)]
struct WorkspaceTreeRow {
    depth: usize,
    node: FileNode,
}

pub(crate) fn workspace_content_panel(
    model: &AppModel,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let (root_path, tree, tabs, active_path, editor, context_drawer_open, brownfield) = {
        let workspace = model.workspace.read(cx);
        (
            workspace.root_path.clone(),
            workspace.tree.clone(),
            workspace.tabs.clone(),
            workspace
                .active_relative_path
                .as_ref()
                .map(ToString::to_string),
            workspace.editor.clone(),
            workspace.context_drawer_open,
            workspace.brownfield.clone(),
        )
    };
    let active_tab = active_path
        .as_deref()
        .and_then(|path| tabs.iter().find(|tab| tab.relative_path.as_ref() == path))
        .cloned();
    let mut tree_rows = Vec::new();
    flatten_workspace_tree(&tree, 0, &mut tree_rows);
    let root_label = root_path
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "Nenhum workspace selecionado".into());
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

    div()
        .flex_1()
        .min_w_0()
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
                        div()
                            .id("choose-workspace")
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(theme::accent())
                            .text_color(theme::canvas())
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.88))
                            .on_click(cx.listener(|model, _, window, cx| {
                                model.choose_workspace(window, cx);
                            }))
                            .child("Abrir workspace"),
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
                                .child(format!("Conflito de revisão: {conflict}")),
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
                                .child("Recarregar"),
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
                                    "Confirmar sobrescrita"
                                } else {
                                    "Sobrescrever"
                                }),
                        ),
                )
            },
        )
        .when_some(
            active_tab.as_ref().and_then(|tab| tab.save_error.clone()),
            |panel, error| {
                panel.child(
                    div()
                        .p_2()
                        .rounded_sm()
                        .bg(theme::danger_soft())
                        .text_color(theme::danger())
                        .child(error),
                )
            },
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .gap_3()
                .child(workspace_tree_panel(tree_rows, active_path.as_deref(), cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::surface())
                        .overflow_hidden()
                        .child(workspace_editor_surface(
                            tabs.clone(),
                            active_path.clone(),
                            active_tab.clone(),
                            editor,
                            cx,
                        )),
                )
                .when(context_drawer_open, |layout| {
                    layout.child(context_drawer(active_tab.as_ref(), context_documents))
                }),
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
            "Workspace existente detectado",
            "O mapeamento gradual enumera apenas metadados seguros e respeita o .gitignore. Nenhum script, build ou chamada de LLM será executado.",
        ),
        BrownfieldOnboardingState::Disabled => (
            "Mapeamento gradual desativado",
            "O projeto continua disponível normalmente. Nenhuma descoberta brownfield será iniciada sem uma nova autorização.",
        ),
        BrownfieldOnboardingState::Enabled => (
            "Mapeamento gradual ativo",
            "O consentimento pode ser revogado a qualquer momento. A revogação não remove arquivos já autorizados.",
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
            "{} fontes · {} manifests · {} pontos de entrada · idiomas: {}{}",
            inventory.source_files,
            inventory.manifests.len(),
            inventory.entry_points.len(),
            if languages.is_empty() {
                "não identificados"
            } else {
                &languages
            },
            if inventory.coverage.truncated {
                " · cobertura parcial"
            } else {
                ""
            }
        )
    });
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
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(description),
        )
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
                                .child(format!("Fonte selecionada: {path}")),
                        )
                        .child(brownfield_ignore_button(
                            active_is_ignored,
                            onboarding.busy,
                            cx,
                        )),
                )
            },
        )
        .when_some(onboarding.error, |card, error| {
            card.child(div().text_sm().text_color(theme::danger()).child(error))
        })
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
                                    "Mapeando…"
                                } else {
                                    "Iniciar mapeamento gradual"
                                },
                                true,
                                onboarding.busy,
                                cx,
                            ))
                            .child(brownfield_action_button(
                                "brownfield-decline",
                                "Usar como projeto novo (sem mapeamento)",
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
                                "Mapeando…"
                            } else {
                                "Ativar mapeamento gradual"
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
                                    "Atualizando…"
                                } else {
                                    "Atualizar inventário"
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
    div()
        .id("brownfield-ignore-source")
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::surface())
        .text_color(theme::text())
        .opacity(if disabled { 0.55 } else { 1.0 })
        .when(!disabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(0.88))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.set_active_brownfield_ignored(!ignored, cx);
                }))
        })
        .child(if ignored {
            "Reativar no mapa"
        } else {
            "Ignorar no mapa"
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
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if consent {
            theme::accent()
        } else {
            theme::surface()
        })
        .text_color(if consent {
            theme::canvas()
        } else {
            theme::text()
        })
        .opacity(if disabled { 0.55 } else { 1.0 })
        .when(!disabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(0.88))
                .on_click(cx.listener(move |model, _, _, cx| {
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
    div()
        .id("brownfield-revoke")
        .px_3()
        .py_2()
        .rounded_md()
        .tab_index(0)
        .bg(if confirming {
            theme::danger()
        } else {
            theme::surface()
        })
        .text_color(if confirming {
            theme::canvas()
        } else {
            theme::text()
        })
        .opacity(if disabled { 0.55 } else { 1.0 })
        .when(!disabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
                .on_click(cx.listener(move |model, _, _, cx| {
                    if confirming {
                        model.set_brownfield_mapping_enabled(false, cx);
                    } else {
                        model.set_confirm_brownfield_revoke(true, cx);
                    }
                }))
        })
        .child(if confirming {
            "Confirmar revogação"
        } else {
            "Revogar consentimento"
        })
        .into_any_element()
}

fn flatten_workspace_tree(nodes: &[FileNode], depth: usize, rows: &mut Vec<WorkspaceTreeRow>) {
    for node in nodes {
        rows.push(WorkspaceTreeRow {
            depth,
            node: node.clone(),
        });
        flatten_workspace_tree(&node.children, depth.saturating_add(1), rows);
    }
}

fn workspace_tree_panel(
    rows: Vec<WorkspaceTreeRow>,
    active_path: Option<&str>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id("workspace-tree-scroll")
        .w(px(220.0))
        .min_h_0()
        .p_2()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .overflow_y_scroll()
        .child(
            div()
                .px_2()
                .pb_2()
                .text_xs()
                .text_color(theme::text_muted())
                .child("ARQUIVOS"),
        )
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            let is_file = matches!(row.node.kind, FileNodeKind::File);
            let path = row.node.relative_path.clone();
            let is_active = is_file && active_path == Some(path.as_str());
            div()
                .id(SharedString::from(format!("workspace-tree-{index}")))
                .pl(px(8.0 + row.depth as f32 * 14.0))
                .pr_2()
                .py_1()
                .rounded_sm()
                .text_sm()
                .text_color(if is_active {
                    theme::accent()
                } else if is_file {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                .when(is_active, |item| {
                    item.bg(theme::accent_soft())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .border_l_2()
                        .border_color(theme::accent())
                })
                .when(is_file, |item| {
                    item.cursor_pointer()
                        .hover(|style| style.bg(theme::surface_raised()))
                        .on_click(cx.listener(move |model, _, _, cx| {
                            model.open_workspace_file(path.clone(), cx);
                        }))
                })
                .child(format!(
                    "{} {}",
                    if is_active {
                        "▶"
                    } else if is_file {
                        "·"
                    } else {
                        "▾"
                    },
                    row.node.name
                ))
        }))
}

fn workspace_editor_surface(
    tabs: Vec<WorkspaceTabModel>,
    active_path: Option<String>,
    active_tab: Option<WorkspaceTabModel>,
    editor: gpui::Entity<crate::NativeEditor>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let preview = active_tab
        .as_ref()
        .map(|tab| parse_markdown(&tab.content))
        .unwrap_or_default();
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .min_h(px(38.0))
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
                        .child(tab.file_name().to_owned())
                        .when(tab.saving, |item| {
                            item.child(
                                div()
                                    .text_xs()
                                    .text_color(theme::accent())
                                    .child("salvando"),
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
            surface.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme::text_muted())
                    .child("Abra um arquivo Markdown ou texto na árvore para começar."),
            )
        })
        .when(active_tab.is_some(), |surface| {
            surface.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .p_3()
                            .border_r_1()
                            .border_color(theme::border())
                            .child(editor),
                    )
                    .child(
                        div()
                            .id("workspace-markdown-preview")
                            .w(px(360.0))
                            .min_h_0()
                            .p_4()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .pb_3()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Markdown Preview"),
                            )
                            .children(preview.into_iter().map(render_markdown_block)),
                    ),
            )
        })
}

pub(crate) fn render_markdown_block(block: MarkdownBlock) -> AnyElement {
    match block {
        MarkdownBlock::Heading { level, text } => div()
            .mt_2()
            .mb_2()
            .text_size(px(22.0 - f32::from(level) * 1.5))
            .font_weight(gpui::FontWeight::BOLD)
            .child(text)
            .into_any_element(),
        MarkdownBlock::Paragraph(text) => div()
            .mb_3()
            .text_sm()
            .line_height(px(22.0))
            .child(text)
            .into_any_element(),
        MarkdownBlock::CodeBlock { language, code } => div()
            .mb_3()
            .p_3()
            .rounded_md()
            .bg(theme::canvas())
            .font_family(theme::FONT_MONO)
            .text_sm()
            .child(format!("{}\n{code}", language.unwrap_or_default()))
            .into_any_element(),
        MarkdownBlock::Quote(text) => div()
            .mb_2()
            .pl_3()
            .border_l_2()
            .border_color(theme::accent())
            .text_color(theme::text_muted())
            .child(text)
            .into_any_element(),
        MarkdownBlock::ListItem(text) => div()
            .mb_1()
            .text_sm()
            .child(format!("• {text}"))
            .into_any_element(),
        MarkdownBlock::Rule => div()
            .my_3()
            .h(px(1.0))
            .bg(theme::border())
            .into_any_element(),
    }
}

fn context_drawer(
    active_tab: Option<&WorkspaceTabModel>,
    documents: Vec<(String, String)>,
) -> impl IntoElement {
    let (path, revision, state) = active_tab.map_or_else(
        || ("Nenhum arquivo".into(), "—".into(), "ocioso".into()),
        |tab| {
            (
                tab.relative_path.to_string(),
                tab.revision.clone(),
                if tab.conflict.is_some() {
                    "conflito"
                } else if tab.saving {
                    "salvando"
                } else if tab.dirty {
                    "alterado"
                } else {
                    "sincronizado"
                }
                .into(),
            )
        },
    );
    let total_documents = documents.len();
    let display_limit = 4;
    let overflow_count = total_documents.saturating_sub(display_limit);

    div()
        .w(px(210.0))
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
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Contexto"),
        )
        .child(context_item("Arquivo", path))
        .child(context_item("Revisão", revision))
        .child(context_item("Estado", state))
        .when(documents.is_empty(), |drawer| {
            drawer.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Contexto relacionado sendo carregado…"),
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
                        .unwrap_or("Documento sem conteúdo")
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
                    .child(format!("… e mais {overflow_count} doc(s)"))
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
