use std::sync::Arc;

use anyhow::Context as _;
use gpui::{
    App, AppContext as _, Bounds, Context, IntoElement, Render, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, size,
};
use hvc_core::AppCore;

use crate::app_model::{AppModel, AppView};
use crate::native_editor::register_editor_keybindings;
use crate::session::{HeaderNotice, HeaderNoticeKind};
use crate::theme;

pub fn open_main_window(core: Arc<AppCore>, cx: &mut App) -> anyhow::Result<()> {
    register_editor_keybindings(cx);
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

fn content_panel(
    model: &AppModel,
    active: AppView,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let (metric, note, editor) = match active {
        AppView::Workspace => {
            let workspace = model.workspace.read(cx);
            let root = workspace
                .root_path
                .clone()
                .unwrap_or_else(|| "Nenhum workspace selecionado".into());
            (
                format!("{} aba(s) aberta(s)", workspace.open_tab_count()),
                root.to_string(),
                Some(workspace.editor.clone()),
            )
        }
        AppView::Pulsations => {
            let view = model.pulsations.read(cx);
            (
                format!("{} job(s) em execução", view.running_jobs),
                format!("{} decisão(ões) HITL aguardando", view.awaiting_hitl),
                None,
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
                None,
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
                None,
            )
        }
        AppView::Settings => {
            let view = model.settings.read(cx);
            (
                format!("{} provedor(es) configurado(s)", view.configured_providers),
                "Rotas e credenciais permanecem no core/keyring".into(),
                None,
            )
        }
        AppView::Logs => {
            let view = model.logs.read(cx);
            (
                format!("{} evento(s) nesta sessão", view.visible_entries),
                "Barramento tipado com capacidade para 1.024 eventos".into(),
                None,
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
        .when_some(editor, |panel, editor| {
            panel.child(
                div()
                    .flex_1()
                    .min_h(px(240.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme::border())
                    .overflow_hidden()
                    .child(editor),
            )
        })
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
