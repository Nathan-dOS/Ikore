//! The Agents page's list of agents (ADR-040 M7, specification 12.1): the
//! pipeline stages in their order, the support agents, then the free-form
//! slots with the archived ones folded.

use std::time::Instant;

use gpui::{AnyElement, Context, FontWeight, IntoElement, Rgba, SharedString, div, prelude::*, px};
use ikore_core::llm::AgentRole;
use ikore_core::terminals::{
    AgentExecutionMode, CliAgentConfig, CliBackend, TerminalRun, TerminalRunStatus,
};

use crate::agents::AgentIdentifier;
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::settings::SettingsSnapshot;
use crate::theme;

use super::{action_button, run_status, status_color};

/// The pipeline stages, in the pipeline's order.
const PIPELINE: [AgentRole; 6] = [
    AgentRole::IntakeRouter,
    AgentRole::TaskSpecifier,
    AgentRole::ContractAgent,
    AgentRole::BackendAgent,
    AgentRole::TestAgent,
    AgentRole::Integrator,
];

/// The agents beside the pipeline.
const SUPPORT: [AgentRole; 5] = [
    AgentRole::FeatureAgent,
    AgentRole::Visualizer,
    AgentRole::LearningAgent,
    AgentRole::NotesScribe,
    AgentRole::DocumentationReconciler,
];

/// What one row of the list shows.
struct ListItem {
    id: AgentIdentifier,
    label: String,
    color: Rgba,
    run: Option<TerminalRun>,
    /// `CLI · claude`, or `API · {route}`.
    transport: String,
    /// What the run does now, from its pulse; only while it runs.
    now: Option<String>,
    fallbacks: usize,
}

impl ListItem {
    /// Failed, waiting for a fallback decision, or with a pending fallback.
    fn needs_attention(&self) -> bool {
        self.fallbacks > 0
            || self.run.as_ref().is_some_and(|run| {
                matches!(
                    run.status,
                    TerminalRunStatus::Failed | TerminalRunStatus::AwaitingFallback
                )
            })
    }
}

/// The transport badge: the CLI's backend, or the API's bound route when it
/// has one.
fn transport(mode: AgentExecutionMode, backend: CliBackend, route: Option<String>) -> String {
    match (mode, route) {
        (AgentExecutionMode::CliProcess, _) => format!("CLI · {}", backend.as_str()),
        (AgentExecutionMode::ApiDirect, Some(route)) => format!("API · {route}"),
        (AgentExecutionMode::ApiDirect, None) => "API".to_owned(),
    }
}

fn route_name(settings: Option<&SettingsSnapshot>, route_id: &str) -> Option<String> {
    settings
        .and_then(|settings| settings.routes.iter().find(|route| route.id == route_id))
        .map(|route| route.name.clone())
}

/// The route a stage's agent is bound to, by the same binding
/// `configure_agent_api` resolves: project, role and stage.
fn stage_route(
    settings: Option<&SettingsSnapshot>,
    project_id: &str,
    role: AgentRole,
) -> Option<String> {
    let stage = ikore_core::terminals::canonical_agent(role).stage;
    settings
        .and_then(|settings| {
            settings.bindings.iter().find(|binding| {
                binding.project_id == project_id
                    && binding.agent_role == role.as_str()
                    && binding.stage.eq_ignore_ascii_case(stage)
            })
        })
        .and_then(|binding| route_name(settings, &binding.route_id))
}

fn group_label(label: &'static str) -> AnyElement {
    div()
        .debug_selector(move || format!("agents-group:{}", label.to_lowercase()))
        .mt_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(label)
        .into_any_element()
}

pub(super) fn agents_sidebar(
    model: &AppModel,
    snapshot: &ikore_core::terminals::TerminalsSnapshot,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let now = Instant::now();
    let (selected, show_archived, canonical, free_form, archived) = {
        let agents = model.agents.read(cx);
        let settings = model.settings.read(cx).snapshot.as_ref();
        let now_line = |run: Option<&TerminalRun>| {
            run.filter(|run| run.status.is_active())
                .and_then(|run| agents.live.get(&run.id))
                .map(|live| live.line(now))
        };
        let canonical_item = |config: &CliAgentConfig, run: Option<&TerminalRun>| ListItem {
            id: AgentIdentifier::Canonical(config.agent_role),
            label: ikore_core::terminals::canonical_agent(config.agent_role)
                .name
                .to_owned(),
            color: theme::agent_color(config.agent_role),
            run: run.cloned(),
            transport: transport(
                config.execution_mode,
                config.backend,
                stage_route(settings, &snapshot.project_id, config.agent_role),
            ),
            now: now_line(run),
            fallbacks: agents
                .fallbacks
                .iter()
                .filter(|fallback| fallback.agent_role == config.agent_role)
                .count(),
        };
        let group = |roles: &[AgentRole]| {
            roles
                .iter()
                .filter_map(|role| {
                    snapshot
                        .agents
                        .iter()
                        .find(|agent| agent.config.agent_role == *role)
                })
                .map(|agent| canonical_item(&agent.config, agent.recent_runs.first()))
                .collect::<Vec<_>>()
        };
        let slot_item = |slot: &ikore_core::terminals::FreeFormTerminalSnapshot| ListItem {
            id: AgentIdentifier::FreeForm(slot.slot.id.clone()),
            label: slot.slot.label.clone(),
            color: theme::accent(),
            run: slot.recent_runs.first().cloned(),
            transport: transport(
                slot.slot.execution_mode,
                slot.slot.backend,
                agents
                    .free_form_bindings
                    .iter()
                    .find(|binding| binding.slot_id == slot.slot.id)
                    .and_then(|binding| route_name(settings, &binding.route_id)),
            ),
            now: now_line(slot.recent_runs.first()),
            fallbacks: 0,
        };
        // A role in neither list (a newer build's) still shows, with the support agents.
        let mut support = group(&SUPPORT);
        support.extend(
            snapshot
                .agents
                .iter()
                .filter(|agent| {
                    !PIPELINE.contains(&agent.config.agent_role)
                        && !SUPPORT.contains(&agent.config.agent_role)
                })
                .map(|agent| canonical_item(&agent.config, agent.recent_runs.first())),
        );
        (
            agents.selected_agent.clone(),
            agents.show_archived,
            [("PIPELINE", group(&PIPELINE)), ("SUPPORT", support)],
            snapshot
                .free_form_slots
                .iter()
                .map(slot_item)
                .collect::<Vec<_>>(),
            snapshot
                .archived_free_form_slots
                .iter()
                .map(slot_item)
                .collect::<Vec<_>>(),
        )
    };
    let mut list = div()
        .id("agents-list")
        .debug_selector(|| "agents-list".into())
        .w_full()
        .h_full()
        .min_h_0()
        .overflow_y_scroll()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .border_r_1()
        .border_color(theme::border())
        .bg(theme::surface());
    for (label, items) in canonical {
        if items.is_empty() {
            continue;
        }
        list = list.child(group_label(label)).children(
            items
                .into_iter()
                .map(|item| agent_list_item(item, &selected, cx)),
        );
    }
    list = list
        .child(group_label("FREE-FORM"))
        .child(action_button(
            "free-form-create".into(),
            "New freeform agent".into(),
            !busy && snapshot.free_form_slots.len() < 8,
            false,
            cx,
            |model, cx| model.create_free_form_agent(cx),
        ))
        .children(
            free_form
                .into_iter()
                .map(|item| agent_list_item(item, &selected, cx)),
        );
    if !archived.is_empty() {
        let count = archived.len();
        list = list.child(
            div()
                .id("agents-archived-toggle")
                .debug_selector(|| "agents-archived-toggle".into())
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .rounded_sm()
                .text_xs()
                .text_color(theme::text_muted())
                .cursor_pointer()
                // A background, not a text colour: that would drop the label's size in GPUI.
                .hover(|style| style.bg(theme::hover_bg()))
                .on_click(cx.listener(|model, _, _, cx| {
                    model.agents.update(cx, |agents, _| {
                        agents.show_archived = !agents.show_archived;
                    });
                    cx.notify();
                }))
                .child(crate::widgets::icon(
                    if show_archived {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    px(12.0),
                    theme::text_muted(),
                ))
                .child(format!("Archived · {count}")),
        );
        if show_archived {
            list = list.children(archived.into_iter().map(|mut item| {
                item.label.push_str(" · archived");
                agent_list_item(item, &selected, cx)
            }));
        }
    }
    list.into_any_element()
}

fn agent_list_item(
    item: ListItem,
    selected: &Option<AgentIdentifier>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let attention = item.needs_attention();
    let is_selected = selected.as_ref() == Some(&item.id);
    let ListItem {
        id: agent,
        label,
        color,
        run,
        transport,
        now,
        fallbacks,
    } = item;
    let key = match &agent {
        AgentIdentifier::Canonical(role) => role.as_str().to_owned(),
        AgentIdentifier::FreeForm(slot_id) => slot_id.clone(),
    };
    let element_id = format!("agent-select:{key}");
    let transport_selector = format!("agent-transport:{key}");
    let attention_selector = format!("agent-attention:{key}");
    let status = run.as_ref().map(|run| {
        let mut text = run_status(run.status).to_owned();
        if let Some(time) = crate::agent_dock_view::run_time(run) {
            text.push_str(" · ");
            text.push_str(&time);
        }
        (status_color(Some(run.status)), text)
    });
    div()
        .id(SharedString::from(element_id.clone()))
        .debug_selector(move || element_id.clone())
        .p_2()
        .rounded_md()
        .cursor_pointer()
        .bg(if is_selected {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .hover(|style| style.bg(theme::surface_raised()))
        .on_click(cx.listener(move |model, _, _, cx| {
            let selected = agent.clone();
            model.agents.update(cx, |agents, _| {
                agents.selected_agent = Some(agent.clone());
                agents.selected_api_target = None;
                agents.active_model_picker = None;
            });
            if model.settings.read(cx).snapshot.is_none() {
                model.refresh_settings(cx);
            }
            match selected {
                AgentIdentifier::Canonical(role) => model.configure_agent_api(role, cx),
                AgentIdentifier::FreeForm(slot_id) => model.configure_free_form_api(slot_id, cx),
            }
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(crate::widgets::status_dot(color))
                .child(div().flex_1().min_w_0().truncate().text_sm().child(label))
                .when(attention, |row| {
                    row.child(
                        div()
                            .debug_selector(move || attention_selector.clone())
                            .child(crate::widgets::icon(
                                Icon::TriangleAlert,
                                px(12.0),
                                theme::warning(),
                            )),
                    )
                }),
        )
        .child(
            div()
                .mt_1()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .text_xs()
                .when_some(status, |row, (color, text)| {
                    row.child(div().text_color(color).child(text))
                })
                .child(
                    div()
                        .debug_selector(move || transport_selector.clone())
                        .px_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme::border())
                        .text_color(theme::text_muted())
                        .child(transport),
                ),
        )
        .when_some(now, |item, line| {
            item.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .truncate()
                    .child(line),
            )
        })
        .when(fallbacks > 0, |item| {
            item.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!("{fallbacks} pending fallback(s)")),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use ikore_core::llm::{LlmResilienceSnapshot, ProviderRoute, ProviderRouteBinding};

    use super::*;

    /// One route, `Primary`, bound to the Backend agent of `project` at `stage`.
    fn settings(stage: &str) -> SettingsSnapshot {
        SettingsSnapshot {
            projects: vec![],
            pending_project_deletions: 0,
            composition: None,
            preparation: Default::default(),
            config: Default::default(),
            profiles: vec![],
            connections: vec![],
            api_model_catalogs: vec![],
            routes: vec![ProviderRoute {
                id: "route-1".into(),
                name: "Primary".into(),
                strategy: "ordered".into(),
                max_attempts: 1,
                attempt_timeout_ms: 1,
                route_timeout_ms: 1,
                enabled: true,
                version: 1,
                targets: vec![],
                created_at: "now".into(),
                updated_at: "now".into(),
            }],
            bindings: vec![ProviderRouteBinding {
                project_id: "project".into(),
                agent_role: AgentRole::BackendAgent.as_str().into(),
                stage: stage.into(),
                route_id: "route-1".into(),
                enabled: true,
                version: 1,
                created_at: "now".into(),
                updated_at: "now".into(),
            }],
            key_statuses: vec![],
            resilience: LlmResilienceSnapshot {
                generated_at: "now".into(),
                states: vec![],
            },
            attestation: None,
        }
    }

    /// ADR-040 M7 (12.1): the badge names the CLI's backend, or the API's route.
    #[test]
    fn the_transport_badge_names_the_backend_or_the_route() {
        let cli = transport(AgentExecutionMode::CliProcess, CliBackend::Claude, None);
        assert_eq!(cli, "CLI · claude");
        let routed = transport(
            AgentExecutionMode::ApiDirect,
            CliBackend::Claude,
            Some("Primary".into()),
        );
        assert_eq!(routed, "API · Primary");
        let unbound = transport(AgentExecutionMode::ApiDirect, CliBackend::Claude, None);
        assert_eq!(unbound, "API");
    }

    /// The route is the project's binding for the role at its stage, whatever
    /// the stage's case; nothing else names it.
    #[test]
    fn a_stage_finds_its_route_by_project_role_and_stage() {
        let stage = ikore_core::terminals::canonical_agent(AgentRole::BackendAgent).stage;
        let settings = settings(&stage.to_uppercase());
        let found = stage_route(Some(&settings), "project", AgentRole::BackendAgent);
        assert_eq!(found.as_deref(), Some("Primary"));
        assert_eq!(
            stage_route(Some(&settings), "another", AgentRole::BackendAgent),
            None
        );
        assert_eq!(
            stage_route(Some(&settings), "project", AgentRole::TaskSpecifier),
            None
        );
        assert_eq!(stage_route(None, "project", AgentRole::BackendAgent), None);
        assert_eq!(route_name(Some(&settings), "route-2"), None);
    }
}
