//! Stage verifiers attached after default pipeline stages (feature 25, M4):
//! canvas nodes under Labs, read-only synthesis without Run, history drawer,
//! and create/edit form.

use gpui::{AnyElement, Context, Entity, FontWeight, SharedString, div, prelude::*};
use ikore_core::custom_pipelines::STAGE_VALUES;
use ikore_core::persistence_service::PulsationStage;
use ikore_core::stage_verifiers::{
    DEFAULT_REJECT_LIMIT, MAX_REJECT_LIMIT, MAX_VERIFIERS_PER_PROJECT, MAX_VERIFIERS_PER_STAGE,
    SaveStageVerifierOutcome, StageVerifierDraft, StageVerifierRunView, StageVerifierView,
    StepAgent, StepTransport, verifier_slot_id,
};
use ikore_core::terminals::{CliBackend, ThinkingLevel};

use crate::agents::{AgentDetailTab, AgentIdentifier, AgentsModel, CliModelPickerTarget};
use crate::agents_canvas::layout::stage_column;
use crate::agents_canvas::{CanvasNode, NodeState};
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::session::HeaderNoticeKind;
use crate::text_input::NativeTextInput;
use crate::theme;

use super::{action_button, empty_state, icon_action, run_status};

#[derive(Debug)]
pub(crate) struct VerifierEditor {
    pub(crate) verifier_id: Option<String>,
    pub(crate) stage: String,
    pub(crate) position: i64,
    pub(crate) enabled: bool,
    pub(crate) reject_limit: u8,
    pub(crate) transport: StepTransport,
    pub(crate) backend: CliBackend,
    pub(crate) model: Option<String>,
    pub(crate) model_profile: Option<String>,
    pub(crate) thinking: ThinkingLevel,
    pub(crate) route_id: Option<String>,
    pub(crate) label: Entity<NativeTextInput>,
    pub(crate) instructions: Entity<NativeTextInput>,
    pub(crate) errors: Vec<String>,
    pub(crate) saving: bool,
}

fn input(
    placeholder: &str,
    value: &str,
    multiline: bool,
    cx: &mut Context<AppModel>,
) -> Entity<NativeTextInput> {
    let placeholder = placeholder.to_owned();
    let value = value.to_owned();
    let entity = cx.new(move |cx| {
        let mut field = NativeTextInput::new(placeholder, cx);
        if multiline {
            field = field.multiline().max_rows(8);
        }
        field.set_value(value, cx);
        field
    });
    cx.observe(&entity, |_, _, cx| cx.notify()).detach();
    entity
}

fn starting_model(catalog: &ikore_core::terminals::CliModelCatalogSnapshot) -> Option<String> {
    catalog
        .models
        .iter()
        .find_map(|entry| {
            entry
                .raw_metadata
                .get("configuredModel")
                .and_then(|model| model.as_str())
                .map(str::to_owned)
        })
        .or_else(|| {
            catalog
                .models
                .iter()
                .find_map(|entry| entry.model_id.clone())
        })
}

fn default_route(model: &AppModel, cx: &gpui::App) -> Option<String> {
    model
        .settings
        .read(cx)
        .snapshot
        .as_ref()
        .and_then(|s| s.routes.first())
        .map(|r| r.id.clone())
}

fn stage_label(stage: &str) -> String {
    ikore_core::custom_pipelines::trigger_stage_label(stage).to_owned()
}

fn choice(
    id: String,
    label: impl Into<SharedString>,
    active: bool,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if active {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .text_xs()
        .opacity(if enabled { 1.0 } else { 0.5 })
        .when(enabled, |chip| {
            chip.cursor_pointer()
                .hover(|style| style.bg(theme::hover_bg()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(label.into())
        .into_any_element()
}

fn field_label(text: &'static str) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

pub(super) fn can_create_verifier(agents: &AgentsModel, stage: Option<&str>) -> bool {
    if !agents.verifiers.enabled_by_labs {
        return false;
    }
    if agents.verifiers.verifiers.len() >= MAX_VERIFIERS_PER_PROJECT {
        return false;
    }
    match stage {
        Some(stage) => {
            agents
                .verifiers
                .verifiers
                .iter()
                .filter(|v| v.stage.eq_ignore_ascii_case(stage))
                .count()
                < MAX_VERIFIERS_PER_STAGE
        }
        None => true,
    }
}

fn verifier_node_state(verifier: &StageVerifierView, agents: &AgentsModel) -> (NodeState, String) {
    if !verifier.enabled {
        return (NodeState::Idle, "Disabled".into());
    }
    let latest_slot_run = agents
        .verifier_slot(&verifier.id)
        .and_then(|slot| slot.recent_runs.first());
    if let Some(run) = latest_slot_run {
        let status = match run.status {
            ikore_core::terminals::TerminalRunStatus::Queued => "Queued",
            ikore_core::terminals::TerminalRunStatus::Starting
            | ikore_core::terminals::TerminalRunStatus::Running => "Running",
            ikore_core::terminals::TerminalRunStatus::Completed => "Done",
            ikore_core::terminals::TerminalRunStatus::Failed => "Failed",
            ikore_core::terminals::TerminalRunStatus::AwaitingFallback => "Waiting",
            ikore_core::terminals::TerminalRunStatus::Cancelled
            | ikore_core::terminals::TerminalRunStatus::Interrupted => "Stopped",
        };
        let state = match run.status {
            ikore_core::terminals::TerminalRunStatus::Queued => NodeState::Idle,
            ikore_core::terminals::TerminalRunStatus::Starting
            | ikore_core::terminals::TerminalRunStatus::Running => NodeState::Running,
            ikore_core::terminals::TerminalRunStatus::Completed => NodeState::Done,
            ikore_core::terminals::TerminalRunStatus::AwaitingFallback => NodeState::Waiting,
            ikore_core::terminals::TerminalRunStatus::Failed
            | ikore_core::terminals::TerminalRunStatus::Cancelled
            | ikore_core::terminals::TerminalRunStatus::Interrupted => NodeState::Attention,
        };
        return (state, status.to_owned());
    }
    if let Some(run) = agents
        .verifiers
        .runs
        .iter()
        .find(|r| r.verifier_id == verifier.id)
    {
        return match run.status.as_str() {
            "passed" => (NodeState::Done, "Passed".into()),
            "rejected" => (NodeState::Attention, "Rejected".into()),
            "overridden" => (NodeState::Done, "Overridden".into()),
            "failed" => (NodeState::Attention, "Error".into()),
            "running" => (NodeState::Running, "Running".into()),
            "pending" => (NodeState::Idle, "Queued".into()),
            "skipped" => (NodeState::Done, "Skipped".into()),
            _ => (NodeState::Idle, run.status.clone()),
        };
    }
    let mode = if verifier.is_advisory() {
        "Advisory".to_owned()
    } else {
        format!("Blocking · limit {}", verifier.reject_limit)
    };
    (NodeState::Idle, mode)
}

fn verifier_transport(model: &AppModel, agent: &StepAgent, cx: &gpui::App) -> String {
    match agent.transport {
        StepTransport::Cli => format!("CLI · {}", agent.backend.as_deref().unwrap_or("codex")),
        StepTransport::Api => {
            let route = agent.api_route_id.as_deref().unwrap_or("—");
            let name = model
                .settings
                .read(cx)
                .snapshot
                .as_ref()
                .and_then(|s| s.routes.iter().find(|r| r.id == route))
                .map_or(route.to_owned(), |r| r.name.clone());
            format!("API · {name}")
        }
    }
}

fn stage_role(stage: PulsationStage) -> ikore_core::llm::AgentRole {
    use ikore_core::llm::AgentRole;
    match stage {
        PulsationStage::Intake => AgentRole::IntakeRouter,
        PulsationStage::Specification => AgentRole::TaskSpecifier,
        PulsationStage::Contract => AgentRole::ContractAgent,
        PulsationStage::Code => AgentRole::BackendAgent,
        PulsationStage::Test => AgentRole::TestAgent,
        PulsationStage::Integrator => AgentRole::Integrator,
    }
}

/// The stage verifier nodes rendered beneath default stages on the Agents canvas.
/// Returns empty when Settings › Labs `stage_verifiers_enabled` is Off.
pub(super) fn canvas_verifiers(
    model: &AppModel,
    items: &super::model::AgentItems,
    cx: &gpui::App,
) -> Vec<(usize, CanvasNode)> {
    let agents = model.agents.read(cx);
    if !agents.verifiers.enabled_by_labs {
        return Vec::new();
    }
    let column_of = |stage_raw: &str| {
        let Some(stage) = PulsationStage::from_db_value(stage_raw) else {
            return items.stages.len().saturating_sub(1);
        };
        let role = stage_role(stage);
        items
            .stages
            .iter()
            .position(|item| item.id == AgentIdentifier::Canonical(role))
            .unwrap_or_else(|| stage_column(stage).min(items.stages.len().saturating_sub(1)))
    };
    let mut verifiers = agents.verifiers.verifiers.clone();
    verifiers.sort_by_key(|v| (column_of(&v.stage), v.position, v.id.clone()));
    verifiers
        .into_iter()
        .map(|verifier| {
            let col = column_of(&verifier.stage);
            let (state, status) = verifier_node_state(&verifier, agents);
            (
                col,
                CanvasNode {
                    agent: AgentIdentifier::StageVerifier(verifier.id.clone()),
                    label: verifier.label.clone(),
                    color: theme::purple(),
                    state,
                    status,
                    transport: verifier_transport(model, &verifier.agent, cx),
                    checkpoint: false,
                },
            )
        })
        .collect()
}

pub(super) fn verifier_synthesis(
    model: &AppModel,
    verifier: &StageVerifierView,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let agents = model.agents.read(cx);
    let slot = agents.verifier_slot(&verifier.id);
    let run = slot.and_then(|s| s.recent_runs.first());
    let detail_tab = agents.detail_tab;
    let confirm_delete = agents.pending_verifier_delete.as_deref() == Some(verifier.id.as_str());
    let can_delete = verifier.run_count == 0;
    let mode_text = if verifier.is_advisory() {
        "Advisory (never blocks the stage)".to_owned()
    } else {
        format!(
            "Blocking · automatic reject limit {}",
            verifier.reject_limit
        )
    };
    let transport_text = match verifier.agent.transport {
        StepTransport::Cli => format!(
            "CLI · {} · {}",
            verifier.agent.backend.as_deref().unwrap_or("—"),
            verifier.agent.model.as_deref().unwrap_or("CLI default")
        ),
        StepTransport::Api => verifier_transport(model, &verifier.agent, cx),
    };
    let (status_color_val, status_word) = if !verifier.enabled {
        (theme::text_muted(), "Disabled".to_owned())
    } else if let Some(run) = run {
        let c = match run.status {
            ikore_core::terminals::TerminalRunStatus::Failed => theme::danger(),
            ikore_core::terminals::TerminalRunStatus::AwaitingFallback => theme::warning(),
            ikore_core::terminals::TerminalRunStatus::Running
            | ikore_core::terminals::TerminalRunStatus::Starting => theme::accent(),
            ikore_core::terminals::TerminalRunStatus::Completed => theme::success(),
            _ => theme::text_muted(),
        };
        (c, run_status(run.status).to_owned())
    } else {
        (theme::text(), "Enabled · ready".to_owned())
    };
    let history_runs: Vec<StageVerifierRunView> = agents
        .verifier_runs
        .get(&verifier.id)
        .cloned()
        .unwrap_or_else(|| {
            agents
                .verifiers
                .runs
                .iter()
                .filter(|r| r.verifier_id == verifier.id)
                .cloned()
                .collect()
        });
    let id_edit = verifier.id.clone();
    let id_toggle = verifier.id.clone();
    let id_history = verifier.id.clone();
    let id_arm_del = verifier.id.clone();
    let id_del = verifier.id.clone();
    let next_enabled = !verifier.enabled;

    let mut panel = div()
        .id("verifier-synthesis")
        .debug_selector(|| "verifier-synthesis".into())
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::widgets::status_dot(theme::purple()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(verifier.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(theme::accent_soft())
                        .text_xs()
                        .text_color(theme::accent())
                        .child("Added by you"),
                )
                .child(icon_action(
                    "agents-synthesis-close".into(),
                    Icon::X,
                    "Close",
                    true,
                    cx,
                    |model, cx| model.close_agents_synthesis(cx),
                )),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface_raised())
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(format!("Stage: {} · Runs: {}", stage_label(&verifier.stage), verifier.run_count)),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text())
                        .child(mode_text),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(status_color_val)
                        .child(format!("Status: {status_word} · {transport_text}")),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Verifiers run automatically after the default stage accepts its result. There is no manual Run action."),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface_raised())
                .flex()
                .flex_col()
                .gap_1()
                .child(field_label("Instructions"))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text())
                        .child(verifier.instructions.clone()),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(action_button(
                    "verifier-edit".into(),
                    "Edit".into(),
                    !busy,
                    false,
                    cx,
                    move |model, cx| model.edit_stage_verifier(id_edit.clone(), cx),
                ))
                .child(action_button(
                    "verifier-toggle-enabled".into(),
                    if verifier.enabled { "Disable" } else { "Enable" }.into(),
                    !busy,
                    false,
                    cx,
                    move |model, cx| {
                        model.toggle_stage_verifier_enabled(id_toggle.clone(), cx);
                    },
                ))
                .child(action_button(
                    "verifier-history".into(),
                    if detail_tab == AgentDetailTab::History {
                        "Hide history"
                    } else {
                        "History"
                    }
                    .into(),
                    !busy,
                    detail_tab == AgentDetailTab::History,
                    cx,
                    move |model, cx| model.toggle_verifier_history(id_history.clone(), cx),
                ))
                .when(can_delete && !confirm_delete, |row| {
                    row.child(action_button(
                        "verifier-delete".into(),
                        "Delete".into(),
                        !busy,
                        false,
                        cx,
                        move |model, cx| model.arm_delete_stage_verifier(id_arm_del.clone(), cx),
                    ))
                })
                .when(can_delete && confirm_delete, |row| {
                    row.child(crate::widgets::btn(
                        "verifier-delete-confirm",
                        "Confirm delete",
                        crate::widgets::ButtonVariant::Danger,
                        !busy,
                        cx,
                        move |model, cx| model.confirm_delete_stage_verifier(id_del.clone(), cx),
                    ))
                })
                .when(!can_delete, |row| {
                    row.child(
                        div()
                            .id("verifier-delete-disabled")
                            .debug_selector(|| "verifier-delete-disabled".into())
                            .flex_none()
                            .tooltip(|_, cx| {
                                cx.new(|_| {
                                    crate::widgets::Tooltip::new(
                                        "Verifiers with run history cannot be deleted; disable it instead.",
                                    )
                                })
                                .into()
                            })
                            .child(action_button(
                                "verifier-delete".into(),
                                "Delete".into(),
                                false,
                                false,
                                cx,
                                |_, _| {},
                            )),
                    )
                }),
        );

    let _ = next_enabled;
    if detail_tab == AgentDetailTab::History {
        panel = panel.child(verifier_history_drawer(&history_runs));
    }
    panel.into_any_element()
}

fn verifier_history_drawer(runs: &[StageVerifierRunView]) -> AnyElement {
    let mut box_div = div()
        .id("agents-drawer:verifier-history")
        .debug_selector(|| "agents-drawer:verifier-history".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .child(field_label("Verification run history"));
    if runs.is_empty() {
        box_div = box_div.child(empty_state("No verifier runs yet."));
        return box_div.into_any_element();
    }
    for (idx, run) in runs.iter().take(20).enumerate() {
        let mut item = div()
            .id(SharedString::from(format!("verifier-history-row-{idx}")))
            .w_full()
            .min_w_0()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::canvas())
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!(
                        "{} · attempt {} · gen {} · {}",
                        run.status, run.attempt, run.stage_generation, run.created_at
                    )),
            );
        if let Some(error) = run.error.as_deref().filter(|s| !s.trim().is_empty()) {
            item = item.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme::danger())
                    .child(error.to_owned()),
            );
        }
        if let Some(verdict) = &run.verdict
            && !verdict.summary.trim().is_empty()
        {
            item = item.child(
                div()
                    .w_full()
                    .min_w_0()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(verdict.summary.clone()),
            );
        }
        box_div = box_div.child(item);
    }
    box_div.into_any_element()
}

pub(super) fn verifier_editor_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some(editor) = model.verifier_editor.as_ref() else {
        return div().into_any_element();
    };
    let editing = editor.verifier_id.is_some();
    let title = if editing {
        "Edit stage verifier"
    } else {
        "New stage verifier"
    };
    let saving = editor.saving;
    let enabled = !saving;
    let stage = editor.stage.clone();
    let advisory = editor.reject_limit == 0;
    let reject_limit = editor.reject_limit;
    let transport = editor.transport;
    let backend = editor.backend;
    let model_label = editor.model.as_deref().unwrap_or("CLI default").to_owned();
    let thinking = editor.thinking;
    let route_id = editor.route_id.clone();
    let routes = model
        .settings
        .read(cx)
        .snapshot
        .as_ref()
        .map(|s| s.routes.clone())
        .unwrap_or_default();

    div()
        .id("verifier-editor")
        .debug_selector(|| "verifier-editor".into())
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(icon_action(
                    "verifier-editor-close".into(),
                    Icon::X,
                    "Close",
                    enabled,
                    cx,
                    |model, cx| model.close_verifier_editor(cx),
                )),
        )
        .child(field_label("Label"))
        .child(editor.label.clone())
        .child(field_label("Target default stage"))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(STAGE_VALUES.iter().map(|candidate| {
                    let candidate_str = (*candidate).to_owned();
                    let active = stage.eq_ignore_ascii_case(candidate);
                    choice(
                        format!("verifier-stage-{}", candidate.to_lowercase()),
                        stage_label(candidate),
                        active,
                        enabled && !editing,
                        cx,
                        move |model, cx| {
                            model.set_verifier_editor_stage(candidate_str.clone(), cx);
                        },
                    )
                })),
        )
        .child(field_label("Mode"))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(choice(
                    "verifier-mode-blocking".into(),
                    "Blocking",
                    !advisory,
                    enabled,
                    cx,
                    |model, cx| {
                        let limit =
                            model
                                .verifier_editor
                                .as_ref()
                                .map_or(DEFAULT_REJECT_LIMIT, |e| {
                                    if e.reject_limit == 0 {
                                        DEFAULT_REJECT_LIMIT
                                    } else {
                                        e.reject_limit
                                    }
                                });
                        model.set_verifier_editor_reject_limit(limit, cx);
                    },
                ))
                .child(choice(
                    "verifier-mode-advisory".into(),
                    "Advisory",
                    advisory,
                    enabled,
                    cx,
                    |model, cx| model.set_verifier_editor_reject_limit(0, cx),
                )),
        )
        .child(field_label("Automatic rejection limit (0 = advisory)"))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_1()
                .children((0..=MAX_REJECT_LIMIT).map(|limit| {
                    choice(
                        format!("verifier-limit-{limit}"),
                        limit.to_string(),
                        reject_limit == limit,
                        enabled,
                        cx,
                        move |model, cx| model.set_verifier_editor_reject_limit(limit, cx),
                    )
                })),
        )
        .child(field_label("Transport"))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_1()
                .child(choice(
                    "verifier-transport-cli".into(),
                    "CLI",
                    transport == StepTransport::Cli,
                    enabled,
                    cx,
                    |model, cx| model.set_verifier_editor_transport(StepTransport::Cli, cx),
                ))
                .child(choice(
                    "verifier-transport-api".into(),
                    "API",
                    transport == StepTransport::Api,
                    enabled,
                    cx,
                    |model, cx| model.set_verifier_editor_transport(StepTransport::Api, cx),
                )),
        )
        .when(transport == StepTransport::Cli, |form| {
            form.child(field_label("CLI backend"))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(
                            CliBackend::ALL
                                .into_iter()
                                .filter(|b| *b != CliBackend::Custom)
                                .map(|b| {
                                    choice(
                                        format!("verifier-backend-{}", b.as_str()),
                                        b.label(),
                                        backend == b,
                                        enabled,
                                        cx,
                                        move |model, cx| model.set_verifier_editor_backend(b, cx),
                                    )
                                }),
                        ),
                )
                .child(field_label("Model"))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme::text())
                                .child(model_label),
                        )
                        .child(action_button(
                            "verifier-pick-model".into(),
                            "Choose model…".into(),
                            enabled,
                            false,
                            cx,
                            |model, cx| model.open_verifier_model_picker(cx),
                        )),
                )
                .child(field_label("Thinking level"))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(
                            [
                                ThinkingLevel::Default,
                                ThinkingLevel::Low,
                                ThinkingLevel::Medium,
                                ThinkingLevel::High,
                            ]
                            .into_iter()
                            .map(|level| {
                                choice(
                                    format!("verifier-thinking-{}", level.as_str()),
                                    level.as_str().to_owned(),
                                    thinking == level,
                                    enabled,
                                    cx,
                                    move |model, cx| model.set_verifier_editor_thinking(level, cx),
                                )
                            }),
                        ),
                )
        })
        .when(transport == StepTransport::Api, |form| {
            form.child(field_label("API route"))
                .when(routes.is_empty(), |box_div| {
                    box_div.child(empty_state(
                        "Configure an API route in Settings › Connections first.",
                    ))
                })
                .when(!routes.is_empty(), |box_div| {
                    box_div.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(routes.into_iter().map(|route| {
                                let rid = route.id.clone();
                                let active = route_id.as_deref() == Some(rid.as_str());
                                choice(
                                    format!("verifier-route-{}", route.id),
                                    route.name,
                                    active,
                                    enabled,
                                    cx,
                                    move |model, cx| {
                                        model.set_verifier_editor_route(rid.clone(), cx);
                                    },
                                )
                            })),
                    )
                })
        })
        .child(field_label("Verification instructions"))
        .child(crate::sizing::text_area(editor.instructions.clone()))
        .when(!editor.errors.is_empty(), |form| {
            form.child(
                div()
                    .debug_selector(|| "verifier-editor-errors".into())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(theme::danger())
                    .bg(theme::danger_soft())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(editor.errors.iter().map(|err| {
                        div()
                            .w_full()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme::danger())
                            .child(err.clone())
                    })),
            )
        })
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::widgets::btn(
                    "verifier-editor-save",
                    if saving { "Saving…" } else { "Save verifier" },
                    crate::widgets::ButtonVariant::Primary,
                    enabled,
                    cx,
                    |model, cx| model.save_verifier_editor(cx),
                ))
                .child(crate::widgets::btn(
                    "verifier-editor-cancel",
                    "Cancel",
                    crate::widgets::ButtonVariant::Secondary,
                    enabled,
                    cx,
                    |model, cx| model.close_verifier_editor(cx),
                )),
        )
        .into_any_element()
}

impl AppModel {
    pub(crate) fn open_new_verifier(&mut self, stage: Option<&str>, cx: &mut Context<Self>) {
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.show_archived = false;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.active_model_picker = None;
        });
        let backend = CliBackend::Codex;
        let model = self
            .agents
            .read(cx)
            .model_catalogs
            .get(&backend)
            .and_then(starting_model);
        let stage = stage
            .filter(|s| {
                STAGE_VALUES
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(s))
            })
            .map(str::to_ascii_uppercase)
            .unwrap_or_else(|| "CODE".to_owned());
        let position = self
            .agents
            .read(cx)
            .verifiers
            .verifiers
            .iter()
            .filter(|v| v.stage.eq_ignore_ascii_case(&stage))
            .map(|v| v.position + 1)
            .max()
            .unwrap_or(0);
        self.verifier_editor = Some(VerifierEditor {
            verifier_id: None,
            stage,
            position,
            enabled: true,
            reject_limit: DEFAULT_REJECT_LIMIT,
            transport: StepTransport::Cli,
            backend,
            model,
            model_profile: None,
            thinking: ThinkingLevel::Default,
            route_id: default_route(self, cx),
            label: input("Verifier name", "", false, cx),
            instructions: input(
                "What this verifier should check before accepting the stage output",
                "",
                true,
                cx,
            ),
            errors: Vec::new(),
            saving: false,
        });
        if !self.agents.read(cx).model_catalogs.contains_key(&backend) {
            self.load_cli_model_catalog(backend, false, cx);
        }
        cx.notify();
    }

    pub(crate) fn edit_stage_verifier(&mut self, verifier_id: String, cx: &mut Context<Self>) {
        let Some(verifier) = self.agents.read(cx).verifier(&verifier_id).cloned() else {
            return;
        };
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.show_archived = false;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.active_model_picker = None;
        });
        let backend = verifier
            .agent
            .backend
            .as_deref()
            .and_then(CliBackend::parse)
            .unwrap_or(CliBackend::Codex);
        let thinking =
            ThinkingLevel::parse(&verifier.agent.thinking_level).unwrap_or(ThinkingLevel::Default);
        self.verifier_editor = Some(VerifierEditor {
            verifier_id: Some(verifier.id),
            stage: verifier.stage,
            position: verifier.position,
            enabled: verifier.enabled,
            reject_limit: verifier.reject_limit,
            transport: verifier.agent.transport,
            backend,
            model: verifier.agent.model,
            model_profile: verifier.agent.model_profile,
            thinking,
            route_id: verifier
                .agent
                .api_route_id
                .or_else(|| default_route(self, cx)),
            label: input("Verifier name", &verifier.label, false, cx),
            instructions: input(
                "What this verifier should check before accepting the stage output",
                &verifier.instructions,
                true,
                cx,
            ),
            errors: Vec::new(),
            saving: false,
        });
        cx.notify();
    }

    pub(crate) fn close_verifier_editor(&mut self, cx: &mut Context<Self>) {
        self.verifier_editor = None;
        cx.notify();
    }

    pub(crate) fn set_verifier_editor_stage(&mut self, stage: String, cx: &mut Context<Self>) {
        let next_pos = self
            .agents
            .read(cx)
            .verifiers
            .verifiers
            .iter()
            .filter(|v| v.stage.eq_ignore_ascii_case(&stage))
            .map(|v| v.position + 1)
            .max()
            .unwrap_or(0);
        if let Some(editor) = self.verifier_editor.as_mut()
            && editor.verifier_id.is_none()
        {
            editor.stage = stage;
            editor.position = next_pos;
            cx.notify();
        }
    }

    pub(crate) fn set_verifier_editor_reject_limit(&mut self, limit: u8, cx: &mut Context<Self>) {
        if let Some(editor) = self.verifier_editor.as_mut() {
            editor.reject_limit = limit.min(MAX_REJECT_LIMIT);
            cx.notify();
        }
    }

    pub(crate) fn set_verifier_editor_transport(
        &mut self,
        transport: StepTransport,
        cx: &mut Context<Self>,
    ) {
        let fallback_route = default_route(self, cx);
        if let Some(editor) = self.verifier_editor.as_mut() {
            editor.transport = transport;
            if transport == StepTransport::Api && editor.route_id.is_none() {
                editor.route_id = fallback_route;
            }
            cx.notify();
        }
    }

    pub(crate) fn set_verifier_editor_backend(
        &mut self,
        backend: CliBackend,
        cx: &mut Context<Self>,
    ) {
        let prefill = self
            .agents
            .read(cx)
            .model_catalogs
            .get(&backend)
            .and_then(starting_model);
        if let Some(editor) = self.verifier_editor.as_mut() {
            editor.backend = backend;
            editor.model = prefill;
            editor.model_profile = None;
        }
        if !self.agents.read(cx).model_catalogs.contains_key(&backend) {
            self.load_cli_model_catalog(backend, false, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_verifier_editor_thinking(
        &mut self,
        level: ThinkingLevel,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.verifier_editor.as_mut() {
            editor.thinking = level;
            cx.notify();
        }
    }

    pub(crate) fn set_verifier_editor_route(&mut self, route_id: String, cx: &mut Context<Self>) {
        if let Some(editor) = self.verifier_editor.as_mut() {
            editor.route_id = Some(route_id);
            cx.notify();
        }
    }

    pub(crate) fn open_verifier_model_picker(&mut self, cx: &mut Context<Self>) {
        let Some(backend) = self.verifier_editor.as_ref().map(|e| e.backend) else {
            return;
        };
        self.open_cli_model_picker(CliModelPickerTarget::StageVerifier, backend, cx);
    }

    pub(crate) fn set_verifier_editor_model(
        &mut self,
        model_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.verifier_editor.as_mut() {
            editor.model = model_id;
            editor.model_profile = None;
            cx.notify();
        }
    }

    pub(crate) fn save_verifier_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(backend), Some(project_id)) = (
            self.agents_backend.clone(),
            self.agents
                .read(cx)
                .snapshot
                .as_ref()
                .map(|s| s.project_id.clone()),
        ) else {
            return;
        };
        let Some(editor) = self.verifier_editor.as_mut() else {
            return;
        };
        if editor.saving {
            return;
        }
        editor.saving = true;
        editor.errors.clear();
        let existing_id = editor.verifier_id.clone();
        let draft = StageVerifierDraft {
            stage: editor.stage.clone(),
            label: editor.label.read(cx).value().trim().to_owned(),
            position: editor.position,
            enabled: editor.enabled,
            reject_limit: editor.reject_limit,
            agent: StepAgent {
                transport: editor.transport,
                backend: (editor.transport == StepTransport::Cli)
                    .then(|| editor.backend.as_str().to_owned()),
                model: (editor.transport == StepTransport::Cli)
                    .then(|| editor.model.clone())
                    .flatten(),
                model_profile: (editor.transport == StepTransport::Cli)
                    .then(|| editor.model_profile.clone())
                    .flatten(),
                thinking_level: editor.thinking.as_str().to_owned(),
                api_route_id: (editor.transport == StepTransport::Api)
                    .then(|| editor.route_id.clone())
                    .flatten(),
            },
            instructions: editor.instructions.read(cx).value().to_owned(),
        };
        cx.notify();
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = match existing_id {
                Some(id) => backend.update_stage_verifier(project_id, id, draft).await,
                None => backend.create_stage_verifier(project_id, draft).await,
            };
            let _ = this.update(cx, |model, cx| {
                if let Some(editor) = model.verifier_editor.as_mut() {
                    editor.saving = false;
                }
                match result {
                    Ok(SaveStageVerifierOutcome::Saved { verifier }) => {
                        model.verifier_editor = None;
                        let id = verifier.id.clone();
                        model.agents.update(cx, |agents, _| {
                            agents.selected_agent =
                                Some(AgentIdentifier::StageVerifier(id.clone()));
                        });
                        model.refresh_agents(cx);
                    }
                    Ok(SaveStageVerifierOutcome::Invalid { errors }) => {
                        if let Some(editor) = model.verifier_editor.as_mut() {
                            editor.errors = errors
                                .into_iter()
                                .map(|err| err.message.replace('`', ""))
                                .collect();
                        }
                    }
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_stage_verifier_enabled(
        &mut self,
        verifier_id: String,
        cx: &mut Context<Self>,
    ) {
        let (Some(backend), Some(project_id), Some(current)) = (
            self.agents_backend.clone(),
            self.agents
                .read(cx)
                .snapshot
                .as_ref()
                .map(|s| s.project_id.clone()),
            self.agents
                .read(cx)
                .verifier(&verifier_id)
                .map(|v| v.enabled),
        ) else {
            return;
        };
        let next = !current;
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .set_stage_verifier_enabled(project_id, verifier_id, next)
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(_) => model.refresh_agents(cx),
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn toggle_verifier_history(&mut self, verifier_id: String, cx: &mut Context<Self>) {
        let opening = self.agents.update(cx, |agents, _| {
            if agents.detail_tab == AgentDetailTab::History {
                agents.detail_tab = AgentDetailTab::default();
                false
            } else {
                agents.detail_tab = AgentDetailTab::History;
                true
            }
        });
        cx.notify();
        if !opening {
            return;
        }
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.stage_verifier_runs(verifier_id.clone()).await;
            let _ = this.update(cx, |model, cx| {
                if let Ok(runs) = result {
                    model.agents.update(cx, |agents, _| {
                        agents.verifier_runs.insert(verifier_id, runs);
                    });
                }
                cx.notify();
            });
        }));
    }

    pub(crate) fn arm_delete_stage_verifier(
        &mut self,
        verifier_id: String,
        cx: &mut Context<Self>,
    ) {
        self.agents.update(cx, |agents, _| {
            agents.pending_verifier_delete = Some(verifier_id);
        });
        cx.notify();
    }

    pub(crate) fn confirm_delete_stage_verifier(
        &mut self,
        verifier_id: String,
        cx: &mut Context<Self>,
    ) {
        let (Some(backend), Some(project_id)) = (
            self.agents_backend.clone(),
            self.agents
                .read(cx)
                .snapshot
                .as_ref()
                .map(|s| s.project_id.clone()),
        ) else {
            return;
        };
        self.agents.update(cx, |agents, _| {
            agents.pending_verifier_delete = None;
        });
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = backend
                .delete_stage_verifier(project_id, verifier_id.clone())
                .await;
            let _ = this.update(cx, |model, cx| {
                match result {
                    Ok(()) => {
                        model.agents.update(cx, |agents, _| {
                            if agents.selected_agent.as_ref()
                                == Some(&AgentIdentifier::StageVerifier(verifier_id.clone()))
                            {
                                agents.selected_agent = None;
                            }
                            agents.verifier_runs.remove(&verifier_id);
                        });
                        model.refresh_agents(cx);
                    }
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                    }
                }
                cx.notify();
            });
        }));
        let _ = verifier_slot_id;
    }
}
