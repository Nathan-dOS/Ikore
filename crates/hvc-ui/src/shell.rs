use std::sync::Arc;

use anyhow::Context as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Focusable, IntoElement, KeyBinding, Render,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use hvc_core::AppCore;

use crate::agents_view::agents_panel;
use crate::app_model::{AppModel, AppTab, AppView, TabBadges};
use crate::chat_view::chat_panel;
use crate::hitl::register_hitl_keybindings;
use crate::logs_view::logs_panel;
use crate::native_editor::register_editor_keybindings;
use crate::pulsations_view::pulsations_panel;
use crate::session::{HeaderNotice, HeaderNoticeKind};
use crate::settings_view::settings_panel;
use crate::text_input::register_text_input_keybindings;
use crate::theme;
use crate::vault_view::vault_panel;
use crate::workspace_view::workspace_content_panel;

/// One header-tab badge pill: `(count, background, foreground)`.
type BadgePill = (usize, fn() -> gpui::Rgba, fn() -> gpui::Rgba);

actions!(
    navigation,
    [
        SelectAgentTab,
        SelectEngineTab,
        SelectCompanionTab,
        SelectSettingsView,
        SelectSubView1,
        SelectSubView2,
        SelectSubView3,
        PreviousSection,
        NextSection,
        RunPrimaryFlowStep
    ]
);

pub(crate) fn register_navigation_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-1", SelectAgentTab, None),
        KeyBinding::new("ctrl-2", SelectEngineTab, None),
        KeyBinding::new("ctrl-3", SelectCompanionTab, None),
        KeyBinding::new("ctrl-,", SelectSettingsView, None),
        // Jump to the Nth sub-view of the active header tab.
        KeyBinding::new("alt-1", SelectSubView1, None),
        KeyBinding::new("alt-2", SelectSubView2, None),
        KeyBinding::new("alt-3", SelectSubView3, None),
        // Cycle the current view's own section strip (Pulsations/Forge/Settings).
        KeyBinding::new("ctrl-shift-[", PreviousSection, None),
        KeyBinding::new("ctrl-shift-]", NextSection, None),
        // Runs the flow bar's primary suggested next step.
        KeyBinding::new("ctrl-enter", RunPrimaryFlowStep, None),
    ]);
}

pub fn open_main_window(core: Arc<AppCore>, cx: &mut App) -> anyhow::Result<()> {
    register_editor_keybindings(cx);
    register_text_input_keybindings(cx);
    register_hitl_keybindings(cx);
    register_navigation_keybindings(cx);
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
    let (window_width, window_height) = initial_window_size();
    let bounds = Bounds::centered(None, size(px(window_width), px(window_height)), cx);
    let handle = cx
        .open_window(
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
        .context("GPUI failed to create main window")?;
    let _ = handle.update(cx, |_model, window, _cx| {
        window.activate_window();
    });
    cx.activate(true);
    Ok(())
}

fn initial_window_size() -> (f32, f32) {
    #[cfg(debug_assertions)]
    if let Some(size) = std::env::var("HVC_QA_WINDOW_SIZE")
        .ok()
        .and_then(|value| parse_qa_window_size(&value))
    {
        return size;
    }
    (1280.0, 820.0)
}

#[cfg(any(debug_assertions, test))]
fn parse_qa_window_size(value: &str) -> Option<(f32, f32)> {
    let (width, height) = value.trim().split_once(['x', 'X'])?;
    let width = width.trim().parse::<f32>().ok()?;
    let height = height.trim().parse::<f32>().ok()?;
    (width.is_finite()
        && height.is_finite()
        && (960.0..=3840.0).contains(&width)
        && (640.0..=2160.0).contains(&height))
    .then_some((width, height))
}

impl Render for AppModel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.observe_performance_frame(window);
        let active = self.active_view();
        let status = self.status().clone();
        let event_count = self.event_count();
        let needs_resync = self.needs_resync();
        let active_tab = self.active_tab();
        let badges = self.sidebar_badges(cx);
        let notice = self.current_notice().cloned();
        if self.agents.read(cx).active_model_picker.is_some()
            && matches!(active, AppView::Agents | AppView::Chat)
        {
            self.cli_model_search_input
                .read(cx)
                .focus_handle(cx)
                .focus(window);
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::canvas())
            .text_color(theme::text())
            .font_family(theme::FONT_UI)
            .on_action(cx.listener(|model, _: &SelectAgentTab, _, cx| {
                model.select_tab(AppTab::Agent, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectEngineTab, _, cx| {
                model.select_tab(AppTab::Engine, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectCompanionTab, _, cx| {
                model.select_tab(AppTab::Companion, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectSettingsView, _, cx| {
                model.select_view(AppView::Settings, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectSubView1, _, cx| {
                model.select_nth_sub_view(0, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectSubView2, _, cx| {
                model.select_nth_sub_view(1, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectSubView3, _, cx| {
                model.select_nth_sub_view(2, cx);
            }))
            .on_action(cx.listener(|model, _: &PreviousSection, _, cx| {
                model.cycle_current_section(-1, cx);
            }))
            .on_action(cx.listener(|model, _: &NextSection, _, cx| {
                model.cycle_current_section(1, cx);
            }))
            .on_action(cx.listener(|model, _: &RunPrimaryFlowStep, window, cx| {
                if let Some(step) = crate::flow::next_steps(model, cx).into_iter().next() {
                    model.run_next_step(step.target, window, cx);
                }
            }))
            .child(header(active_tab, active, badges, notice, cx))
            .child(flow_bar(self, active_tab, active, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(content_panel(self, active, cx)),
            )
            .child(status_bar(status, event_count, needs_resync, badges, cx))
            .when(self.forge.new_open, |panel| {
                panel.child(crate::forge_view::capture_overlay(self, cx))
            })
    }
}

fn header(
    active_tab: AppTab,
    active: AppView,
    badges: TabBadges,
    notice: Option<HeaderNotice>,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_b_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(
            div()
                .h(px(58.0))
                .flex()
                .items_center()
                .gap_4()
                .px_5()
                .children(AppTab::ALL.into_iter().map(|tab| {
                    let selected = active != AppView::Settings && tab == active_tab;
                    // Engine can show two counters side by side (reviews waiting take priority
                    // visually via the warning color); Agent and Companion each get one.
                    let pills: &[BadgePill] = match tab {
                        AppTab::Engine => &[
                            (
                                badges.engine_awaiting_hitl,
                                theme::warning_soft,
                                theme::warning,
                            ),
                            (
                                badges.engine_running_jobs,
                                theme::accent_soft,
                                theme::accent,
                            ),
                        ],
                        AppTab::Agent => {
                            &[(badges.agent_failed_runs, theme::danger_soft, theme::danger)]
                        }
                        AppTab::Companion => &[(
                            badges.companion_streaming,
                            theme::accent_soft,
                            theme::accent,
                        )],
                    };
                    div()
                        .id(tab.id())
                        .debug_selector(move || tab.id().into())
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(if selected {
                            theme::accent_soft()
                        } else {
                            theme::surface()
                        })
                        .text_color(if selected {
                            theme::accent()
                        } else {
                            theme::text_muted()
                        })
                        .hover(|style| style.bg(theme::surface_raised()))
                        .on_click(cx.listener(move |model, _, _, cx| model.select_tab(tab, cx)))
                        .child(tab.label())
                        .children(pills.iter().filter(|(count, _, _)| *count > 0).map(
                            |&(count, bg, fg)| {
                                div()
                                    .min_w(px(20.0))
                                    .px_1()
                                    .rounded_full()
                                    .text_xs()
                                    .bg(bg())
                                    .text_color(fg())
                                    .child(count.to_string())
                            },
                        ))
                }))
                .when(active_tab == AppTab::Engine, |header| {
                    header.child(crate::forge_view::button(
                        "new-work",
                        "New",
                        crate::forge::ForgeAction::New(false),
                        cx,
                    ))
                })
                .child(
                    div()
                        .id("settings")
                        .debug_selector(|| "settings".into())
                        .ml_auto()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2()
                        .bg(if active == AppView::Settings {
                            theme::accent_soft()
                        } else {
                            theme::surface()
                        })
                        .text_color(if active == AppView::Settings {
                            theme::accent()
                        } else {
                            theme::text_muted()
                        })
                        .hover(|style| style.bg(theme::surface_raised()))
                        .on_click(
                            cx.listener(|model, _, _, cx| model.select_view(AppView::Settings, cx)),
                        )
                        .child("⚙")
                        .child("Settings"),
                ),
        )
        .when_some(notice, |header, notice| {
            let (background, foreground, prefix) = match notice.kind {
                HeaderNoticeKind::Notice => (theme::accent_soft(), theme::accent(), "Info"),
                HeaderNoticeKind::Warning => (theme::warning_soft(), theme::warning(), "Warning"),
                HeaderNoticeKind::Error => (theme::danger_soft(), theme::danger(), "Error"),
            };
            header.child(
                div()
                    .id("header-notice")
                    .min_h(px(34.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .py_2()
                    .bg(background)
                    .text_sm()
                    .text_color(foreground)
                    .cursor_pointer()
                    .on_click(cx.listener(|model, _, _, cx| {
                        model.dismiss_header_notice(cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(format!("{prefix}:")),
                            )
                            .child(div().child(notice.message.clone())),
                    )
                    .when_some(notice.action.clone(), |header, action| {
                        header.child(
                            div()
                                .id("header-notice-action")
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .bg(theme::surface_raised())
                                .text_color(theme::text())
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    cx.stop_propagation();
                                    model.open_pulsation_review(action.pulsation_id.clone(), cx);
                                    model.dismiss_header_notice(cx);
                                }))
                                .child(action.label.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("header-notice-close")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(theme::hover_bg()))
                            .child("✕ Fechar"),
                    ),
            )
        })
}

/// The flow bar: sub-view pills for the active tab (or Settings' own Connections/Routes/Runtime
/// pills) on the left, and the flow engine's suggested next step(s) on the right. Rendered in every
/// view — including Settings, which previously hid this row entirely — so the layout never jumps
/// and the next action is always in the same place. See the UI flow plan Parts 2-3.
fn flow_bar(
    model: &AppModel,
    tab: AppTab,
    active: AppView,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let left_pills: Vec<AnyElement> = if active == AppView::Settings {
        let section = model.settings.read(cx).settings_section;
        [
            (
                crate::settings::SettingsSection::Connections,
                "settings-section-connections",
                "Connections",
            ),
            (
                crate::settings::SettingsSection::Routes,
                "settings-section-routes",
                "Routes",
            ),
            (
                crate::settings::SettingsSection::Runtime,
                "settings-section-runtime",
                "Runtime",
            ),
        ]
        .into_iter()
        .map(|(target, id, label)| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_3()
                .py_1()
                .rounded_full()
                .cursor_pointer()
                .text_sm()
                .bg(if section == target {
                    theme::accent_soft()
                } else {
                    theme::surface()
                })
                .text_color(if section == target {
                    theme::accent()
                } else {
                    theme::text_muted()
                })
                .hover(|style| style.bg(theme::surface_raised()))
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.settings.update(cx, |settings, _| {
                        settings.settings_section = target;
                    });
                    cx.notify();
                }))
                .child(label)
                .into_any_element()
        })
        .collect()
    } else {
        tab.sub_views()
            .iter()
            .copied()
            .map(|view| {
                div()
                    .id(view.id())
                    .debug_selector(move || view.id().into())
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .cursor_pointer()
                    .text_sm()
                    .bg(if active == view {
                        theme::accent_soft()
                    } else {
                        theme::surface()
                    })
                    .text_color(if active == view {
                        theme::accent()
                    } else {
                        theme::text_muted()
                    })
                    .hover(|style| style.bg(theme::surface_raised()))
                    .on_click(cx.listener(move |model, _, _, cx| model.select_view(view, cx)))
                    .child(if view == AppView::Chat {
                        "Chat"
                    } else {
                        view.short_label()
                    })
                    .into_any_element()
            })
            .collect()
    };

    let steps = crate::flow::next_steps(model, cx);

    div()
        .id("flow-bar")
        .debug_selector(|| "flow-bar".into())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .gap_2()
        .px_5()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(div().flex().items_center().gap_2().children(left_pills))
        .when(!steps.is_empty(), |bar| {
            bar.child(div().flex().items_center().gap_2().children(
                steps.into_iter().enumerate().map(|(index, step)| {
                    let primary = index == 0;
                    let id = step.id;
                    let target = step.target;
                    div()
                        .id(id)
                        .debug_selector(move || id.into())
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .cursor_pointer()
                        .text_sm()
                        .bg(if primary {
                            theme::accent()
                        } else {
                            theme::surface_raised()
                        })
                        .text_color(if primary {
                            theme::canvas()
                        } else {
                            theme::text()
                        })
                        .hover(|style| style.opacity(theme::HOVER_OPACITY))
                        .on_click(cx.listener(move |model, _, window, cx| {
                            model.run_next_step(target.clone(), window, cx);
                        }))
                        .child(step.label)
                }),
            ))
        })
}

fn content_panel(model: &AppModel, active: AppView, cx: &mut Context<AppModel>) -> AnyElement {
    match active {
        AppView::Forge => crate::forge_view::forge_panel(model, cx),
        AppView::Workspace => workspace_content_panel(model, cx).into_any_element(),
        AppView::Pulsations => pulsations_panel(model, cx),
        AppView::Vault => vault_panel(model, cx),
        AppView::Chat => chat_panel(model, cx),
        AppView::Agents => agents_panel(model, cx),
        AppView::Settings => settings_panel(model, cx),
        AppView::Logs => logs_panel(model, cx),
    }
}

/// A status-bar segment that jumps to `view` on click. Each segment is independently clickable
/// (see the UI flow plan Part 4.4) — the status bar is another place the next relevant view is one
/// click away, not just a passive readout.
fn status_segment(
    id: &'static str,
    color: gpui::Rgba,
    content: impl IntoElement,
    view: AppView,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_color(color)
        .hover(|style| style.opacity(theme::HOVER_OPACITY))
        .on_click(cx.listener(move |model, _, _, cx| model.select_view(view, cx)))
        .child(content)
        .into_any_element()
}

fn status_bar(
    status: gpui::SharedString,
    event_count: u64,
    needs_resync: bool,
    badges: TabBadges,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let hitl_count = badges.engine_awaiting_hitl;
    let active_jobs = badges.engine_running_jobs;
    div()
        .id("status-bar")
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_t_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .text_xs()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().size_2().rounded_full().bg(if needs_resync {
                    theme::danger()
                } else {
                    theme::success()
                }))
                .child(
                    div()
                        .text_color(if needs_resync {
                            theme::danger()
                        } else {
                            theme::text_muted()
                        })
                        .child(status),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .when(hitl_count > 0, |seg| {
                    seg.child(status_segment(
                        "status-hitl",
                        theme::warning(),
                        format!("⚠ {hitl_count} pending review(s)"),
                        AppView::Pulsations,
                        cx,
                    ))
                })
                .when(active_jobs > 0, |seg| {
                    seg.child(status_segment(
                        "status-jobs",
                        theme::accent(),
                        format!("● {active_jobs} job(s) running"),
                        AppView::Pulsations,
                        cx,
                    ))
                })
                .when(hitl_count == 0 && active_jobs == 0, |seg| {
                    seg.child(div().text_color(theme::text_muted()).child("System ready"))
                }),
        )
        .child(status_segment(
            "status-events",
            theme::text_muted(),
            format!("{event_count} event(s)"),
            AppView::Logs,
            cx,
        ))
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[test]
    fn qa_window_size_accepts_only_supported_finite_dimensions() {
        assert_eq!(parse_qa_window_size("1024x768"), Some((1024.0, 768.0)));
        assert_eq!(
            parse_qa_window_size(" 2560 X 1440 "),
            Some((2560.0, 1440.0))
        );
        assert_eq!(parse_qa_window_size("959x768"), None);
        assert_eq!(parse_qa_window_size("1024x639"), None);
        assert_eq!(parse_qa_window_size("NaNx768"), None);
        assert_eq!(parse_qa_window_size("invalid"), None);
    }

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
            let badges = model.sidebar_badges(cx);
            assert_eq!(badges.engine_awaiting_hitl, 2);
            assert_eq!(badges.engine_running_jobs, 3);
        });
    }

    #[gpui::test]
    fn queues_header_errors_warnings_and_notices(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            model.push_header_notice(HeaderNoticeKind::Notice, "Workspace saved", cx);
            model.push_header_notice(HeaderNoticeKind::Warning, "Database unavailable", cx);
            model.push_header_notice(HeaderNoticeKind::Error, "Runtime failure", cx);
        });

        cx.read(|cx| {
            assert_eq!(
                model.read(cx).current_notice().unwrap().message,
                "Runtime failure"
            );
        });
        model.update(cx, |model, cx| model.dismiss_header_notice(cx));
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).current_notice().unwrap().message,
                "Database unavailable"
            );
        });
        model.update(cx, |model, cx| model.dismiss_header_notice(cx));
        cx.read(|cx| {
            assert_eq!(
                model.read(cx).current_notice().unwrap().message,
                "Workspace saved"
            );
        });
    }

    #[gpui::test]
    fn two_step_confirmation_flags_track_and_reset_on_navigation(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);
        model.update(cx, |model, cx| {
            assert!(model.confirm_workspace_overwrite.is_none());
            assert!(!model.confirm_hitl_reject);
            assert!(!model.confirm_stage_reject);
            assert!(!model.confirm_brownfield_revoke);

            model.set_confirm_workspace_overwrite(Some("test.md".into()), cx);
            model.set_confirm_hitl_reject(true, cx);
            model.set_confirm_stage_reject(true, cx);
            model.set_confirm_brownfield_revoke(true, cx);

            assert_eq!(
                model.confirm_workspace_overwrite.as_deref(),
                Some("test.md")
            );
            assert!(model.confirm_hitl_reject);
            assert!(model.confirm_stage_reject);
            assert!(model.confirm_brownfield_revoke);

            // Switching views resets all confirmation flags for safety
            model.select_view(AppView::Logs, cx);

            assert!(model.confirm_workspace_overwrite.is_none());
            assert!(!model.confirm_hitl_reject);
            assert!(!model.confirm_stage_reject);
            assert!(!model.confirm_brownfield_revoke);
        });
    }
}
