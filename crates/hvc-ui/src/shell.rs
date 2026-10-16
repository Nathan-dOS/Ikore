use std::sync::Arc;

use anyhow::Context as _;
use gpui::{
    AnyElement, App, Bounds, Context, Focusable, IntoElement, KeyBinding, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use hvc_core::AppCore;
use hvc_core::embedding_service::ModelPullEvent;
use hvc_core::orchestration::IndexingEvent;

use crate::agents_view::agents_panel;
use crate::app_model::{AppModel, AppTab, AppView, TabBadges};
use crate::chat_view::chat_panel;
use crate::flow::FlowTarget;
use crate::hitl::register_hitl_keybindings;
use crate::icons::Icon;
use crate::logs_view::logs_panel;
use crate::native_editor::register_editor_keybindings;
use crate::pulsations_view::pulsations_panel;
#[cfg(test)]
use crate::session::HeaderNoticeKind;
use crate::settings::SettingsPage;
use crate::sizing::{HEADER_HEIGHT, NAV_RAIL_WIDTH, STATUS_BAR_HEIGHT};
use crate::text_input::register_text_input_keybindings;
use crate::theme;
use crate::vault_view::vault_panel;
use crate::workspace_view::workspace_content_panel;

actions!(
    navigation,
    [
        SelectHomeView,
        SelectForgeView,
        SelectPulsationsView,
        SelectVaultView,
        SelectAgentsView,
        SelectLogsView,
        SelectChatView,
        SelectWorkspaceView,
        SelectSettingsView,
        PreviousSection,
        NextSection,
        RunPrimaryFlowStep,
        ToggleContextRail,
        FocusNextControl,
        FocusPreviousControl
    ]
);

/// Nav rail order, top to bottom: Home, then each `AppTab::RAIL_ORDER` group. `ctrl-1..8` follow
/// this order, so the shortcut for a view is its position on the rail.
pub(crate) const RAIL_VIEWS: [AppView; 8] = [
    AppView::Home,
    AppView::Forge,
    AppView::Pulsations,
    AppView::Vault,
    AppView::Agents,
    AppView::Logs,
    AppView::Chat,
    AppView::Workspace,
];

pub(crate) fn register_navigation_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-1", SelectHomeView, None),
        KeyBinding::new("ctrl-2", SelectForgeView, None),
        KeyBinding::new("ctrl-3", SelectPulsationsView, None),
        KeyBinding::new("ctrl-4", SelectVaultView, None),
        KeyBinding::new("ctrl-5", SelectAgentsView, None),
        KeyBinding::new("ctrl-6", SelectLogsView, None),
        KeyBinding::new("ctrl-7", SelectChatView, None),
        KeyBinding::new("ctrl-8", SelectWorkspaceView, None),
        KeyBinding::new("ctrl-,", SelectSettingsView, None),
        // Cycle the current view's own section strip (Pulsations/Forge/Settings).
        KeyBinding::new("ctrl-shift-[", PreviousSection, None),
        KeyBinding::new("ctrl-shift-]", NextSection, None),
        // Runs the flow engine's primary suggested next step.
        KeyBinding::new("ctrl-enter", RunPrimaryFlowStep, None),
        KeyBinding::new("ctrl-b", ToggleContextRail, None),
        // Tab walks the controls; the code editor keeps Tab for itself.
        // A bare `!NativeEditor` never matches: GPUI needs a context on the path to test.
        KeyBinding::new("tab", FocusNextControl, Some("Shell && !NativeEditor")),
        KeyBinding::new(
            "shift-tab",
            FocusPreviousControl,
            Some("Shell && !NativeEditor"),
        ),
    ]);
}

/// Keyboard hint shown in a rail tooltip for `view`, derived from its rail position.
fn rail_shortcut(view: AppView) -> Option<String> {
    RAIL_VIEWS
        .iter()
        .position(|candidate| *candidate == view)
        .map(|index| format!("ctrl-{}", index + 1))
}

pub fn open_main_window(core: Arc<AppCore>, cx: &mut App) -> anyhow::Result<()> {
    register_editor_keybindings(cx);
    register_text_input_keybindings(cx);
    register_hitl_keybindings(cx);
    crate::failure_checkpoint::register_failure_keybindings(cx);
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
        crate::title_bar::paint(window);
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
        let badges = self.sidebar_badges(cx);
        self.collect_notifications(cx);
        let unread = self.notifications.queue.unread();
        if self.agents.read(cx).active_model_picker.is_some()
            && matches!(active, AppView::Agents | AppView::Chat)
        {
            self.cli_model_search_input
                .read(cx)
                .focus_handle(cx)
                .focus(window);
        }

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::canvas())
            .text_color(theme::text())
            .font_family(theme::FONT_UI)
            // Named so bindings can exclude a descendant (Tab stays with the code editor).
            .key_context("Shell")
            .on_action(cx.listener(|model, _: &SelectHomeView, _, cx| {
                model.select_view(AppView::Home, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectForgeView, _, cx| {
                model.select_view(AppView::Forge, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectPulsationsView, _, cx| {
                model.select_view(AppView::Pulsations, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectVaultView, _, cx| {
                model.select_view(AppView::Vault, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectAgentsView, _, cx| {
                model.select_view(AppView::Agents, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectLogsView, _, cx| {
                model.select_view(AppView::Logs, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectChatView, _, cx| {
                model.select_view(AppView::Chat, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectWorkspaceView, _, cx| {
                model.select_view(AppView::Workspace, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectSettingsView, _, cx| {
                model.toggle_settings(cx);
            }))
            .on_action(cx.listener(|model, _: &PreviousSection, _, cx| {
                model.cycle_current_section(-1, cx);
            }))
            .on_action(cx.listener(|model, _: &NextSection, _, cx| {
                model.cycle_current_section(1, cx);
            }))
            .on_action(cx.listener(|model, _: &RunPrimaryFlowStep, window, cx| {
                // The suggested step can approve a review; never run it from behind Settings.
                if model.settings_modal.open {
                    return;
                }
                if let Some(step) = crate::flow::next_steps(model, cx).into_iter().next() {
                    model.run_next_step(step.target, window, cx);
                }
            }))
            .on_action(cx.listener(|model, _: &ToggleContextRail, _, cx| {
                model.toggle_context_rail(cx);
            }))
            .on_action(cx.listener(|model, _: &FocusNextControl, window, cx| {
                model.move_keyboard_focus(true, window, cx);
            }))
            .on_action(cx.listener(|model, _: &FocusPreviousControl, window, cx| {
                model.move_keyboard_focus(false, window, cx);
            }))
            .child(header(self, unread, window, cx))
            .child(
                div()
                    .id("shell-body")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(nav_rail(active, badges, self.settings_modal.open, cx))
                    .child(
                        // The one framed surface: the centre view sits in a raised container
                        // on the canvas; the rails, header and footer around it are frameless.
                        div()
                            .id("shell-center")
                            .debug_selector(|| "shell-center".into())
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .pb_2()
                            .when(active == AppView::Home, |center| center.pr_2())
                            .child(
                                div()
                                    .id("shell-frame")
                                    .debug_selector(|| "shell-frame".into())
                                    .relative()
                                    .flex()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::surface())
                                    .overflow_hidden()
                                    .child(content_panel(self, active, window, cx))
                                    .child(crate::resolution_view::chip_stack(self, cx)),
                            ),
                    )
                    .when_some(
                        crate::context_rail::context_rail(self, window, cx),
                        |body, rail| body.child(rail),
                    ),
            )
            .child(status_bar(self, badges, cx))
            .child(self.notification_overlay(window, cx))
            .when_some(self.choice_menu.clone(), |d, menu| {
                d.child(gpui::deferred(menu).with_priority(20))
            })
            .when(self.forge.new_open, |panel| {
                panel.child(crate::forge_view::capture_overlay(self, cx))
            })
            .when(self.resolution.read(cx).expanded.is_some(), |panel| {
                panel.child(crate::resolution_view::expanded_conversation(self, cx))
            })
            .children(self.settings_modal_overlay(window, cx))
    }
}

/// The informational header: brand and project on the left, the active pulsation's position in
/// the pipeline in the centre (a readout, not a control — the rail and Home carry the jumps), and
/// the system-level controls on the right.
fn header(
    model: &AppModel,
    unread: usize,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let project = model
        .workspace
        .read(cx)
        .root_path
        .as_ref()
        .and_then(|root| {
            std::path::Path::new(root.as_ref())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
    div()
        .id("header")
        .debug_selector(|| "header".into())
        .h(HEADER_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .bg(theme::canvas())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("header-brand")
                        .debug_selector(|| "header-brand".into())
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(crate::widgets::status_dot(theme::accent()))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("HARD VIBE CODE"),
                        ),
                )
                .when_some(project, |left, name| {
                    left.child(
                        div()
                            .id("header-project")
                            .debug_selector(|| "header-project".into())
                            .min_w_0()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .text_ellipsis()
                            .child(format!("· {name}")),
                    )
                }),
        )
        // The readout takes its content width (up to 60% of the header) and the two flexible
        // sides share the rest equally, so it stays centred without being squeezed into a third.
        .child(
            div()
                .flex_initial()
                .min_w_0()
                .max_w(gpui::relative(0.6))
                .flex()
                .items_center()
                .justify_center()
                .child(header_context(model, window, cx)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .justify_end()
                .gap_1()
                .child(header_health(model, cx))
                .child(crate::widgets::icon_button(
                    "notifications",
                    Icon::Bell,
                    "Notifications",
                    false,
                    Some(unread),
                    true,
                    cx,
                    |model, window, cx| model.toggle_notification_history(window, cx),
                )),
        )
}

/// Centre readout: who is executing right now — the stage run or background job holding a
/// runtime slot, with its elapsed time — and, when nothing runs, the selected pulsation's settled
/// state. Rendered in every view so the developer never has to open Pulsations to know it.
fn header_context(model: &AppModel, window: &Window, cx: &mut Context<AppModel>) -> AnyElement {
    let now = {
        let pulsations = model.pulsations.read(cx);
        let agents = model.agents.read(cx);
        crate::pipeline_status::NowRunning::derive(
            pulsations,
            agents.snapshot.as_ref(),
            chrono::Local::now(),
        )
    };
    let tooltip = SharedString::from(match &now.also {
        Some(also) => format!("{} — also running: {also}", now.text),
        None => now.text.clone(),
    });
    // Below the default window width the second job collapses to "+1" (named in the tooltip)
    // so the job that owns the pipeline keeps the room.
    let compact_also = window.viewport_size().width < px(1280.0);
    div()
        .id("header-context")
        .debug_selector(|| "header-context".into())
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(if now.busy {
            theme::running_soft()
        } else {
            theme::border()
        })
        .bg(if now.busy {
            theme::running_soft()
        } else {
            theme::surface()
        })
        .tooltip(move |_, cx| {
            cx.new(|_| crate::widgets::Tooltip::new(tooltip.clone()))
                .into()
        })
        .child(crate::widgets::status_dot(now.dot))
        .child(
            div()
                .min_w_0()
                .text_xs()
                .text_color(if now.busy {
                    theme::text()
                } else {
                    theme::text_muted()
                })
                .text_ellipsis()
                .child(now.text),
        )
        .when_some(now.also, |pill, also| {
            pill.child(
                div()
                    .id("header-context-also")
                    .debug_selector(|| "header-context-also".into())
                    .flex_shrink_0()
                    .px_2()
                    .rounded_full()
                    .bg(theme::surface_raised())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(if compact_also {
                        "+1".to_string()
                    } else {
                        format!("+ {also}")
                    }),
            )
        })
        .into_any_element()
}

/// System health at a glance: green unless the event stream lost sync. The tooltip carries the
/// facts that are about the app rather than the current pulsation.
fn header_health(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let needs_resync = model.needs_resync();
    let cli_ready = model.agents.read(cx).snapshot.as_ref().map(|snapshot| {
        snapshot
            .discovery
            .iter()
            .filter(|cli| cli.state == hvc_core::terminals::CliAvailabilityState::Available)
            .count()
    });
    let providers = model
        .settings
        .read(cx)
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.connections.len());
    let mut facts = vec![format!("v{}", env!("CARGO_PKG_VERSION"))];
    if let Some(count) = cli_ready {
        facts.push(format!("{count} CLI backend(s) ready"));
    }
    if let Some(count) = providers {
        facts.push(format!("{count} provider(s)"));
    }
    facts.push(if needs_resync {
        "Event stream out of sync".into()
    } else {
        "Event stream in sync".into()
    });
    let tooltip = SharedString::from(facts.join(" · "));
    let color = if needs_resync {
        theme::danger()
    } else {
        theme::success()
    };
    div()
        .id("header-health")
        .debug_selector(|| "header-health".into())
        .size(crate::widgets::ICON_BUTTON_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .tooltip(move |_, cx| {
            cx.new(|_| crate::widgets::Tooltip::new(tooltip.clone()))
                .into()
        })
        .child(crate::widgets::icon(
            Icon::HeartPulse,
            crate::widgets::ICON_SIZE,
            color,
        ))
        .into_any_element()
}

/// The icon-only nav rail: Home and New work on top, the grouped workbench views, and Settings
/// pinned to the bottom. Each item's tooltip carries its `ctrl-N` shortcut.
fn nav_rail(
    active: AppView,
    badges: TabBadges,
    settings_open: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let mut rail = div()
        .id("rail")
        .debug_selector(|| "rail".into())
        .w(NAV_RAIL_WIDTH)
        .flex_shrink_0()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .py_2()
        .bg(theme::canvas())
        .child(rail_item(AppView::Home, active, None, cx))
        .child(crate::widgets::icon_button(
            "new-work",
            Icon::Plus,
            "New work",
            false,
            None,
            true,
            cx,
            |model, _, cx| model.forge_action(crate::forge::ForgeAction::New(false), cx),
        ));
    for tab in AppTab::RAIL_ORDER {
        rail = rail.child(crate::widgets::rail_divider());
        for view in tab.sub_views().iter().copied() {
            let badge = match view {
                AppView::Pulsations => Some(badges.engine_awaiting_hitl),
                AppView::Agents => Some(badges.agent_failed_runs),
                AppView::Chat => Some(badges.companion_streaming),
                _ => None,
            };
            rail = rail.child(rail_item(view, active, badge, cx));
        }
    }
    rail.child(div().flex_1())
        .child(crate::widgets::icon_button(
            "rail-settings",
            Icon::Settings,
            crate::widgets::Tooltip::new("Settings").with_shortcut("ctrl-,"),
            settings_open,
            None,
            true,
            cx,
            |model, _, cx| model.open_settings(None, cx),
        ))
}

fn rail_item(
    view: AppView,
    active: AppView,
    badge: Option<usize>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let tooltip = match rail_shortcut(view) {
        Some(shortcut) => crate::widgets::Tooltip::new(view.short_label()).with_shortcut(shortcut),
        None => crate::widgets::Tooltip::new(view.short_label()),
    };
    crate::widgets::icon_button(
        SharedString::from(format!("rail-nav-{}", view.id())),
        view.icon(),
        tooltip,
        active == view,
        badge,
        true,
        cx,
        move |model, _, cx| model.select_view(view, cx),
    )
}

fn content_panel(
    model: &AppModel,
    active: AppView,
    window: &Window,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    match active {
        // Settings is a modal, never the active view; `select_view` guarantees it.
        AppView::Home | AppView::Settings => crate::home_view::home_panel(model, window, cx),
        AppView::Forge => crate::forge_view::forge_panel(model, cx),
        AppView::Workspace => workspace_content_panel(model, cx).into_any_element(),
        AppView::Pulsations => pulsations_panel(model, cx),
        AppView::Vault => vault_panel(model, cx),
        AppView::Chat => chat_panel(model, cx),
        AppView::Agents => agents_panel(model, cx),
        AppView::Logs => logs_panel(model, cx),
    }
}

/// A status-bar segment that jumps to `target` on click. Each segment is independently clickable
/// (see the UI flow plan Part 4.4) — the status bar is another place the next relevant view is one
/// click away, not just a passive readout.
fn status_segment(
    id: &'static str,
    color: gpui::Rgba,
    glyph: Option<Icon>,
    content: impl IntoElement,
    target: FlowTarget,
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
        .on_click(
            cx.listener(move |model, _, window, cx| {
                model.run_next_step(target.clone(), window, cx)
            }),
        )
        .when_some(glyph, |segment, glyph| {
            segment.child(crate::widgets::icon(glyph, px(12.0), color))
        })
        .child(content)
        .into_any_element()
}

/// Passive footer readout in the monospace column on the right.
fn status_readout(
    id: &'static str,
    glyph: Option<Icon>,
    text: impl Into<SharedString>,
) -> AnyElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .gap_1()
        .font_family(theme::FONT_MONO)
        .text_color(theme::text_muted())
        .when_some(glyph, |readout, glyph| {
            readout.child(crate::widgets::icon(glyph, px(12.0), theme::text_muted()))
        })
        .child(text.into())
        .into_any_element()
}

/// Compact token count for the footer (`18.4k`, `2.1M`).
pub(crate) fn compact_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn status_bar(model: &AppModel, badges: TabBadges, cx: &mut Context<AppModel>) -> impl IntoElement {
    let status = model.status().clone();
    let event_count = model.event_count();
    let needs_resync = model.needs_resync();
    let hitl_count = badges.engine_awaiting_hitl;
    // `Idle` means the backlog is caught up — nothing to show, fall back to "System ready"
    // below instead of a chip that would otherwise linger for the rest of the session.
    let indexing = model
        .indexing_status()
        .filter(|event| !matches!(event, IndexingEvent::Idle { .. }))
        .cloned();
    let model_pull = match model.model_pull_status() {
        Some(ModelPullEvent::Started { .. }) => Some("Pulling embedding model".to_string()),
        Some(ModelPullEvent::Progress {
            completed, total, ..
        }) if *total > 0 => Some(format!(
            "Pulling model {}%",
            completed.saturating_mul(100) / total
        )),
        Some(ModelPullEvent::Progress { .. }) => Some("Pulling embedding model".to_string()),
        _ => None,
    };
    // The active pulsation's tokens (global usage lives in Settings > Usage).
    let tokens = model
        .pulsations
        .read(cx)
        .usage
        .summary
        .as_ref()
        .and_then(|summary| summary.totals.total_tokens)
        .map(|tokens| tokens.max(0) as u64);
    let clock = model.footer_clock().clone();
    div()
        .id("status-bar")
        .debug_selector(|| "status-bar".into())
        .h(STATUS_BAR_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .bg(theme::canvas())
        .text_xs()
        .child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::widgets::status_dot(if needs_resync {
                    theme::danger()
                } else {
                    theme::success()
                }))
                .child(
                    div()
                        .min_w_0()
                        .text_ellipsis()
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
                        Some(Icon::TriangleAlert),
                        format!("{hitl_count} pending review(s)"),
                        FlowTarget::View(AppView::Pulsations),
                        cx,
                    ))
                })
                .when_some(indexing.as_ref(), |seg, event| {
                    let (color, glyph, label) = match event {
                        IndexingEvent::Progress { remaining, .. } => (
                            theme::accent(),
                            Icon::Database,
                            format!("Indexing knowledge: {remaining} remaining"),
                        ),
                        IndexingEvent::Paused { remaining, .. } => (
                            theme::text_muted(),
                            Icon::Pause,
                            format!("Indexing paused ({remaining}) — foreground priority"),
                        ),
                        IndexingEvent::Failed { .. } => (
                            theme::danger(),
                            Icon::TriangleAlert,
                            "Embedding failed".to_string(),
                        ),
                        IndexingEvent::Idle { .. } => unreachable!("filtered out above"),
                    };
                    seg.child(status_segment(
                        "status-indexing",
                        color,
                        Some(glyph),
                        label,
                        FlowTarget::Settings(SettingsPage::Embeddings),
                        cx,
                    ))
                })
                .when_some(model_pull, |seg, label| {
                    seg.child(status_segment(
                        "status-model-pull",
                        theme::accent(),
                        Some(Icon::Download),
                        label,
                        FlowTarget::Settings(SettingsPage::Embeddings),
                        cx,
                    ))
                })
                // What is running lives in the header's centre readout; the footer only says
                // when nothing else needs saying.
                .when(hitl_count == 0 && indexing.is_none(), |seg| {
                    seg.child(div().text_color(theme::text_subtle()).child("System ready"))
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .when_some(tokens, |right, tokens| {
                    right.child(status_readout(
                        "status-tokens",
                        Some(Icon::Zap),
                        format!("{} tok", compact_count(tokens)),
                    ))
                })
                .child(status_segment(
                    "status-events",
                    theme::text_muted(),
                    None,
                    format!("{event_count} event(s)"),
                    FlowTarget::View(AppView::Logs),
                    cx,
                ))
                .child(status_readout(
                    "status-version",
                    None,
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                ))
                .child(status_readout("status-clock", Some(Icon::Clock), clock)),
        )
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
            model.toggle_context_rail(cx);
            model.select_view(AppView::Logs, cx);
        });

        cx.read(|cx| {
            let model = model.read(cx);
            assert_eq!(model.active_view(), AppView::Logs);
            assert!(model.context_rail_collapsed(cx));
            let badges = model.sidebar_badges(cx);
            assert_eq!(badges.engine_awaiting_hitl, 2);
            assert_eq!(badges.engine_running_jobs, 3);
        });
    }

    #[gpui::test]
    fn indexing_status_reflects_the_latest_applied_event(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);

        assert!(cx.read(|cx| model.read(cx).indexing_status().is_none()));

        model.update(cx, |model, _| {
            model.indexing_status = Some(IndexingEvent::Progress {
                workspace_path: "C:\\vault".into(),
                remaining: 3,
            });
        });
        cx.read(|cx| {
            assert!(matches!(
                model.read(cx).indexing_status(),
                Some(IndexingEvent::Progress { remaining: 3, .. })
            ));
        });

        model.update(cx, |model, _| {
            model.indexing_status = Some(IndexingEvent::Failed {
                workspace_path: "C:\\vault".into(),
                error: "Ollama unreachable".into(),
            });
        });
        cx.read(|cx| {
            assert!(matches!(
                model.read(cx).indexing_status(),
                Some(IndexingEvent::Failed { .. })
            ));
        });
    }

    #[gpui::test]
    fn model_pull_status_reflects_the_latest_applied_event(cx: &mut TestAppContext) {
        let model = cx.new(AppModel::new_for_test);

        assert!(cx.read(|cx| model.read(cx).model_pull_status().is_none()));

        model.update(cx, |model, _| {
            model.model_pull_status = Some(ModelPullEvent::Progress {
                model: "nomic-embed-text".into(),
                status: "downloading".into(),
                completed: 50,
                total: 100,
            });
        });
        cx.read(|cx| {
            assert!(matches!(
                model.read(cx).model_pull_status(),
                Some(ModelPullEvent::Progress {
                    completed: 50,
                    total: 100,
                    ..
                })
            ));
        });

        model.update(cx, |model, _| {
            model.model_pull_status = Some(ModelPullEvent::Completed {
                model: "nomic-embed-text".into(),
            });
        });
        cx.read(|cx| {
            assert!(matches!(
                model.read(cx).model_pull_status(),
                Some(ModelPullEvent::Completed { .. })
            ));
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
                "Workspace saved"
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
                "Runtime failure"
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
