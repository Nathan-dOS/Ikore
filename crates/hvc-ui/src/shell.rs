use std::sync::Arc;

use anyhow::Context as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, IntoElement, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use hvc_core::AppCore;
use hvc_core::persistence_service::HitlDecision;
use hvc_core::workspace_service::{FileNode, FileNodeKind};
use hvc_editor::{DiffMark, MarkdownBlock, build_diff_rows, parse_markdown};

use crate::app_model::{AppModel, AppView};
use crate::hitl::{ApproveHitl, RequestHitlRevision, register_hitl_keybindings};
use crate::native_editor::register_editor_keybindings;
use crate::session::{HeaderNotice, HeaderNoticeKind};
use crate::theme;
use crate::workspace::{WorkspaceTabModel, WorkspaceViewMode};

pub fn open_main_window(core: Arc<AppCore>, cx: &mut App) -> anyhow::Result<()> {
    register_editor_keybindings(cx);
    register_hitl_keybindings(cx);
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Hard Vibe Code".into()),
                ..Default::default()
            }),
            app_id: Some("com.hardvibecode.desktop".into()),
            window_min_size: Some(size(px(960.0), px(640.0))),
            ..Default::default()
        },
        move |_, cx| cx.new(|cx| AppModel::new(core, cx)),
    )
    .context("GPUI recusou a criação da janela principal")?;
    cx.activate(true);
    Ok(())
}

impl Render for AppModel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_view();
        let status = self.status().clone();
        let event_count = self.event_count();
        let needs_resync = self.needs_resync();
        let collapsed = self.sidebar_collapsed(cx);
        let badges = self.sidebar_badges(cx);
        let notice = self.current_notice().cloned();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::canvas())
            .text_color(theme::text())
            .font_family("Segoe UI")
            .child(header(active, notice, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(sidebar(active, collapsed, badges, cx))
                    .child(content_panel(self, active, cx)),
            )
            .child(status_bar(status, event_count, needs_resync))
    }
}

fn header(
    active: AppView,
    notice: Option<HeaderNotice>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(
            div()
                .h(px(58.0))
                .flex()
                .items_center()
                .justify_between()
                .px_5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size_8()
                                .rounded_md()
                                .bg(theme::accent())
                                .text_color(theme::canvas())
                                .font_weight(gpui::FontWeight::BOLD)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child("HV"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child("Hard Vibe Code"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::text_muted())
                                        .child("GPUI / Rust native"),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(active.description()),
                ),
        )
        .when_some(notice, |header, notice| {
            let (background, foreground, prefix) = match notice.kind {
                HeaderNoticeKind::Notice => (theme::accent_soft(), theme::accent(), "Info"),
                HeaderNoticeKind::Warning => (theme::warning_soft(), theme::warning(), "Aviso"),
                HeaderNoticeKind::Error => (theme::danger_soft(), theme::danger(), "Erro"),
            };
            header.child(
                div()
                    .id("header-notice")
                    .min_h(px(34.0))
                    .flex()
                    .items_center()
                    .px_5()
                    .py_2()
                    .bg(background)
                    .text_sm()
                    .text_color(foreground)
                    .cursor_pointer()
                    .on_click(cx.listener(|model, _, _, cx| {
                        model.dismiss_header_notice(cx);
                    }))
                    .child(format!("{prefix}: {}", notice.message)),
            )
        })
}

fn sidebar(
    active: AppView,
    collapsed: bool,
    badges: (usize, usize),
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .w(px(if collapsed { 72.0 } else { 228.0 }))
        .flex()
        .flex_col()
        .p_3()
        .gap_1()
        .border_r_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .children(AppView::ALL.into_iter().map(|view| {
            let selected = view == active;
            let badge = match view {
                AppView::Workspace => badges.0,
                AppView::Pulsations => badges.1,
                _ => 0,
            };
            div()
                .id(view.id())
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |item| {
                    item.bg(theme::accent_soft()).text_color(theme::accent())
                })
                .when(!selected, |item| {
                    item.text_color(theme::text_muted())
                        .hover(|style| style.bg(theme::surface_raised()).text_color(theme::text()))
                })
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.select_view(view, cx);
                }))
                .child(
                    div()
                        .size_6()
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(view.glyph()),
                )
                .when(!collapsed, |item| {
                    item.child(div().flex_1().child(view.short_label()))
                })
                .when(badge > 0, |item| {
                    item.child(
                        div()
                            .min_w(px(20.0))
                            .h(px(20.0))
                            .px_1()
                            .rounded_full()
                            .bg(if view == AppView::Workspace {
                                theme::danger_soft()
                            } else {
                                theme::accent_soft()
                            })
                            .text_color(if view == AppView::Workspace {
                                theme::danger()
                            } else {
                                theme::accent()
                            })
                            .text_xs()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(badge.to_string()),
                    )
                })
        }))
        .child(
            div()
                .id("toggle-sidebar")
                .mt_auto()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .text_color(theme::text_muted())
                .hover(|style| style.bg(theme::surface_raised()).text_color(theme::text()))
                .on_click(cx.listener(|model, _, _, cx| {
                    model.toggle_sidebar(cx);
                }))
                .child(if collapsed { "»" } else { "« Recolher" }),
        )
}

fn content_panel(model: &AppModel, active: AppView, cx: &mut Context<AppModel>) -> AnyElement {
    if active == AppView::Workspace {
        return workspace_content_panel(model, cx).into_any_element();
    }

    let (metric, note) = match active {
        AppView::Workspace => unreachable!("workspace possui painel nativo específico"),
        AppView::Pulsations => {
            let view = model.pulsations.read(cx);
            (
                format!("{} job(s) em execução", view.running_jobs),
                format!("{} decisão(ões) HITL aguardando", view.awaiting_hitl),
            )
        }
        AppView::Vault => {
            let view = model.vault.read(cx);
            (
                format!("{} nota(s) indexada(s)", view.indexed_notes),
                if view.stale {
                    "Snapshot desatualizado"
                } else {
                    "Índice sincronizado"
                }
                .into(),
            )
        }
        AppView::Chat => {
            let view = model.chat.read(cx);
            (
                if view.streaming {
                    "Streaming ativo"
                } else {
                    "Chat ocioso"
                }
                .into(),
                view.active_conversation
                    .clone()
                    .unwrap_or_else(|| "Nenhuma conversa selecionada".into())
                    .to_string(),
            )
        }
        AppView::Settings => {
            let view = model.settings.read(cx);
            (
                format!("{} provedor(es) configurado(s)", view.configured_providers),
                "Rotas e credenciais permanecem no core/keyring".into(),
            )
        }
        AppView::Logs => {
            let view = model.logs.read(cx);
            (
                format!("{} evento(s) nesta sessão", view.visible_entries),
                "Barramento tipado com capacidade para 1.024 eventos".into(),
            )
        }
    };

    div()
        .flex_1()
        .min_w_0()
        .p_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(active.label()),
                )
                .when(active == AppView::Workspace, |row| {
                    row.child(
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
                    )
                }),
        )
        .child(
            div()
                .p_5()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface_raised())
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_lg().child(metric))
                .child(div().text_color(theme::text_muted()).child(note))
                .child(
                    div()
                        .mt_3()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(active.description()),
                ),
        )
        .into_any_element()
}

#[derive(Clone)]
struct WorkspaceTreeRow {
    depth: usize,
    node: FileNode,
}

struct HitlSurfaceContent {
    checkpoint_id: Option<String>,
    title: Option<String>,
    input: String,
    generated: String,
    busy: bool,
    error: Option<String>,
}

fn workspace_content_panel(model: &AppModel, cx: &mut Context<AppModel>) -> impl IntoElement {
    let (root_path, tree, tabs, active_path, editor, view_mode, context_drawer_open, diff_expanded) = {
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
            workspace.view_mode,
            workspace.context_drawer_open,
            workspace.diff_expanded,
        )
    };
    let active_tab = active_path
        .as_deref()
        .and_then(|path| tabs.iter().find(|tab| tab.relative_path.as_ref() == path))
        .cloned();
    let hitl = {
        let pulsations = model.pulsations.read(cx);
        HitlSurfaceContent {
            checkpoint_id: pulsations.hitl.pulsation_id.clone(),
            title: pulsations.hitl.title.clone(),
            input: pulsations.hitl.input.clone(),
            generated: pulsations.hitl.generated.clone(),
            busy: pulsations.hitl.busy,
            error: pulsations.hitl.error.clone(),
        }
    };
    let mut tree_rows = Vec::new();
    flatten_workspace_tree(&tree, 0, &mut tree_rows);
    let root_label = root_path
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "Nenhum workspace selecionado".into());
    let active_for_conflict = active_path.clone();

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
                                .child("Workspace & HITL"),
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
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(mode_button(
                            "workspace-mode-editor",
                            "Editor",
                            view_mode == WorkspaceViewMode::Editor,
                            WorkspaceViewMode::Editor,
                            cx,
                        ))
                        .child(mode_button(
                            "workspace-mode-hitl",
                            "HITL review",
                            view_mode == WorkspaceViewMode::Hitl,
                            WorkspaceViewMode::Hitl,
                            cx,
                        ))
                        .child(
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
        .when_some(
            active_tab.as_ref().and_then(|tab| tab.conflict.clone()),
            |panel, conflict| {
                let reload_path = active_for_conflict.clone().unwrap_or_default();
                let overwrite_path = reload_path.clone();
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
                                .bg(theme::surface_raised())
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| {
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
                                .bg(theme::danger_soft())
                                .text_color(theme::danger())
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.resolve_workspace_conflict_by_overwrite(
                                        overwrite_path.clone(),
                                        cx,
                                    );
                                }))
                                .child("Sobrescrever"),
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
                .when(!diff_expanded, |layout| {
                    layout.child(workspace_tree_panel(tree_rows, cx))
                })
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
                        .when(view_mode == WorkspaceViewMode::Editor, |surface| {
                            surface.child(workspace_editor_surface(
                                tabs.clone(),
                                active_path.clone(),
                                active_tab.clone(),
                                editor,
                                cx,
                            ))
                        })
                        .when(view_mode == WorkspaceViewMode::Hitl, |surface| {
                            surface
                                .key_context("HitlReview")
                                .on_action(cx.listener(|model, _: &ApproveHitl, _, cx| {
                                    model.apply_hitl_decision(HitlDecision::Approve, cx);
                                }))
                                .on_action(cx.listener(|model, _: &RequestHitlRevision, _, cx| {
                                    model.apply_hitl_decision(HitlDecision::RequestRevision, cx);
                                }))
                                .child(workspace_hitl_surface(
                                    active_tab.clone(),
                                    hitl,
                                    diff_expanded,
                                    cx,
                                ))
                        }),
                )
                .when(!diff_expanded && context_drawer_open, |layout| {
                    layout.child(context_drawer(active_tab.as_ref()))
                }),
        )
}

fn mode_button(
    id: &'static str,
    label: &'static str,
    selected: bool,
    view_mode: WorkspaceViewMode,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |button| {
            button.bg(theme::accent_soft()).text_color(theme::accent())
        })
        .when(!selected, |button| {
            button
                .bg(theme::surface_raised())
                .text_color(theme::text_muted())
        })
        .on_click(cx.listener(move |model, _, _, cx| {
            model.set_workspace_view_mode(view_mode, cx);
        }))
        .child(label)
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
            div()
                .id(SharedString::from(format!("workspace-tree-{index}")))
                .pl(px(8.0 + row.depth as f32 * 14.0))
                .pr_2()
                .py_1()
                .rounded_sm()
                .text_sm()
                .text_color(if is_file {
                    theme::text()
                } else {
                    theme::text_muted()
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
                    if is_file { "·" } else { "▾" },
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

fn render_markdown_block(block: MarkdownBlock) -> AnyElement {
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
            .font_family("Cascadia Mono")
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

fn workspace_hitl_surface(
    active_tab: Option<WorkspaceTabModel>,
    hitl: HitlSurfaceContent,
    expanded: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let input = active_tab
        .as_ref()
        .map(|tab| tab.base_content.as_str())
        .unwrap_or(hitl.input.as_str());
    let generated = active_tab
        .as_ref()
        .map(|tab| tab.content.as_str())
        .unwrap_or(hitl.generated.as_str());
    let review_title = active_tab
        .as_ref()
        .map(|tab| tab.relative_path.to_string())
        .or(hitl.title)
        .unwrap_or_else(|| "Markdown Diff".into());
    let rows = build_diff_rows(input, generated);
    let has_checkpoint = hitl.checkpoint_id.is_some();
    let busy = hitl.busy;
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(52.0))
                .flex()
                .items_center()
                .justify_between()
                .px_4()
                .border_b_1()
                .border_color(theme::border())
                .child(
                    div()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("CHECKPOINT REVIEW"),
                        )
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(review_title),
                        ),
                )
                .child(
                    div()
                        .id("workspace-diff-expand")
                        .px_3()
                        .py_1()
                        .rounded_sm()
                        .bg(theme::surface_raised())
                        .cursor_pointer()
                        .on_click(cx.listener(|model, _, _, cx| {
                            model.toggle_workspace_diff(cx);
                        }))
                        .child(if expanded {
                            "Sair da expansão"
                        } else {
                            "Expandir"
                        }),
                ),
        )
        .child(div().flex_1().min_h_0().flex().children(
            [("Input baseline", true), ("Generated draft", false)].map(|(title, left)| {
                div()
                    .id(if left {
                        "workspace-diff-left"
                    } else {
                        "workspace-diff-right"
                    })
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .when(left, |panel| {
                        panel.border_r_1().border_color(theme::border())
                    })
                    .child(
                        div()
                            .p_3()
                            .bg(theme::surface_raised())
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .children(rows.iter().map(move |row| {
                        let side = if left { &row.left } else { &row.right };
                        let color = match side.mark {
                            DiffMark::Add => theme::accent(),
                            DiffMark::Remove | DiffMark::Change => theme::warning(),
                            DiffMark::Neutral | DiffMark::Empty => theme::text_muted(),
                        };
                        div()
                            .min_h(px(24.0))
                            .flex()
                            .gap_2()
                            .px_3()
                            .font_family("Cascadia Mono")
                            .text_sm()
                            .text_color(color)
                            .child(
                                div().w(px(34.0)).text_color(theme::text_muted()).child(
                                    side.line.map(|line| line.to_string()).unwrap_or_default(),
                                ),
                            )
                            .child(side.marker.to_string())
                            .child(if side.text.is_empty() {
                                " ".to_owned()
                            } else {
                                side.text.clone()
                            })
                    }))
            }),
        ))
        .when_some(hitl.error, |surface, error| {
            surface.child(
                div()
                    .px_4()
                    .py_2()
                    .bg(theme::danger_soft())
                    .text_color(theme::danger())
                    .child(error),
            )
        })
        .child(
            div()
                .min_h(px(54.0))
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .border_t_1()
                .border_color(theme::border())
                .child(hitl_button(
                    "hitl-approve",
                    "Aprovar  Enter",
                    !busy && has_checkpoint,
                    HitlDecision::Approve,
                    false,
                    cx,
                ))
                .child(hitl_button(
                    "hitl-revision",
                    "Solicitar revisão  R",
                    !busy && has_checkpoint,
                    HitlDecision::RequestRevision,
                    false,
                    cx,
                ))
                .child(hitl_button(
                    "hitl-reject",
                    "Rejeitar",
                    !busy && has_checkpoint,
                    HitlDecision::Reject,
                    true,
                    cx,
                ))
                .when(!has_checkpoint, |bar| {
                    bar.child(
                        div()
                            .ml_auto()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child("Nenhum checkpoint aguardando decisão."),
                    )
                })
                .when(busy, |bar| {
                    bar.child(
                        div()
                            .ml_auto()
                            .text_sm()
                            .text_color(theme::accent())
                            .child("Aplicando decisão…"),
                    )
                }),
        )
}

fn hitl_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    decision: HitlDecision,
    danger: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if danger {
            theme::danger_soft()
        } else {
            theme::surface_raised()
        })
        .text_color(if danger {
            theme::danger()
        } else {
            theme::text()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(0.82))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.apply_hitl_decision(decision, cx);
                }))
        })
        .when(!enabled, |button| button.opacity(0.45))
        .child(label)
}

fn context_drawer(active_tab: Option<&WorkspaceTabModel>) -> impl IntoElement {
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
}

fn context_item(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().text_sm().child(value))
}

fn status_bar(
    status: gpui::SharedString,
    event_count: u64,
    needs_resync: bool,
) -> impl IntoElement {
    div()
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_t_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .text_xs()
        .text_color(if needs_resync {
            theme::danger()
        } else {
            theme::text_muted()
        })
        .child(status)
        .child(format!("{event_count} evento(s)"))
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn preserves_collapsed_navigation_and_semantic_badges(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.pulsations.update(cx, |pulsations, _| {
                pulsations.awaiting_hitl = 2;
                pulsations.running_jobs = 3;
            });
            model.toggle_sidebar(cx);
            model.select_view(AppView::Logs, cx);
        });

        cx.read(|cx| {
            let model = model.read(cx);
            assert_eq!(model.active_view(), AppView::Logs);
            assert!(model.sidebar_collapsed(cx));
            assert_eq!(model.sidebar_badges(cx), (2, 3));
        });
    }

    #[gpui::test]
    fn queues_header_errors_warnings_and_notices(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.push_header_notice(HeaderNoticeKind::Notice, "Workspace salvo", cx);
            model.push_header_notice(HeaderNoticeKind::Warning, "Banco indisponível", cx);
            model.push_header_notice(HeaderNoticeKind::Error, "Falha no runtime", cx);
        });

        cx.read(|cx| {
            assert_eq!(
                model.read(cx).current_notice().unwrap().message,
                "Falha no runtime"
            );
        });
        model.update(cx, |model, cx| model.dismiss_header_notice(cx));
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).current_notice().unwrap().message,
                "Banco indisponível"
            );
        });
        model.update(cx, |model, cx| model.dismiss_header_notice(cx));
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).current_notice().unwrap().message,
                "Workspace salvo"
            );
        });
    }
}
