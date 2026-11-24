use std::sync::Arc;

use anyhow::Context as _;
use gpui::{
    AnyElement, App, Bounds, Context, Focusable, IntoElement, KeyBinding, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use ikore_core::AppCore;
use ikore_core::embedding_service::ModelPullEvent;
use ikore_core::orchestration::IndexingEvent;

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
        SelectNotesView,
        SelectCanvasView,
        SelectWorkspaceView,
        SelectSettingsView,
        PreviousSection,
        NextSection,
        RunPrimaryFlowStep,
        ToggleContextRail,
        ToggleCompanionDock,
        ToggleAgentDock,
        ToggleProjectsSidebar,
        SearchProject,
        PreviousChatQuestion,
        NextChatQuestion,
        FocusNextControl,
        FocusPreviousControl
    ]
);

/// Nav rail order, top to bottom: Home, then each `AppTab::RAIL_ORDER` group. The rail itself is
/// built from the groups; tests walk it through this list.
#[cfg(test)]
pub(crate) const RAIL_VIEWS: [AppView; 10] = [
    AppView::Home,
    AppView::Forge,
    AppView::Pulsations,
    AppView::Vault,
    AppView::Agents,
    AppView::Logs,
    AppView::Chat,
    AppView::Notes,
    AppView::Canvas,
    AppView::Workspace,
];

/// `ctrl-N` per view. Explicit rather than the rail position: a view added to
/// a group (Notes, ADR-029) takes the next free number instead of moving the
/// shortcuts developers already know.
pub(crate) const RAIL_SHORTCUTS: [(AppView, &str); 10] = [
    (AppView::Home, "ctrl-1"),
    (AppView::Forge, "ctrl-2"),
    (AppView::Pulsations, "ctrl-3"),
    (AppView::Vault, "ctrl-4"),
    (AppView::Agents, "ctrl-5"),
    (AppView::Logs, "ctrl-6"),
    (AppView::Chat, "ctrl-7"),
    (AppView::Workspace, "ctrl-8"),
    (AppView::Notes, "ctrl-9"),
    (AppView::Canvas, "ctrl-0"),
];

pub(crate) fn register_navigation_keybindings(cx: &mut App) {
    let mapper = cx.keyboard_mapper().clone();
    cx.bind_keys(section_keybindings(mapper.as_ref()));
    cx.bind_keys([
        KeyBinding::new("ctrl-1", SelectHomeView, None),
        KeyBinding::new("ctrl-2", SelectForgeView, None),
        KeyBinding::new("ctrl-3", SelectPulsationsView, None),
        KeyBinding::new("ctrl-4", SelectVaultView, None),
        KeyBinding::new("ctrl-5", SelectAgentsView, None),
        KeyBinding::new("ctrl-6", SelectLogsView, None),
        KeyBinding::new("ctrl-7", SelectChatView, None),
        KeyBinding::new("ctrl-8", SelectWorkspaceView, None),
        KeyBinding::new("ctrl-9", SelectNotesView, None),
        KeyBinding::new("ctrl-0", SelectCanvasView, None),
        KeyBinding::new("ctrl-,", SelectSettingsView, None),
        // Runs the flow engine's primary suggested next step.
        KeyBinding::new(
            "ctrl-enter",
            RunPrimaryFlowStep,
            Some("Shell && !CompanionDock && !NativeTextInput"),
        ),
        KeyBinding::new("ctrl-shift-c", ToggleCompanionDock, None),
        // The bottom agent console, from any view and any focus (ADR-040 M3).
        KeyBinding::new("ctrl-j", ToggleAgentDock, None),
        // Walks the questions of the open conversation, from the composer too.
        KeyBinding::new("alt-up", PreviousChatQuestion, None),
        KeyBinding::new("alt-down", NextChatQuestion, None),
        KeyBinding::new("ctrl-b", ToggleContextRail, None),
        KeyBinding::new("ctrl-shift-e", ToggleProjectsSidebar, None),
        // The work panel's Search tab, from any view (ADR-042).
        KeyBinding::new("ctrl-shift-f", SearchProject, None),
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

/// `ctrl-shift-[` / `ctrl-shift-]` cycle the current view's own section strip
/// (Pulsations/Forge/Settings) or the agent detail's tabs (Agents). They go through the
/// keyboard layout: on Windows GPUI reads Shift plus a punctuation key as the shifted
/// character with Shift consumed (`ctrl-}`), so the bindings as written never matched there.
fn section_keybindings(mapper: &dyn gpui::PlatformKeyboardMapper) -> [KeyBinding; 2] {
    let binding = |keys: &str, action: Box<dyn gpui::Action>| {
        KeyBinding::load(keys, action, None, false, None, mapper)
            .expect("the section shortcuts are valid keystrokes")
    };
    [
        binding("ctrl-shift-[", Box::new(PreviousSection)),
        binding("ctrl-shift-]", Box::new(NextSection)),
    ]
}

/// Keyboard hint shown in a rail tooltip for `view`.
fn rail_shortcut(view: AppView) -> Option<String> {
    RAIL_SHORTCUTS
        .iter()
        .find(|(candidate, _)| *candidate == view)
        .map(|(_, keys)| (*keys).to_string())
}

pub fn open_main_window(core: Arc<AppCore>, cx: &mut App) -> anyhow::Result<()> {
    register_editor_keybindings(cx);
    register_text_input_keybindings(cx);
    register_hitl_keybindings(cx);
    crate::failure_checkpoint::register_failure_keybindings(cx);
    register_navigation_keybindings(cx);
    crate::canvas::register_canvas_keybindings(cx);
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
                    title: Some("Ikore".into()),
                    ..Default::default()
                }),
                app_id: Some(ikore_core::app_names::APP_IDENTIFIER.into()),
                window_min_size: Some(size(px(960.0), px(640.0))),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| AppModel::new(core, cx)),
        )
        .context("GPUI failed to create main window")?;
    let _ = handle.update(cx, |_model, window, cx| {
        let model = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            model
                .update(cx, |model, cx| model.request_canvas_close(window, cx))
                .unwrap_or(true)
        });
        crate::title_bar::paint(window);
        window.activate_window();
    });
    cx.activate(true);
    Ok(())
}

fn initial_window_size() -> (f32, f32) {
    if ikore_contracts::var_os_with_legacy("BENCHMARK_UI_REPORT").is_some()
        && let Ok(size_str) = ikore_contracts::var_with_legacy("BENCHMARK_UI_WINDOW_SIZE")
        && let Some(size) = parse_qa_window_size(&size_str)
    {
        return size;
    }
    #[cfg(debug_assertions)]
    if let Ok(size_str) = ikore_contracts::var_with_legacy("QA_WINDOW_SIZE")
        && let Some(size) = parse_qa_window_size(&size_str)
    {
        return size;
    }
    (1280.0, 820.0)
}

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
        if !self.shell_focus_claimed {
            self.shell_focus_claimed = true;
            if window.focused(cx).is_none() {
                self.shell_focus.focus(window);
            }
        }
        self.observe_canvas_focus(window, cx);
        self.observe_performance_frame(window, cx);
        self.note_window_activation(window.is_window_active(), cx);
        let active = self.active_view();
        let badges = self.sidebar_badges(cx);
        self.collect_notifications(cx);
        self.observe_companion_dock(cx);
        self.observe_companion_dock_focus(window, cx);
        let projects_trigger = self.observe_projects_sidebar_focus(window, cx);
        self.observe_work_panel_focus(window, cx);
        let unread = self.notifications.queue.unread();
        // Rendered before the centre view: it records the height the Companion keeps clear.
        let agent_dock = crate::agent_dock_view::dock(self, window, cx);
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
            // Where the keyboard starts: zero-size, so no click ever lands on it.
            .child(div().absolute().size_0().track_focus(&self.shell_focus))
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
            .on_action(cx.listener(|model, _: &SelectNotesView, _, cx| {
                model.select_view(AppView::Notes, cx);
            }))
            .on_action(cx.listener(|model, _: &SelectCanvasView, _, cx| {
                model.select_view(AppView::Canvas, cx);
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
                if model.active_view() == AppView::Canvas && model.canvas.editing_label.is_some() {
                    model.commit_canvas_label(cx);
                    if model.canvas.editing_label.is_none() {
                        model.canvas.focus.focus(window);
                    }
                    cx.stop_propagation();
                    return;
                }
                if let Some(step) = crate::flow::next_steps(model, cx).into_iter().next() {
                    model.run_next_step(step.target, window, cx);
                }
            }))
            .on_action(cx.listener(|model, _: &PreviousChatQuestion, _, cx| {
                if model.active_view() == AppView::Chat && !model.settings_modal.open {
                    model.step_chat_question(false, cx);
                }
            }))
            .on_action(cx.listener(|model, _: &NextChatQuestion, _, cx| {
                if model.active_view() == AppView::Chat && !model.settings_modal.open {
                    model.step_chat_question(true, cx);
                }
            }))
            .on_action(cx.listener(|model, _: &ToggleProjectsSidebar, _, cx| {
                model.toggle_projects_sidebar(cx);
            }))
            .on_action(cx.listener(|model, _: &SearchProject, window, cx| {
                model.open_work_panel_tab(crate::work_panel::WorkPanelTab::Search, window, cx);
            }))
            .on_action(cx.listener(|model, _: &ToggleContextRail, _, cx| {
                model.toggle_context_rail(cx);
            }))
            .on_action(cx.listener(|model, _: &ToggleCompanionDock, _, cx| {
                model.toggle_companion_dock(cx);
            }))
            .on_action(cx.listener(|model, _: &ToggleAgentDock, window, cx| {
                model.toggle_agent_dock(AppModel::shell_body_height(window), cx);
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
                    .child(nav_rail(
                        active,
                        badges,
                        self.settings_modal.open,
                        (self.projects_sidebar.open, projects_trigger),
                        cx,
                    ))
                    .when_some(
                        crate::projects_sidebar::projects_sidebar(self, window, cx),
                        |body, sidebar| body.child(sidebar),
                    )
                    .child(
                        // The one framed surface: the centre view sits in a raised container
                        // on the canvas; the rails, header and footer around it are frameless.
                        // The agent dock (ADR-040 M3) sits under the view in this row's bottom
                        // padding, placed absolutely: as a column, the row would change how the
                        // views measure their text (see `chat_view::centred`).
                        div()
                            .id("shell-center")
                            .debug_selector(|| "shell-center".into())
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .pb(px(8.0).max(self.agent_dock_inset()))
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
                                    .child(content_panel(self, active, window, cx)),
                            )
                            .children(agent_dock),
                    )
                    .when_some(
                        crate::context_rail::context_rail(self, window, cx),
                        |body, rail| body.child(rail),
                    ),
            )
            .child(status_bar(self, badges, cx))
            .child(self.notification_overlay(window, cx))
            .child(crate::companion_dock_view::dock(self, window, cx))
            .children(crate::quota_bar::popover(self, window, cx))
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
    let now_busy = !model.pulsations.read(cx).active_jobs.is_empty();
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
                        .child(crate::animated_icon::status_glyph(
                            "brand-sphere",
                            crate::animated_icon::StatusGlyph::Brand { busy: now_busy },
                            px(14.0),
                            theme::accent(),
                        ))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("IKORE"),
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
                .children(crate::quota_bar::header_pill(model, cx))
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
    let (now, flagged) = {
        let pulsations = model.pulsations.read(cx);
        let agents = model.agents.read(cx);
        // ADR-035 M2: the selected pulsation's worst layer, when one needs a look.
        let flagged = pulsations
            .detail()
            .and_then(|detail| detail.layers.as_ref())
            .and_then(|layers| layers.worst_flagged())
            .map(|layer| (layer.state, layer.kind.title()));
        (
            crate::pipeline_status::NowRunning::derive(
                pulsations,
                agents.snapshot.as_ref(),
                chrono::Local::now(),
            ),
            flagged,
        )
    };
    let mut tooltip = match &now.also {
        Some(also) => format!("{} — also running: {also}", now.text),
        None => now.text.clone(),
    };
    if let Some((state, layer)) = flagged {
        tooltip = format!("{tooltip} · Layers: {layer} {}", state.label());
    }
    let tooltip = SharedString::from(tooltip);
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
        .child(crate::animated_icon::status_glyph_for_tone(
            "header-context-glyph",
            if now.busy {
                crate::pipeline_status::StatusTone::Active
            } else {
                now.tone
            },
            px(13.0),
            now.dot,
        ))
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
        .when_some(flagged, |pill, (state, _)| {
            pill.child(
                div()
                    .debug_selector(|| "header-layers-dot".into())
                    .flex_shrink_0()
                    .child(crate::widgets::status_dot(
                        crate::layers_view::state_style(state).1,
                    )),
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
            .filter(|cli| cli.state == ikore_core::terminals::CliAvailabilityState::Available)
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
        .child(
            crate::animated_icon::AnimatedIcon::clip(
                "header-health-glyph",
                if needs_resync {
                    crate::animated_icon::AnimClip::StatusError
                } else {
                    crate::animated_icon::AnimClip::HeaderHealth
                },
                crate::animated_icon::PlayMode::HoverOneshot,
                crate::widgets::ICON_SIZE,
                color,
            )
            .hover_pad(px(9.0)),
        )
        .into_any_element()
}

/// The icon-only nav rail: Home and New work on top, the grouped workbench views, and Settings
/// pinned to the bottom. Each item's tooltip carries its `ctrl-N` shortcut.
fn nav_rail(
    active: AppView,
    badges: TabBadges,
    settings_open: bool,
    (projects_open, projects_focus): (bool, gpui::FocusHandle),
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
        ))
        // Every project and its recent work, in a sidebar beside this rail (ADR-042). The button
        // takes the focus of a sidebar that closes.
        .child(
            crate::widgets::icon_button_div(
                "rail-projects",
                if projects_open {
                    Icon::FolderOpen
                } else {
                    Icon::Folder
                },
                crate::widgets::Tooltip::new("Projects and sessions").with_shortcut("ctrl-shift-e"),
                projects_open,
                None,
                true,
                cx,
                |model, _, cx| model.toggle_projects_sidebar(cx),
            )
            .track_focus(&projects_focus),
        );
    for tab in AppTab::RAIL_ORDER {
        rail = rail.child(crate::widgets::rail_divider());
        for view in tab.sub_views().iter().copied() {
            let badge = match view {
                AppView::Pulsations => Some(badges.engine_pending_decisions),
                AppView::Agents => Some(badges.agent_failed_runs),
                AppView::Chat => Some(badges.companion_streaming),
                AppView::Notes => Some(badges.notes_running),
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
    let id = SharedString::from(format!("rail-nav-{}", view.id()));
    if view == AppView::Workspace {
        return crate::widgets::icon_button_clip(
            id,
            view.icon(),
            crate::animated_icon::AnimClip::NavWorkspace,
            tooltip,
            active == view,
            badge,
            true,
            cx,
            move |model, _, cx| model.select_view(view, cx),
        );
    }
    crate::widgets::icon_button(
        id,
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
        AppView::Notes => crate::notes_view::notes_panel(model, cx),
        AppView::Canvas => crate::canvas_view::canvas_panel(model, cx),
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
    let (status_glyph, status_dot_color) =
        crate::animated_icon::status_bar_glyph(&status, needs_resync);
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
                .child(crate::animated_icon::status_glyph(
                    "status-bar-glyph",
                    status_glyph,
                    px(13.0),
                    status_dot_color,
                ))
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
                .when(model.doc_drift.overview.drifted() > 0, |seg| {
                    seg.child(status_segment(
                        "status-doc-drift",
                        theme::warning(),
                        Some(Icon::Zap),
                        ikore_core::doc_reconciliation::indicator_text(
                            model.doc_drift.overview.drifted(),
                        ),
                        FlowTarget::View(AppView::Vault),
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
                .child(agent_console_item(model, hitl_count, cx))
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

/// `● {agent} · {what it does} · {time}`, or `No agent running`, with the
/// count of what needs attention; a click shows or hides the agent console.
/// Nothing opens the dock on its own (ADR-040, 9.5).
fn agent_console_item(
    model: &AppModel,
    hitl_count: usize,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let (color, label, attention) = model.agent_console_status(cx);
    let attention = attention + hitl_count;
    div()
        .id("status-agent-console")
        .debug_selector(|| "status-agent-console".into())
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .tooltip(|_, cx| {
            cx.new(|_| {
                crate::widgets::Tooltip::new("Show or hide the agent console")
                    .with_shortcut("Ctrl+J")
            })
            .into()
        })
        .on_click(cx.listener(|model, _, window, cx| {
            model.toggle_agent_dock(AppModel::shell_body_height(window), cx);
        }))
        .when_some(color, |item, color| {
            item.child(crate::widgets::status_dot(color))
        })
        .child(
            div()
                .max_w(px(320.0))
                .truncate()
                .text_color(theme::text_muted())
                .child(label),
        )
        .when(attention > 0, |item| {
            item.child(
                div()
                    .px_1()
                    .rounded_full()
                    .bg(theme::warning())
                    .text_color(theme::surface())
                    .child(attention.to_string()),
            )
        })
        .into_any_element()
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

    /// Windows reads Ctrl+Shift+] as `ctrl-}`: the section shortcuts must take
    /// the layout's mapping instead of matching only the keystroke as written.
    #[test]
    fn section_shortcuts_match_what_the_keyboard_layout_reads() {
        struct ShiftedPunctuation;
        impl gpui::PlatformKeyboardMapper for ShiftedPunctuation {
            fn map_key_equivalent(
                &self,
                mut keystroke: gpui::Keystroke,
                _use_key_equivalents: bool,
            ) -> gpui::KeybindingKeystroke {
                let shifted = match keystroke.key.as_str() {
                    "[" => Some("{"),
                    "]" => Some("}"),
                    _ => None,
                };
                if let Some(shifted) = shifted.filter(|_| keystroke.modifiers.shift) {
                    keystroke.modifiers.shift = false;
                    keystroke.key = shifted.into();
                }
                gpui::KeybindingKeystroke::from_keystroke(keystroke)
            }

            fn get_key_equivalents(&self) -> Option<&collections::HashMap<char, char>> {
                None
            }
        }

        let typed = |keys: &str| [gpui::Keystroke::parse(keys).expect("keystroke")];
        let [previous, next] = section_keybindings(&ShiftedPunctuation);
        assert_eq!(previous.match_keystrokes(&typed("ctrl-{")), Some(false));
        assert_eq!(next.match_keystrokes(&typed("ctrl-}")), Some(false));
        assert_eq!(next.match_keystrokes(&typed("ctrl-shift-]")), None);

        // Where nothing is remapped (the test platform), the keystroke as written.
        let [_, next] = section_keybindings(&gpui::DummyKeyboardMapper);
        assert_eq!(next.match_keystrokes(&typed("ctrl-shift-]")), Some(false));
    }

    #[gpui::test]
    fn pulsation_rail_counts_pending_failures_without_inflating_review_count(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            model.pulsations.update(cx, |pulsations, _| {
                pulsations.awaiting_hitl = 0;
                pulsations.awaiting_failure = 1;
                pulsations.pulsations[0].status =
                    ikore_core::persistence_service::PulsationStatus::Checkpoint;
            });
        });
        model.read_with(cx, |model, cx| {
            let badges = model.sidebar_badges(cx);
            assert_eq!(badges.engine_awaiting_hitl, 0);
            assert_eq!(badges.engine_pending_decisions, 1);
        });
        model.update(cx, |model, cx| {
            model.pulsations.update(cx, |pulsations, _| {
                pulsations
                    .hitl_checkpoints
                    .push(ikore_core::persistence_service::HitlCheckpoint {
                        pulsation_id: pulsations.pulsations[0].id.clone(),
                        checkpoint_id: "same-pending-pulse".into(),
                        state_version: 1,
                    });
            });
        });
        model.read_with(cx, |model, cx| {
            assert_eq!(model.sidebar_badges(cx).engine_pending_decisions, 1)
        });
    }

    #[gpui::test]
    fn pulsation_rail_counts_projected_description_reviews_and_deduplicates_sources(
        cx: &mut TestAppContext,
    ) {
        use ikore_core::orchestration::decisions::{
            PendingDecision, PendingDecisionAction, PendingDecisionKind,
        };
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |model, cx| {
            let (project, pulse) = model.pulsations.update(cx, |pulsations, _| {
                pulsations.awaiting_hitl = 0;
                pulsations.awaiting_failure = 0;
                pulsations.hitl_checkpoints.clear();
                pulsations.failure_checkpoints.clear();
                pulsations.pulsations[0].status =
                    ikore_core::persistence_service::PulsationStatus::Planning;
                (
                    pulsations.pulsations[0].project_id.clone(),
                    pulsations.pulsations[0].id.clone(),
                )
            });
            model
                .resolution
                .update(cx, |resolution, _| resolution.chips.clear());
            model
                .agents
                .update(cx, |agents, _| agents.fallbacks.clear());
            model.workspace.update(cx, |workspace, _| {
                workspace.project_id = Some(project.clone().into())
            });
            let decision = PendingDecision {
                id: format!("description:{pulse}:1"),
                project_id: project.clone(),
                kind: PendingDecisionKind::DescriptionReview,
                source_id: pulse.clone(),
                pulsation_id: Some(pulse),
                checkpoint_id: None,
                feature_id: None,
                input_revision: Some(1),
                state_version: 1,
                stage: None,
                title: "Review the prepared description".into(),
                description: "Confirm the request before Specification.".into(),
                actions: vec![PendingDecisionAction::OpenFullReview],
                question_set: None,
                forge_questions: None,
            };
            model.companion_dock.pending = vec![decision.clone(), decision];
        });
        model.read_with(cx, |model, cx| {
            let badges = model.sidebar_badges(cx);
            assert_eq!(
                badges.engine_pending_decisions, 1,
                "two sources for the same waiting pulsation count once"
            );
            assert_eq!(
                badges.engine_awaiting_hitl, 0,
                "description review is not artifact review"
            );
        });
        model.update(cx, |model, _| {
            for decision in &mut model.companion_dock.pending {
                decision.project_id = "another-project".into();
            }
        });
        model.read_with(cx, |model, cx| {
            assert_eq!(
                model.sidebar_badges(cx).engine_pending_decisions,
                0,
                "late projection from a different project must not count"
            )
        });
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
