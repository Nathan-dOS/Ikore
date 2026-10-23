//! Pages of the Settings modal that edit the provider configuration snapshot: Connections and
//! Routes (lists with a drill-in detail), Embeddings & RAG, Second Brain, Preparation & context,
//! plus the read-only Agent timeouts table. The modal chrome lives in `settings_modal.rs` and the
//! row/section building blocks in `settings_ui.rs`.

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::AgentTimeoutPolicy;
use hvc_core::embedding_service::ModelPullEvent;
use hvc_core::llm::AgentRole;
use hvc_core::orchestration::IndexingEvent;
use hvc_core::persistence_service::EmbeddingReadinessReport;

use crate::app_model::AppModel;
use crate::settings::SettingsPage;
use crate::settings_search::entry_by_id;
use crate::settings_ui::{
    PageContent, back_link, hint, nav_row, note, readout, row, row_with, section, select,
    stacked_row, switch, warning_note,
};
use crate::sizing::{INPUT_LONG, INPUT_MEDIUM, INPUT_SHORT, StyledExt, sized_input};
use crate::theme;
use crate::widgets::ButtonVariant;

fn describe(id: &'static str) -> Option<&'static str> {
    Some(entry_by_id(id).description)
}

pub(crate) fn reload_action(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let (loading, busy) = (settings.loading, settings.busy);
    action(
        "settings-refresh",
        if loading { "Loading…" } else { "Reload" },
        !loading && !busy,
        cx,
        |model, cx| model.refresh_settings(cx),
    )
    .into_any_element()
}

pub(crate) fn projects_page(model: &AppModel, cx: &mut Context<AppModel>) -> PageContent {
    let settings = model.settings.read(cx);
    let selected = settings.deletion_project_id.clone();
    if selected.is_some() {
        let preview = settings.deletion_preview.clone();
        let loading = settings.deletion_loading;
        let busy = settings.deletion_busy;
        let confirmed = preview.as_ref().is_some_and(|p| {
            model.settings_project_delete_input.read(cx).value() == p.project.name
        });
        let mut page = PageContent::new(SettingsPage::Projects.label())
            .description(SettingsPage::Projects.description())
            .back(back_link(
                "settings-project-back",
                "All projects",
                cx,
                |model, cx| model.cancel_project_deletion(cx),
            ));
        if let Some(preview) = preview {
            let project = preview.project;
            let state = if project.is_archived {
                "Archived"
            } else {
                "Active"
            };
            page=page.meta(format!("{} · {state}",project.name))
                .section("Repository",section("Repository",None,vec![
                    row_with("project-delete-path","Directory".into(),Some("The entire directory, including .git and files you created, will be removed.".into()),Some(project.workspace_path.into()),false,readout(state)),
                    row_with("project-delete-size","Files".into(),None,None,false,readout(format!("{} files · {} bytes",preview.files,preview.bytes))),
                    row_with("project-delete-history","App history".into(),None,None,false,readout(format!("{} pulsations · {} conversations",preview.pulsations,preview.conversations))),
                ]))
                .section("Permanent deletion",section("Permanent deletion",None,vec![
                    warning_note("This removes the repository folder and project data. Provider usage remains attributed to this project in separate local history. The operation cannot be undone."),
                    stacked_row("project-deletion",None,div().flex().flex_col().gap_2().child(note(format!("Type {} to confirm:",project.name))).child(sized_input(model.settings_project_delete_input.clone(), INPUT_MEDIUM))),
                    action_with("settings-delete-project","Delete repository permanently",ButtonVariant::Danger,confirmed && !busy && !loading,cx,|model,cx|model.confirm_project_deletion(cx)).into_any_element(),
                ]));
        } else {
            page=page.section("Repository",section("Repository",None,vec![note(if loading {"Inspecting the repository…"} else {"The repository preview is unavailable. Return to the project list and retry."})]));
        }
        return page;
    }
    let projects = settings
        .snapshot
        .as_ref()
        .map(|s| s.projects.clone())
        .unwrap_or_default();
    let mut rows = vec![stacked_row(
        "project-deletion",
        None,
        note("Choose a project to review its files and delete it permanently."),
    )];
    let pending = settings
        .snapshot
        .as_ref()
        .map_or(0, |s| s.pending_project_deletions);
    if pending > 0 {
        rows.push(warning_note(format!(
            "Cleanup is pending for {pending} deleted project(s). The app retries when it starts."
        )));
    }
    rows.extend(if projects.is_empty() {
        vec![note("No local projects are registered.")]
    } else {
        projects
            .into_iter()
            .map(|project| {
                let id = project.id.clone();
                nav_row(
                    SharedString::from(format!("settings-project:{id}")),
                    if project.is_archived {
                        theme::text_subtle()
                    } else {
                        theme::success()
                    },
                    project.name,
                    project.workspace_path,
                    cx,
                    move |model, cx| model.select_project_for_deletion(id.clone(), cx),
                )
            })
            .collect()
    });
    PageContent::new(SettingsPage::Projects.label())
        .description(SettingsPage::Projects.description())
        .action(reload_action(model, cx))
        .section(
            "Local projects",
            section("Local projects", describe("project-deletion"), rows),
        )
}

// ---------------------------------------------------------------------------------------------
// Connections

pub(crate) fn connections_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let detail = settings.connection_detail
        && (settings.creating_connection || settings.selected_connection_id.is_some());
    if detail {
        return connection_detail(model, hl, cx);
    }
    let busy = settings.busy;
    let connections = settings
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.connections.clone())
        .unwrap_or_default();
    let tests = settings.connection_tests.clone();
    let rows: Vec<AnyElement> = if connections.is_empty() {
        vec![note(
            "No connections yet. Add one to reach an LLM provider.",
        )]
    } else {
        connections
            .into_iter()
            .map(|connection| {
                let id = connection.id.clone();
                let last_test = tests.get(&id);
                // A test this session speaks louder than the stored state.
                let dot = match last_test.map(|test| test.outcome.tone()) {
                    _ if !connection.enabled => theme::text_subtle(),
                    Some(crate::settings::StatusTone::Saved) => theme::success(),
                    Some(crate::settings::StatusTone::Working) => theme::warning(),
                    Some(crate::settings::StatusTone::Failed) => theme::danger(),
                    None if connection.has_api_key => theme::success(),
                    None => theme::warning(),
                };
                nav_row(
                    SharedString::from(format!("settings-connection:{id}")),
                    dot,
                    connection.display_name,
                    format!(
                        "{} · {} · {} · {}",
                        connection.provider_id,
                        connection.default_model,
                        if connection.has_api_key {
                            "key saved"
                        } else {
                            "no key"
                        },
                        last_test
                            .map(|test| test.summary())
                            .unwrap_or_else(|| connection.state.clone())
                    ),
                    cx,
                    move |model, cx| model.open_connection_detail(id.clone(), cx),
                )
            })
            .collect()
    };
    PageContent::new(SettingsPage::Connections.label())
        .description(SettingsPage::Connections.description())
        .action(reload_action(model, cx))
        .action(action(
            "settings-new-connection",
            "New connection",
            !busy,
            cx,
            |model, cx| model.new_settings_connection(cx),
        ))
        .section(
            "Providers",
            section("Providers", describe("connections-list"), rows),
        )
        .section(
            "CLI connections",
            crate::cli_connections::section(model, hl, cx),
        )
}

fn connection_detail(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let busy = settings.busy;
    let creating = settings.creating_connection;
    let connection = settings.connection().cloned();
    let selected = settings.selected_connection_id.is_some();
    let snapshot = settings.snapshot.clone();
    let provider_id = settings.connection_provider_id.clone();
    let api_catalog = settings.api_model_catalog().cloned();
    let endpoint_allowed = snapshot.as_ref().is_some_and(|snapshot| {
        snapshot
            .profiles
            .iter()
            .any(|profile| profile.id == provider_id.as_ref() && profile.endpoint_configurable)
    });
    let connection_model = model.settings_connection_model_input.clone();
    let selected_id = settings
        .selected_connection_id
        .as_ref()
        .map(ToString::to_string);
    let test_result = selected_id
        .as_ref()
        .and_then(|id| settings.connection_tests.get(id).cloned());
    let testing = selected_id.is_some() && settings.testing_connection == selected_id;

    let title: SharedString = match (&connection, creating) {
        (Some(connection), false) => connection.display_name.clone().into(),
        _ => "New connection".into(),
    };
    let description = match (&connection, creating) {
        (Some(connection), false) => format!(
            "{} · {}",
            connection.provider_id,
            if connection.has_api_key {
                "key saved"
            } else {
                "no key saved yet"
            }
        ),
        _ => "Choose a provider, name the connection and paste its key, then save.".into(),
    };

    let mut connection_rows = vec![
        row(
            "connection-provider",
            hl,
            select("settings-provider", provider_id.clone(), !busy, cx),
        ),
        row(
            "connection-name",
            hl,
            sized_input(model.settings_connection_name_input.clone(), INPUT_MEDIUM),
        ),
        row(
            "connection-model",
            hl,
            sized_input(connection_model.clone(), INPUT_MEDIUM),
        ),
    ];
    connection_rows.push(if endpoint_allowed {
        stacked_row(
            "connection-endpoint",
            hl,
            sized_input(model.settings_connection_endpoint_input.clone(), INPUT_LONG),
        )
    } else {
        row_with(
            "connection-endpoint",
            "Endpoint".into(),
            Some("Fixed by the selected provider profile.".into()),
            None,
            hl == Some("connection-endpoint"),
            readout("Fixed"),
        )
    });
    connection_rows.push(stacked_row(
        "connection-api-key",
        hl,
        sized_input(model.settings_connection_secret_input.clone(), INPUT_LONG),
    ));

    let mut page = PageContent::new(title)
        .back(back_link(
            "settings-back-connections",
            "Connections",
            cx,
            |model, cx| model.close_settings_detail(cx),
        ))
        .meta(description)
        .action(action(
            "settings-save-connection",
            "Save",
            !busy,
            cx,
            |model, cx| model.save_settings_connection(cx),
        ))
        .section("Connection", section("Connection", None, connection_rows));

    if !creating && selected {
        page = page.section(
            "Connection test",
            section(
                "Connection test",
                None,
                vec![stacked_row(
                    "connection-test",
                    hl,
                    connection_test_body(test_result, testing, busy, cx),
                )],
            ),
        );
        let catalog_label = api_catalog
            .as_ref()
            .map(|catalog| {
                format!(
                    "{} model(s) · {:?} · {}",
                    catalog.models.len(),
                    catalog.state,
                    catalog.refreshed_at
                )
            })
            .unwrap_or_else(|| "No catalog loaded for this connection yet.".into());
        let reasoning = api_catalog
            .as_ref()
            .and_then(|catalog| {
                let selected = connection_model.read(cx).value();
                catalog
                    .models
                    .iter()
                    .find(|model| model.model_id == selected)
            })
            .map(|model| {
                if model.reasoning_efforts.is_empty() {
                    "Reasoning: not published".to_string()
                } else {
                    format!("Reasoning: {}", model.reasoning_efforts.join(", "))
                }
            })
            .unwrap_or_else(|| "Reasoning: not published".to_string());
        let diagnostic = api_catalog
            .as_ref()
            .and_then(|catalog| catalog.diagnostic.clone());
        let has_models = api_catalog
            .as_ref()
            .is_some_and(|catalog| !catalog.models.is_empty());
        let body = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(action(
                        "settings-refresh-api-catalog",
                        "Refresh catalog",
                        !busy,
                        cx,
                        |model, cx| model.refresh_settings_api_catalog(cx),
                    ))
                    .child(select(
                        "settings-api-model",
                        "Choose model",
                        !busy && has_models,
                        cx,
                    )),
            )
            .child(hint(catalog_label))
            .child(hint(reasoning))
            .when_some(diagnostic, |body, diagnostic| body.child(hint(diagnostic)));
        page = page
            .section(
                "API model catalog",
                section(
                    "API model catalog",
                    None,
                    vec![stacked_row("connection-api-catalog", hl, body)],
                ),
            )
            .section(
                "Danger zone",
                section(
                    "Danger zone",
                    None,
                    vec![row(
                        "connection-delete",
                        hl,
                        action(
                            "settings-delete-connection",
                            "Delete",
                            !busy,
                            cx,
                            |model, cx| model.delete_settings_connection(cx),
                        ),
                    )],
                ),
            );
    }
    page
}

/// The Test button with the last result beside it: status dot, outcome, latency, model and time,
/// the advice in an "i", and the provider's own message when the test did not pass.
fn connection_test_body(
    result: Option<crate::settings::ConnectionTestResult>,
    testing: bool,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    use crate::settings::{ConnectionTestOutcome, StatusTone};
    let tone_color = |tone: StatusTone| match tone {
        StatusTone::Saved => theme::success(),
        StatusTone::Working => theme::warning(),
        StatusTone::Failed => theme::danger(),
    };
    let failed_detail = result
        .as_ref()
        .filter(|result| result.outcome != ConnectionTestOutcome::Connected)
        .map(|result| result.detail.clone());
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_3()
                .child(action(
                    "settings-test-connection",
                    if testing {
                        "Testing…"
                    } else {
                        "Test connection"
                    },
                    !busy && !testing,
                    cx,
                    |model, cx| model.test_settings_connection(cx),
                ))
                .when_some(result, |line, result| {
                    let color = tone_color(result.outcome.tone());
                    let mut facts = Vec::new();
                    if let Some(latency) = result.latency_ms {
                        facts.push(format!("{latency} ms"));
                    }
                    if !result.model.is_empty() {
                        facts.push(result.model.clone());
                    }
                    facts.push(result.at.clone());
                    line.child(
                        div()
                            .debug_selector(|| "connection-test-result".into())
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(crate::widgets::status_dot(color))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(color)
                                    .child(result.outcome.label()),
                            )
                            .child(readout(facts.join(" · ")))
                            .child(crate::widgets::info_tip(
                                "connection-test-advice",
                                result.outcome.advice(),
                            )),
                    )
                }),
        )
        .when_some(failed_detail, |body, detail| body.child(hint(detail)))
        .into_any_element()
}

// ---------------------------------------------------------------------------------------------
// Routes

/// Why a route target fails every call: its model belongs to another
/// first-party provider than its connection's. The dispatch preflight blocks a
/// pulsation whose stage route has only such targets.
fn target_conflict(
    connections: &[hvc_core::llm::ProviderConnection],
    connection_id: &str,
    model_id: &str,
) -> Option<String> {
    let connection = connections.iter().find(|c| c.id == connection_id)?;
    let owner = hvc_core::llm::model_family_conflict(&connection.provider_id, model_id)?;
    Some(format!(
        "`{model_id}` is an {owner} model and `{}` is a {} connection: this target fails every call.",
        connection.display_name, connection.provider_id
    ))
}

pub(crate) fn routes_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let detail =
        settings.route_detail && (settings.creating_route || settings.selected_route_id.is_some());
    if detail {
        return route_detail(model, hl, cx);
    }
    let busy = settings.busy;
    let Some(snapshot) = settings.snapshot.clone() else {
        return PageContent::new(SettingsPage::Routes.label());
    };
    let mode = snapshot.config.routing_v2_mode.as_str();
    let cooling = snapshot
        .resilience
        .states
        .iter()
        .filter(|state| state.state != "closed")
        .count();
    let rows: Vec<AnyElement> = if snapshot.routes.is_empty() {
        vec![note(
            "No routes yet. A route lists connections to try in order, with retry limits.",
        )]
    } else {
        snapshot
            .routes
            .iter()
            .map(|route| {
                let id = route.id.clone();
                let failing = route
                    .targets
                    .iter()
                    .filter(|target| {
                        target_conflict(
                            &snapshot.connections,
                            &target.connection_id,
                            &target.model_id,
                        )
                        .is_some()
                    })
                    .count();
                nav_row(
                    SharedString::from(format!("settings-route:{id}")),
                    if failing > 0 {
                        theme::warning()
                    } else if route.enabled {
                        theme::success()
                    } else {
                        theme::text_subtle()
                    },
                    route.name.clone(),
                    format!(
                        "{} · {} target(s) · {} attempt(s){}",
                        route.strategy,
                        route.targets.len(),
                        route.max_attempts,
                        if failing > 0 {
                            format!(" · {failing} will fail")
                        } else {
                            String::new()
                        }
                    ),
                    cx,
                    move |model, cx| model.open_route_detail(id.clone(), cx),
                )
            })
            .collect()
    };
    let resilience = entry_by_id("resilience");
    PageContent::new(SettingsPage::Routes.label())
        .description(SettingsPage::Routes.description())
        .action(reload_action(model, cx))
        .action(action(
            "settings-new-route",
            "New route",
            !busy,
            cx,
            |model, cx| model.new_settings_route(cx),
        ))
        .section(
            "Routing engine",
            section(
                "Routing engine",
                None,
                vec![row(
                    "routing-mode",
                    hl,
                    select("settings-routing-mode", mode, !busy, cx),
                )],
            ),
        )
        .section("Routes", section("Routes", describe("routes-list"), rows))
        .section(
            "Resilience",
            section(
                "Resilience",
                None,
                vec![row_with(
                    resilience.id,
                    resilience.title.into(),
                    Some(resilience.description.into()),
                    Some(format!("{cooling} provider(s) cooling down now").into()),
                    hl == Some(resilience.id),
                    action(
                        "settings-reset-resilience",
                        "Clear cooldowns",
                        !busy,
                        cx,
                        |model, cx| model.reset_settings_resilience(cx),
                    ),
                )],
            ),
        )
}

fn route_detail(model: &AppModel, hl: Option<&str>, cx: &mut Context<AppModel>) -> PageContent {
    let settings = model.settings.read(cx);
    let busy = settings.busy;
    let creating = settings.creating_route;
    let route = settings.route().cloned();
    let selected_route = settings.selected_route_id.is_some();
    let snapshot = settings.snapshot.clone();
    let connection = settings.connection().cloned();
    let strategy = settings.route_strategy.clone();
    let limits = (
        settings.route_max_attempts,
        settings.route_attempt_timeout_ms,
        settings.route_timeout_ms,
    );
    let targets = settings.route_targets.clone();
    let binding_role = settings.binding_agent_role.clone();
    let binding_stage = settings.binding_stage.clone();
    let connection_name = |id: &str| {
        snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.connections.iter().find(|c| c.id == id))
            .map(|c| c.display_name.clone())
            .unwrap_or_else(|| id.to_owned())
    };
    let target_lines: Vec<AnyElement> = if targets.is_empty() {
        vec![hint("No targets yet — choose a connection below and save.")]
    } else {
        let connections = snapshot
            .as_ref()
            .map(|snapshot| snapshot.connections.as_slice())
            .unwrap_or_default();
        targets
            .iter()
            .enumerate()
            .flat_map(|(index, target)| {
                let line = readout(format!(
                    "{}. {} · {}{}",
                    index + 1,
                    connection_name(&target.connection_id),
                    target.model_id,
                    if target.enabled { "" } else { " · disabled" }
                ));
                let conflict =
                    target_conflict(connections, &target.connection_id, &target.model_id)
                        .map(warning_note);
                std::iter::once(line).chain(conflict)
            })
            .collect()
    };
    let bindings = snapshot
        .as_ref()
        .map(|snapshot| snapshot.bindings.len())
        .unwrap_or_default();
    let title: SharedString = match (&route, creating) {
        (Some(route), false) => route.name.clone().into(),
        _ => "New route".into(),
    };

    let mut page = PageContent::new(title)
        .back(back_link(
            "settings-back-routes",
            "Routes",
            cx,
            |model, cx| model.close_settings_detail(cx),
        ))
        .description(if creating {
            "Name the route, pick its first target connection and save."
        } else {
            "Targets are tried in order until one answers within the limits."
        })
        .action(action(
            "settings-save-route",
            "Save",
            !busy && connection.is_some(),
            cx,
            |model, cx| model.save_settings_route(cx),
        ))
        .section(
            "Route",
            section(
                "Route",
                None,
                vec![
                    row(
                        "route-name",
                        hl,
                        sized_input(model.settings_route_name_input.clone(), INPUT_MEDIUM),
                    ),
                    row(
                        "route-strategy",
                        hl,
                        select("settings-route-strategy", strategy, !busy, cx),
                    ),
                    row(
                        "route-limits",
                        hl,
                        select(
                            "settings-route-limits",
                            format!(
                                "{} × {} s · {} s",
                                limits.0,
                                limits.1 / 1000,
                                limits.2 / 1000
                            ),
                            !busy,
                            cx,
                        ),
                    ),
                ],
            ),
        )
        .section(
            "Targets",
            section(
                "Targets",
                None,
                vec![
                    div()
                        .px_3()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(target_lines)
                        .into_any_element(),
                    row(
                        "route-target-connection",
                        hl,
                        select(
                            "settings-route-target",
                            connection
                                .as_ref()
                                .map(|c| c.display_name.clone())
                                .unwrap_or_else(|| "Choose a connection".into()),
                            !busy,
                            cx,
                        ),
                    ),
                    row(
                        "route-target-model",
                        hl,
                        sized_input(model.settings_connection_model_input.clone(), INPUT_MEDIUM),
                    ),
                    row(
                        "route-target-order",
                        hl,
                        div()
                            .flex()
                            .gap_2()
                            .child(action(
                                "settings-target-up",
                                "Move up",
                                !busy && connection.is_some(),
                                cx,
                                |model, cx| model.move_selected_route_target(-1, cx),
                            ))
                            .child(action(
                                "settings-target-down",
                                "Move down",
                                !busy && connection.is_some(),
                                cx,
                                |model, cx| model.move_selected_route_target(1, cx),
                            )),
                    ),
                ],
            ),
        );
    if !creating && selected_route {
        page = page
            .section(
                "Exact binding",
                section(
                    "Exact binding",
                    None,
                    vec![stacked_row(
                        "route-binding",
                        hl,
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_2()
                                    .child(select("settings-binding-role", binding_role, !busy, cx))
                                    .child(select(
                                        "settings-binding-stage",
                                        binding_stage,
                                        !busy,
                                        cx,
                                    ))
                                    .child(action_with(
                                        "settings-save-binding",
                                        "Bind",
                                        ButtonVariant::Secondary,
                                        !busy,
                                        cx,
                                        |model, cx| model.save_settings_binding(cx),
                                    )),
                            )
                            .child(hint(format!("{bindings} binding(s) saved"))),
                    )],
                ),
            )
            .section(
                "Danger zone",
                section(
                    "Danger zone",
                    None,
                    vec![row(
                        "route-delete",
                        hl,
                        action("settings-delete-route", "Delete", !busy, cx, |model, cx| {
                            model.delete_settings_route(cx)
                        }),
                    )],
                ),
            );
    }
    page
}

// ---------------------------------------------------------------------------------------------
// Embeddings & RAG

pub(crate) fn embeddings_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let Some(snapshot) = settings.snapshot.clone() else {
        return PageContent::new(SettingsPage::Embeddings.label());
    };
    let busy = settings.busy;
    let readiness = settings.embedding_readiness.clone();
    let readiness_loading = settings.embedding_readiness_loading;
    let strategy = match snapshot.config.context_strategy {
        hvc_core::agent_runtime::ContextStrategy::YamlGraph => "graph",
        hvc_core::agent_runtime::ContextStrategy::YamlGraphRag => "rag",
    };
    let pull_status = model.model_pull_status().cloned();
    let model_entry = entry_by_id("embedding-model");
    let indexing = model.indexing_status().map(|event| match event {
        IndexingEvent::Progress { remaining, .. } => (
            theme::accent(),
            format!("Indexing knowledge: {remaining} note(s) remaining"),
        ),
        IndexingEvent::Paused { remaining, .. } => (
            theme::text_muted(),
            format!("Indexing paused ({remaining} remaining) — a pulsation has priority"),
        ),
        IndexingEvent::Idle { .. } => (theme::success(), "Knowledge index synced".to_string()),
        IndexingEvent::Failed { error, .. } => {
            (theme::danger(), format!("Embedding failed: {error}"))
        }
    });

    PageContent::new(SettingsPage::Embeddings.label())
        .description(SettingsPage::Embeddings.description())
        .section(
            "Retrieval",
            section(
                "Retrieval",
                None,
                vec![
                    row(
                        "embeddings-enabled",
                        hl,
                        switch(
                            "settings-embeddings",
                            snapshot.config.embedding_enabled,
                            !busy,
                            cx,
                            |model, cx| model.toggle_embeddings(cx),
                        ),
                    ),
                    row(
                        "context-strategy",
                        hl,
                        select("settings-context-strategy", strategy, !busy, cx),
                    ),
                    row(
                        "embedding-top-k",
                        hl,
                        select(
                            "settings-embedding-top-k",
                            snapshot.config.embedding_top_k.to_string(),
                            !busy,
                            cx,
                        ),
                    ),
                ],
            ),
        )
        .section(
            "Model",
            section(
                "Model",
                None,
                vec![
                    row_with(
                        model_entry.id,
                        model_entry.title.into(),
                        Some(model_entry.description.into()),
                        Some(
                            format!(
                                "Provider: {} · {} key(s) in the keyring",
                                snapshot.config.embedding_provider.as_str(),
                                snapshot
                                    .key_statuses
                                    .iter()
                                    .filter(|status| status.has_api_key)
                                    .count()
                            )
                            .into(),
                        ),
                        hl == Some(model_entry.id),
                        sized_input(model.settings_embedding_model_input.clone(), INPUT_MEDIUM),
                    ),
                    row(
                        "reindex",
                        hl,
                        action("settings-reindex", "Reindex", !busy, cx, |model, cx| {
                            model.reindex_embeddings(cx)
                        }),
                    ),
                ],
            ),
        )
        .section(
            "Setup checklist",
            section(
                "Setup checklist",
                None,
                vec![stacked_row(
                    "embedding-checklist",
                    hl,
                    embedding_readiness_checklist(
                        readiness.as_ref(),
                        readiness_loading,
                        pull_status.as_ref(),
                        busy,
                        cx,
                    )
                    .when_some(indexing, |checklist, (color, label)| {
                        checklist.child(div().pt_1().text_sm().text_color(color).child(label))
                    }),
                )],
            ),
        )
}

/// Three ordered facts, each gating the next, so the developer sees exactly which step is
/// blocking embeddings instead of a raw HTTP error. Only the first failing step is
/// highlighted — steps after it depend on it and haven't really been evaluated yet.
fn embedding_readiness_checklist(
    report: Option<&EmbeddingReadinessReport>,
    loading: bool,
    pull_status: Option<&ModelPullEvent>,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> gpui::Div {
    let mut container = div().flex().flex_col().gap_1().child(div().child(action(
        "settings-readiness-check",
        if loading {
            "Checking…"
        } else {
            "Check again"
        },
        !busy && !loading,
        cx,
        |model, cx| model.refresh_embedding_readiness(cx),
    )));
    let Some(report) = report else {
        return container.child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child("Checking Ollama…"),
        );
    };
    let first_blocking = if !report.ollama_reachable {
        Some(0)
    } else if !report.model_installed {
        Some(1)
    } else if !report.embedding_call_ok {
        Some(2)
    } else {
        None
    };
    let step = |index: usize, label: String| {
        let (icon, color) = if first_blocking == Some(index) {
            ("⚠", theme::warning())
        } else if first_blocking.is_none_or(|blocked| index < blocked) {
            ("✓", theme::success())
        } else {
            ("○", theme::text_muted())
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .when(first_blocking == Some(index), |step| {
                step.bg(theme::warning_soft())
            })
            .child(div().text_color(color).child(icon))
            .child(div().text_sm().child(label))
    };
    let pulling = matches!(
        pull_status,
        Some(ModelPullEvent::Started { .. } | ModelPullEvent::Progress { .. })
    );
    container = container
        .child(step(
            0,
            "Ollama is reachable at 127.0.0.1:11434".to_string(),
        ))
        .child(step(1, format!("Model '{}' is downloaded", report.model)))
        .when(first_blocking == Some(1) && !pulling, |section| {
            section.child(div().pl_6().child(action(
                "settings-download-model",
                "Download model",
                !busy,
                cx,
                |model, cx| model.pull_embedding_model(cx),
            )))
        })
        .child(step(2, "A test embedding call succeeds".to_string()));
    if let Some(status) = pull_status {
        let (color, text) = match status {
            ModelPullEvent::Started { model } => {
                (theme::accent(), format!("Starting download of {model}…"))
            }
            ModelPullEvent::Progress {
                status,
                completed,
                total,
                ..
            } => {
                let text = match completed
                    .checked_mul(100)
                    .and_then(|n| n.checked_div(*total))
                {
                    Some(percent) => format!(
                        "{status}: {}% ({} MB / {} MB)",
                        percent.min(100),
                        completed / 1_000_000,
                        total / 1_000_000
                    ),
                    None => status.clone(),
                };
                (theme::accent(), text)
            }
            ModelPullEvent::Completed { model } => {
                (theme::success(), format!("{model} downloaded."))
            }
            ModelPullEvent::Failed { error, .. } => {
                (theme::danger(), format!("Download failed: {error}"))
            }
        };
        container = container.child(div().text_sm().text_color(color).child(text));
    }
    if let Some(error) = &report.error {
        container = container.child(
            div()
                .text_sm()
                .text_color(theme::danger())
                .child(format!("Last error: {error}")),
        );
    }
    if first_blocking == Some(2) {
        container = container.child(div().text_sm().text_color(theme::text_muted()).child(
            "Ollama is running and the model is downloaded, but a real embedding call still \
                 fails — this usually means Ollama itself needs attention: update it \
                 (`ollama pull` again, or reinstall) if it's old, check `ollama ps`/its logs \
                 while reindexing, and make sure the machine has enough free RAM/VRAM for this \
                 model.",
        ));
    }
    container
}

// ---------------------------------------------------------------------------------------------
// Second Brain

pub(crate) fn second_brain_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let Some(snapshot) = settings.snapshot.clone() else {
        return PageContent::new(SettingsPage::SecondBrain.label());
    };
    let busy = settings.busy;
    let policy = snapshot.config.dynamic_context_policy.clone();
    let mut picker = |entry: &'static str, id: &'static str, value: String| {
        row(entry, hl, select(id, value, !busy, cx))
    };
    let activation = vec![picker(
        "adaptive-mode",
        "settings-adaptive-mode",
        snapshot.config.adaptive_context_mode.as_str().to_owned(),
    )];
    let expansion = vec![
        picker(
            "adaptive-initial-k",
            "settings-adaptive-initial-k",
            policy.initial_k.to_string(),
        ),
        picker(
            "adaptive-max-k",
            "settings-adaptive-max-k",
            policy.max_k.to_string(),
        ),
        picker(
            "adaptive-similarity",
            "settings-adaptive-similarity",
            format!("{:.2}", policy.min_similarity),
        ),
        picker(
            "adaptive-min-candidates",
            "settings-adaptive-min-candidates",
            policy.min_candidates_count.to_string(),
        ),
        picker(
            "adaptive-max-notes",
            "settings-adaptive-max-notes",
            policy.max_notes_budget.to_string(),
        ),
        picker(
            "adaptive-beam",
            "settings-adaptive-beam",
            policy.beam_width.to_string(),
        ),
    ];
    let code = vec![
        picker(
            "adaptive-files",
            "settings-adaptive-files",
            policy.max_source_files.to_string(),
        ),
        picker(
            "adaptive-code",
            "settings-adaptive-code",
            format!("{} chars", policy.max_code_chars),
        ),
        picker(
            "adaptive-timeout",
            "settings-adaptive-timeout",
            format!("{} ms", policy.timeout_ms),
        ),
    ];
    PageContent::new(SettingsPage::SecondBrain.label())
        .description(SettingsPage::SecondBrain.description())
        .section(
            "Adaptive navigation",
            section("Adaptive navigation", None, activation),
        )
        .section(
            "Note expansion",
            section(
                "Note expansion",
                Some("How the navigator widens its search through linked notes."),
                expansion,
            ),
        )
        .section(
            "Code resolution",
            section(
                "Code resolution",
                Some("How much source code notes may pull in."),
                code,
            ),
        )
}

// ---------------------------------------------------------------------------------------------
// Preparation & context

pub(crate) fn preparation_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let settings = model.settings.read(cx);
    let Some(snapshot) = settings.snapshot.clone() else {
        return PageContent::new(SettingsPage::Preparation.label());
    };
    let busy = settings.busy;
    let preparation = snapshot.preparation.clone();
    let threshold = entry_by_id("preparation-threshold");
    let budget = entry_by_id("context-budget");
    PageContent::new(SettingsPage::Preparation.label())
        .description(SettingsPage::Preparation.description())
        .section(
            "Task refinement",
            section(
                "Task refinement",
                None,
                vec![
                    row(
                        "preparation-enabled",
                        hl,
                        switch(
                            "preparation-enabled",
                            preparation.preparation_enabled,
                            !busy,
                            cx,
                            |model, cx| model.save_preparation_settings(true, cx),
                        ),
                    ),
                    row_with(
                        threshold.id,
                        threshold.title.into(),
                        Some(threshold.description.into()),
                        Some(format!("Current: {}", preparation.vagueness_threshold.get()).into()),
                        hl == Some(threshold.id),
                        inline_save(
                            sized_input(model.preparation_threshold_input.clone(), INPUT_SHORT),
                            action_with(
                                "preparation-save",
                                "Save",
                                ButtonVariant::Secondary,
                                !busy,
                                cx,
                                |model, cx| model.save_preparation_settings(false, cx),
                            ),
                        ),
                    ),
                ],
            ),
        )
        .section(
            "Context budget",
            section(
                "Context budget",
                None,
                vec![row_with(
                    budget.id,
                    budget.title.into(),
                    Some(budget.description.into()),
                    Some(
                        format!(
                            "Current: {} characters",
                            snapshot.config.context_chars_limit
                        )
                        .into(),
                    ),
                    hl == Some(budget.id),
                    inline_save(
                        sized_input(model.context_limit_input.clone(), INPUT_SHORT),
                        action_with(
                            "context-budget-save",
                            "Save",
                            ButtonVariant::Secondary,
                            !busy,
                            cx,
                            |model, cx| model.save_context_limit(cx),
                        ),
                    ),
                )],
            ),
        )
        .section(
            "Advisory learning",
            section(
                "Advisory learning",
                None,
                vec![
                    row(
                        "learning-enabled",
                        hl,
                        switch(
                            "learning-enabled",
                            preparation.learning_enabled,
                            !busy,
                            cx,
                            |model, cx| model.toggle_learning(cx),
                        ),
                    ),
                    row(
                        "learning-refinement",
                        hl,
                        select(
                            "learning-refinement-mode",
                            preparation.learning_refinement_mode.as_str(),
                            !busy && preparation.learning_enabled,
                            cx,
                        ),
                    ),
                ],
            ),
        )
        .section(
            "Brownfield enrichment",
            section(
                "Brownfield enrichment",
                Some("Opportunistic LLM calls that map an existing codebase while you work."),
                vec![
                    row(
                        "brownfield-budget",
                        hl,
                        select(
                            "brownfield-llm-budget",
                            preparation.brownfield_llm_budget.to_string(),
                            !busy,
                            cx,
                        ),
                    ),
                    row(
                        "brownfield-radius",
                        hl,
                        select(
                            "brownfield-expansion-radius",
                            format!("k={}", preparation.brownfield_expansion_radius),
                            !busy,
                            cx,
                        ),
                    ),
                ],
            ),
        )
}

fn inline_save(input: impl IntoElement, save: impl IntoElement) -> impl IntoElement {
    div().flex().items_center().gap_2().child(input).child(save)
}

// ---------------------------------------------------------------------------------------------
// Agent timeouts

pub(crate) fn timeouts_page(hl: Option<&str>) -> PageContent {
    PageContent::new(SettingsPage::Timeouts.label())
        .description(SettingsPage::Timeouts.description())
        .section(
            "Execution deadlines",
            section(
                "Execution deadlines",
                None,
                vec![stacked_row("agent-timeouts", hl, agent_timeouts_table())],
            ),
        )
}

fn timeout_row(
    tag: impl Into<SharedString>,
    tag_color: gpui::Rgba,
    tag_soft: gpui::Rgba,
    label: &'static str,
    policy: AgentTimeoutPolicy,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .bg(theme::surface_raised())
        .text_xs()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .bg(tag_soft)
                        .text_color(tag_color)
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(tag.into()),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(theme::text())
                        .child(label),
                ),
        )
        .child(
            div()
                .w(px(88.0))
                .flex_shrink_0()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(format_ms_minutes(policy.base_timeout_ms)),
        )
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .text_color(theme::warning())
                .child(format!("{}s silence", policy.inactivity_window_ms / 1000)),
        )
        .child(
            div()
                .w(px(80.0))
                .flex_shrink_0()
                .text_color(theme::accent())
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(format_ms_minutes(policy.max_timeout_ms)),
        )
        .child(
            div()
                .w(px(96.0))
                .flex_shrink_0()
                .text_color(theme::success())
                .child(format!(
                    "+{}s / frame",
                    policy.extension_increment_ms / 1000
                )),
        )
}

fn agent_timeouts_table() -> gpui::Div {
    let roles = [
        (AgentRole::FeatureAgent, "Feature Forge"),
        (AgentRole::BackendAgent, "Code implementation"),
        (AgentRole::TestAgent, "Suite & verification"),
        (AgentRole::TaskSpecifier, "Batch specification"),
        (AgentRole::ContractAgent, "Interfaces & schemas"),
        (AgentRole::Integrator, "Merge & reconciliation"),
        (AgentRole::LearningAgent, "Knowledge synthesis"),
        (AgentRole::IntakeRouter, "Triage & validation"),
        (AgentRole::Visualizer, "Review brief notes"),
        (AgentRole::NotesScribe, "Note structuring"),
    ];
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_subtle())
        .child(div().flex_1().min_w_0().child("AGENT"))
        .child(div().w(px(88.0)).flex_shrink_0().child("BASE"))
        .child(div().w(px(96.0)).flex_shrink_0().child("INACTIVITY"))
        .child(div().w(px(80.0)).flex_shrink_0().child("CEILING"))
        .child(div().w(px(96.0)).flex_shrink_0().child("EXTENSION"));
    let mut table = div().flex().flex_col().gap_1().child(header);
    for (role, label) in roles {
        table = table.child(timeout_row(
            role.as_str(),
            theme::agent_color(role),
            theme::agent_soft_color(role),
            label,
            AgentTimeoutPolicy::for_role(role),
        ));
    }
    table.child(timeout_row(
        "companion",
        theme::accent(),
        theme::accent_soft(),
        "Interactive chat",
        AgentTimeoutPolicy::companion(),
    ))
}

fn format_ms_minutes(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} min", secs / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

// ---------------------------------------------------------------------------------------------
// Buttons

pub(crate) fn action(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    action_with(id, label, action_variant(id), enabled, cx, handler)
}

/// `action` with an explicit style, for inline saves and secondary binds that should not
/// compete with the page's one primary button.
pub(crate) fn action_with(
    id: &'static str,
    label: &'static str,
    variant: ButtonVariant,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    crate::widgets::button_frame(
        div().id(id).debug_selector(move || id.into()).self_start(),
        variant,
        enabled,
    )
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
    })
    .gap_2()
    .when_some(crate::widgets::action_icon(id), |button, glyph| {
        button.child(crate::widgets::icon(
            glyph,
            px(14.0),
            crate::widgets::button_ink(variant, enabled),
        ))
    })
    .child(label)
}

/// One loud button per form: saving and sending are primary, creating is tinted, deleting and
/// cancelling are danger, and refresh/test/reorder/toggle controls stay secondary.
fn action_variant(id: &str) -> ButtonVariant {
    if id.contains("-delete-") || id.ends_with("-cancel-turn") {
        ButtonVariant::Danger
    } else if id.contains("-save")
        || id.ends_with("-send-message")
        || id.ends_with("-download-model")
    {
        ButtonVariant::Primary
    } else if id.contains("-new-") || id.ends_with("-add-presets") {
        ButtonVariant::Tinted
    } else {
        ButtonVariant::Secondary
    }
}

#[cfg(test)]
mod tests {
    use super::target_conflict;

    fn connection(id: &str, provider: &str, name: &str) -> hvc_core::llm::ProviderConnection {
        hvc_core::llm::ProviderConnection {
            id: id.into(),
            provider_id: provider.into(),
            display_name: name.into(),
            endpoint_url: None,
            default_model: "model".into(),
            enabled: true,
            state: "active".into(),
            blocked_reason: None,
            cooldown_until: None,
            max_concurrency: 1,
            version: 1,
            has_api_key: true,
            chat_eligible: true,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    /// The route of "Haiku Teste 1": the legacy connection became Gemini and
    /// still received `gpt-4.1-mini`.
    #[test]
    fn a_target_whose_model_belongs_to_another_provider_is_flagged() {
        let connections = [
            connection("legacy-openai", "gemini", "Gemini"),
            connection("router", "openrouter", "OpenRouter"),
        ];
        assert_eq!(
            target_conflict(&connections, "legacy-openai", "gpt-4.1-mini").as_deref(),
            Some(
                "`gpt-4.1-mini` is an openai model and `Gemini` is a gemini connection: this target fails every call."
            )
        );
        assert_eq!(
            target_conflict(&connections, "legacy-openai", "gemini-2.5-flash"),
            None
        );
        assert_eq!(
            target_conflict(&connections, "router", "openai/gpt-4.1-mini"),
            None
        );
        assert_eq!(
            target_conflict(&connections, "missing", "gpt-4.1-mini"),
            None
        );
    }
}
