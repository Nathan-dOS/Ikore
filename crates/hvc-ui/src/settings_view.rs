use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use hvc_core::AgentTimeoutPolicy;
use hvc_core::embedding_service::ModelPullEvent;
use hvc_core::llm::AgentRole;
use hvc_core::orchestration::IndexingEvent;
use hvc_core::persistence_service::EmbeddingReadinessReport;

use crate::app_model::AppModel;
use crate::settings::SettingsSection;
use crate::sizing::{INPUT_LONG, INPUT_MEDIUM, INPUT_SHORT, StyledExt, sized_input};
use crate::theme;

/// The Settings category strip (Connections / Routes / Runtime / Usage). Lived in the shell's
/// context bar before the rail redesign; it now sits at the top of the Settings panel itself,
/// keeping the same selector ids.
pub(crate) fn settings_section_strip(
    section: SettingsSection,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .id("settings-section-strip")
        .debug_selector(|| "settings-section-strip".into())
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .children(
            [
                (
                    SettingsSection::Connections,
                    "settings-section-connections",
                    "Connections",
                ),
                (SettingsSection::Routes, "settings-section-routes", "Routes"),
                (SettingsSection::Runtime, "settings-section-runtime", "Runtime"),
                (SettingsSection::Usage, "settings-section-usage", "Usage"),
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
                        if target == SettingsSection::Usage {
                            model.refresh_usage(cx);
                        }
                        cx.notify();
                    }))
                    .child(label)
            }),
        )
        .into_any_element()
}

pub(crate) fn settings_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let section = model.settings.read(cx).settings_section;
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .child(settings_section_strip(section, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(settings_body(model, cx)),
        )
        .into_any_element()
}

fn settings_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let snapshot = settings.snapshot.clone();
    let loading = settings.loading;
    let busy = settings.busy;
    let selected_connection = settings.selected_connection_id.clone();
    let selected_route = settings.selected_route_id.clone();
    let section = settings.settings_section;
    if section == SettingsSection::Usage {
        return crate::usage::usage_panel(model, cx);
    }
    let Some(snapshot) = snapshot else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::text_muted())
            .child(if loading {
                "Loading LLM configuration…"
            } else {
                "Configuration unavailable. Use Reload to try again."
            })
            .into_any_element();
    };

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(280.0))
                .flex_shrink_0()
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header(
                    "PROVIDERS",
                    "Connections",
                    snapshot.connections.len(),
                ))
                .child(action(
                    "settings-new-connection",
                    "New connection",
                    !busy,
                    cx,
                    |model, cx| model.new_settings_connection(cx),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .id("settings-connections-scroll")
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(snapshot.connections.is_empty(), |list| {
                            list.child(empty("No connections configured."))
                        })
                        .children(snapshot.connections.into_iter().map(|connection| {
                            let id = connection.id.clone();
                            let selected = selected_connection
                                .as_ref()
                                .is_some_and(|selected| selected.as_ref() == id);
                            div()
                                .id(SharedString::from(format!("settings-connection:{id}")))
                                .p_3()
                                .rounded_md()
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
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.select_settings_connection(id.clone(), cx);
                                    model.settings.update(cx, |state, _| {
                                        state.settings_section = SettingsSection::Connections
                                    });
                                }))
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(connection.display_name),
                                )
                                .child(div().text_xs().text_color(theme::text_muted()).child(
                                    format!(
                                        "{} · {} · key {}",
                                        connection.provider_id,
                                        connection.default_model,
                                        if connection.has_api_key {
                                            "salva"
                                        } else {
                                            "ausente"
                                        }
                                    ),
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if connection.enabled {
                                            theme::accent()
                                        } else {
                                            theme::warning()
                                        })
                                        .child(connection.state),
                                )
                        })),
                )
                .child(section_header("ROUTING", "Rotas", snapshot.routes.len()))
                .child(action(
                    "settings-new-route",
                    "Nova rota",
                    !busy,
                    cx,
                    |model, cx| model.new_settings_route(cx),
                ))
                .child(
                    div()
                        .max_h(px(190.0))
                        .id("settings-routes-scroll")
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(snapshot.routes.is_empty(), |list| {
                            list.child(empty("Nenhuma rota configurada."))
                        })
                        .children(snapshot.routes.into_iter().map(|route| {
                            let id = route.id.clone();
                            let selected = selected_route
                                .as_ref()
                                .is_some_and(|selected| selected.as_ref() == id);
                            div()
                                .id(SharedString::from(format!("settings-route:{id}")))
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(if selected {
                                    theme::accent()
                                } else {
                                    theme::border()
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.select_settings_route(id.clone(), cx);
                                    model.settings.update(cx, |state, _| {
                                        state.settings_section = SettingsSection::Routes
                                    });
                                }))
                                .child(format!(
                                    "{} · {} · {} alvo(s)",
                                    route.name,
                                    route.strategy,
                                    route.targets.len()
                                ))
                        })),
                ),
        )
        .child(
            div()
                .id("settings-detail-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
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
                                .text_xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("Runtime & LLM routing"),
                        )
                        .child(action(
                            "settings-refresh",
                            if loading {
                                "Carregando…"
                            } else {
                                "Recarregar"
                            },
                            !loading && !busy,
                            cx,
                            |model, cx| model.refresh_settings(cx),
                        )),
                )
                .child(
                    div()
                        .id("settings-section-scroll")
                        .debug_selector(|| "settings-section-scroll".into())
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(match section {
                            SettingsSection::Connections => settings_connections(model, cx),
                            SettingsSection::Routes => settings_routes(model, cx),
                            SettingsSection::Runtime => settings_runtime(model, cx),
                            SettingsSection::Usage => crate::usage::usage_panel(model, cx),
                        }),
                ),
        )
        .into_any_element()
}

fn settings_connections(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let selected_connection = settings.selected_connection_id.clone();
    if selected_connection.is_none() && !settings.creating_connection {
        return empty("Select or create a connection.").into_any_element();
    }
    let snapshot = settings.snapshot.clone().expect("settings snapshot");
    let busy = settings.busy;
    let provider_id = settings.connection_provider_id.clone();
    let endpoint_allowed = snapshot
        .profiles
        .iter()
        .any(|profile| profile.id == provider_id.as_ref() && profile.endpoint_configurable);
    let connection_name = model.settings_connection_name_input.clone();
    let connection_model = model.settings_connection_model_input.clone();
    let connection_endpoint = model.settings_connection_endpoint_input.clone();
    let connection_secret = model.settings_connection_secret_input.clone();

    let provider_div = div().w(px(220.0)).child(crate::select_menu::control(
        "settings-provider",
        "PROVIDER",
        provider_id.clone(),
        !busy,
        cx,
    ));

    div()
        .child(
            card("Selected / new connection")
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(provider_div)
                        .child(sized_input(connection_name, INPUT_MEDIUM)),
                )
                .child(sized_input(connection_model, INPUT_MEDIUM))
                .when(endpoint_allowed, |section| {
                    section.child(sized_input(connection_endpoint, INPUT_LONG))
                })
                .when(!endpoint_allowed, |section| {
                    section.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Fixed endpoint for selected profile."),
                    )
                })
                .child(sized_input(connection_secret, INPUT_LONG))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action(
                            "settings-save-connection",
                            "Save connection",
                            !busy,
                            cx,
                            |model, cx| model.save_settings_connection(cx),
                        ))
                        .child(action(
                            "settings-test-connection",
                            "Test",
                            !busy && selected_connection.is_some(),
                            cx,
                            |model, cx| model.test_settings_connection(cx),
                        ))
                        .child(action(
                            "settings-delete-connection",
                            "Delete",
                            !busy && selected_connection.is_some(),
                            cx,
                            |model, cx| model.delete_settings_connection(cx),
                        )),
                ),
        )
        .into_any_element()
}

fn settings_routes(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let selected_route = settings.selected_route_id.clone();
    let show_form = selected_route.is_some() || settings.creating_route;
    let selected_connection = settings.selected_connection_id.clone();
    let snapshot = settings.snapshot.clone().expect("settings snapshot");
    let busy = settings.busy;
    let route_strategy = settings.route_strategy.clone();
    let binding_role = settings.binding_agent_role.clone();
    let binding_stage = settings.binding_stage.clone();
    let route_limits = (
        settings.route_max_attempts,
        settings.route_attempt_timeout_ms,
        settings.route_timeout_ms,
    );
    let route_target_count = settings.route_targets.len();
    let route_name = model.settings_route_name_input.clone();

    let strategy_div = div().child(crate::select_menu::control(
        "settings-route-strategy",
        "STRATEGY",
        route_strategy.clone(),
        !busy,
        cx,
    ));

    div().flex().flex_col().gap_4()
        .when(!show_form, |panel| panel.child(empty("Select or create a route.")))
        .when(show_form, |panel| panel
                .child(
                    card("Selected / new route")
                        .child(sized_input(route_name, INPUT_MEDIUM))
                        .child(strategy_div)
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "Limits: {} attempt(s) · {} ms/attempt · {} ms/route · {} target(s)",
                            route_limits.0, route_limits.1, route_limits.2, route_target_count
                        )))
                        .child(div().flex().gap_2()
                            .child(crate::select_menu::control(("settings-route-limits").to_string(),"Limits",format!("{} attempts · {} ms", route_limits.0,route_limits.2),!busy,cx))
                            .child(action("settings-target-up", "Target ↑", !busy && selected_connection.is_some(), cx, |model, cx| model.move_selected_route_target(-1, cx)))
                            .child(action("settings-target-down", "Target ↓", !busy && selected_connection.is_some(), cx, |model, cx| model.move_selected_route_target(1, cx))))
                        .child(div().flex().gap_2()
                            .child(action("settings-save-route", "Save route", !busy && selected_connection.is_some(), cx, |model, cx| model.save_settings_route(cx)))
                            .child(action("settings-delete-route", "Delete", !busy && selected_route.is_some(), cx, |model, cx| model.delete_settings_route(cx)))),
                )
        )
                .child(
                    card("Exact binding")
                        .child(div().text_sm().child(format!("Agent: {binding_role} · stage: {binding_stage}")))
                        .child(div().text_xs().text_color(theme::text_muted()).child(format!("{} persisted binding(s)", snapshot.bindings.len())))
                        .child(div().flex().gap_2()
                            .child(crate::select_menu::control(("settings-binding-role").to_string(),"Agent",binding_role.clone(),!busy,cx))
                            .child(crate::select_menu::control(("settings-binding-stage").to_string(),"Stage",binding_stage.clone(),!busy,cx))
                            .child(action("settings-save-binding", "Bind route", !busy && selected_route.is_some(), cx, |model, cx| model.save_settings_binding(cx)))),
                )
                .child(
                    card("Resilience")
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "{} state(s) · snapshot {}",
                            snapshot.resilience.states.len(), snapshot.resilience.generated_at
                        )))
                        .child(action("settings-reset-resilience", "Clear cooldowns", !busy, cx, |model, cx| model.reset_settings_resilience(cx))),
                )
        .into_any_element()
}

fn settings_runtime(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let snapshot = settings.snapshot.clone().expect("settings snapshot");
    let busy = settings.busy;
    let embedding_readiness = settings.embedding_readiness.clone();
    let embedding_readiness_loading = settings.embedding_readiness_loading;
    let mode = snapshot.config.routing_v2_mode.as_str();
    let strategy = match snapshot.config.context_strategy {
        hvc_core::agent_runtime::ContextStrategy::YamlGraph => "graph",
        hvc_core::agent_runtime::ContextStrategy::YamlGraphRag => "rag",
    };
    let adaptive_mode = snapshot.config.adaptive_context_mode.as_str();
    let adaptive_policy = snapshot.config.dynamic_context_policy.clone();
    let embedding_model = model.settings_embedding_model_input.clone();

    let routing_mode_div = div().child(crate::select_menu::control(
        "settings-routing-mode",
        "ROUTING V2",
        mode,
        !busy,
        cx,
    ));

    let auto_open_review = model.auto_open_review;
    div().flex().flex_col().gap_4()
                .child(
                    div().grid().grid_cols(3).gap_2()
                        .child(routing_mode_div)
                        .child(crate::select_menu::control(("settings-context-strategy").to_string(),"CONTEXT",strategy,!busy,cx))
                        .child(toggle("settings-embeddings", "EMBEDDINGS", if snapshot.config.embedding_enabled { "on" } else { "off" }, !busy, cx, |model, cx| model.toggle_embeddings(cx))),
                )
                .child(
                    card("Checkpoint review")
                        .child(toggle("settings-auto-open-review", "AUTO-OPEN REVIEW (Recommended)", if auto_open_review { "on" } else { "off" }, true, cx, |model, cx| model.set_auto_open_review(!model.auto_open_review, cx)))
                        .child(div().text_xs().text_color(theme::text_muted()).child("Off: the flow bar shows \"Review now\" when a checkpoint appears. On: the application opens the review automatically, unless you are composing a Companion message or have an overlay open.")),
                )
                .child(
                    card("Pulsation preparation · local and optional")
                        .child(toggle("preparation-enabled", "TASKREFINE / PRESPEC", if snapshot.preparation.preparation_enabled {"on"}else{"off"}, !busy,cx,|model,cx|model.save_preparation_settings(true,cx)))
                        .child(div().text_sm().child(format!("Current threshold: {}. Changes apply to the next evaluation. Disabling does not confirm a pending review.",snapshot.preparation.vagueness_threshold.get())))
                        .child(sized_input(model.preparation_threshold_input.clone(), INPUT_SHORT))
                        .child(action("preparation-save", "Save threshold (0–100)",!busy,cx,|model,cx|model.save_preparation_settings(false,cx)))
                        .child(div().text_sm().child(format!("Current handoff budget: {} characters. Increase here if mandatory artifacts/responses do not fit.",snapshot.config.context_chars_limit)))
                        .child(sized_input(model.context_limit_input.clone(), INPUT_SHORT))
                        .child(action("context-budget-save","Save context budget",!busy,cx,|model,cx|model.save_context_limit(cx)))
                        .child(toggle("learning-enabled", "ADVISORY LEARNING", if snapshot.preparation.learning_enabled {"on"}else{"off"}, !busy,cx,|model,cx|model.toggle_learning(cx)))
                        .child(crate::select_menu::control(("learning-refinement-mode").to_string(),"GRAPH REFINEMENT",snapshot.preparation.learning_refinement_mode.as_str(),!busy && snapshot.preparation.learning_enabled,cx))
                        .child(div().text_xs().text_color(theme::text_muted()).child("Learning runs after Done or Failed. Observe audits without creating proposals; Propose allows suggestions by host IDs, but all writes still require human approval and CAS."))
                        .child(crate::select_menu::control(("brownfield-llm-budget").to_string(),"BROWNFIELD · CALLS PER PULSATION",snapshot.preparation.brownfield_llm_budget.to_string(),!busy,cx))
                        .child(crate::select_menu::control(("brownfield-expansion-radius").to_string(),"BROWNFIELD · RADIUS",format!("k={}", snapshot.preparation.brownfield_expansion_radius),!busy,cx))
                        .child(div().text_xs().text_color(theme::text_muted()).child("Enrichment is opportunistic: 0 makes no LLM calls; k=1 uses direct neighbors and k=2 allows a second hop. Each batch publishes at most two proposals.")),
                )
                .child(crate::composition_ui::composition_settings_card(model, cx))
                .child({
                    let pull_status = model.model_pull_status().cloned();
                    card("RAG / YAML Graph")
                        .child(div().text_sm().text_color(theme::text_muted()).child(
                            "Turns your knowledge notes (knowledge/obsidian/) into vectors so agents pull in the most relevant context instead of the whole vault. Needs a local Ollama, with the model below downloaded and running in the background — the checklist further down shows exactly what is missing. Once synced, the status bar chip reads \"Knowledge index synced\"."
                        ))
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "Provider: {} · top K: {} · {} key(s) in keyring",
                            snapshot.config.embedding_provider.as_str(), snapshot.config.embedding_top_k,
                            snapshot.key_statuses.iter().filter(|status| status.has_api_key).count()
                        )))
                        .child(sized_input(embedding_model, INPUT_MEDIUM))
                        .child(div().flex().gap_2()
                            .child(crate::select_menu::control(("settings-embedding-top-k").to_string(),"Embedding Top K",snapshot.config.embedding_top_k.to_string(),!busy,cx))
                            .child(action("settings-reindex", "Reindex workspace", !busy, cx, |model, cx| model.reindex_embeddings(cx))))
                        .child(div().text_xs().text_color(theme::text_muted()).child(
                            "Embedding Top K: how many matching notes get pulled into an agent's context per query. Higher finds more but costs more tokens and can dilute focus; lower is faster and cheaper but may miss relevant notes."
                        ))
                        .child(div().text_xs().text_color(theme::text_muted()).child(
                            "Reindex workspace: recomputes vectors for notes that changed since the last sync. Safe to run anytime — it queues background work and never blocks a running pulsation, which always keeps priority."
                        ))
                        .child(embedding_readiness_checklist(
                            embedding_readiness.as_ref(),
                            embedding_readiness_loading,
                            pull_status.as_ref(),
                            busy,
                            cx,
                        ))
                        .when_some(model.indexing_status(), |card, event| {
                            let (color, label) = match event {
                                IndexingEvent::Progress { remaining, .. } => (
                                    theme::accent(),
                                    format!("↻ Indexing knowledge: {remaining} note(s) remaining"),
                                ),
                                IndexingEvent::Paused { remaining, .. } => (
                                    theme::text_muted(),
                                    format!("⏸ Indexing paused ({remaining} remaining) — a pulsation has foreground priority"),
                                ),
                                IndexingEvent::Idle { .. } => {
                                    (theme::success(), "✓ Knowledge index synced".to_string())
                                }
                                IndexingEvent::Failed { error, .. } => {
                                    (theme::danger(), format!("⚠ Embedding failed: {error}"))
                                }
                            };
                            card.child(div().text_sm().text_color(color).child(label))
                        })
                })
                .child(
                    card("Second Brain adaptive navigation")
                        .child(div().text_xs().text_color(theme::text_muted()).child(
                            "Shadow evaluates policy without altering prompt; On enables dynamic expansion and exact code resolution.",
                        ))
                        .child(crate::select_menu::control(("settings-adaptive-mode").to_string(),"ACTIVATION",adaptive_mode,!busy,cx))
                        .child(
                            div().grid().grid_cols(3).gap_2()
                                .child(crate::select_menu::control(("settings-adaptive-initial-k").to_string(),"INITIAL K",adaptive_policy.initial_k.to_string(),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-max-k").to_string(),"MAX K",adaptive_policy.max_k.to_string(),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-similarity").to_string(),"SIMILARITY",format!("{:.2}", adaptive_policy.min_similarity),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-min-candidates").to_string(),"MIN NOTES",adaptive_policy.min_candidates_count.to_string(),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-max-notes").to_string(),"MAX NOTES",adaptive_policy.max_notes_budget.to_string(),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-beam").to_string(),"BEAM",adaptive_policy.beam_width.to_string(),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-files").to_string(),"FILES",adaptive_policy.max_source_files.to_string(),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-code").to_string(),"CODE",format!("{} chars", adaptive_policy.max_code_chars),!busy,cx))
                                .child(crate::select_menu::control(("settings-adaptive-timeout").to_string(),"TIMEOUT",format!("{} ms", adaptive_policy.timeout_ms),!busy,cx)),
                        ),
                )
                .child(settings_agent_timeouts())
        .into_any_element()
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
    let mut container = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded_md()
        .bg(theme::surface())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_muted())
                        .child("EMBEDDING SETUP CHECKLIST"),
                )
                .child(action(
                    "settings-readiness-check",
                    if loading {
                        "Checking…"
                    } else {
                        "Check again"
                    },
                    !busy && !loading,
                    cx,
                    |model, cx| model.refresh_embedding_readiness(cx),
                )),
        );
    let Some(report) = report else {
        return container.child(
            div()
                .text_xs()
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
    let row = |index: usize, label: String| {
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
            .when(first_blocking == Some(index), |row| {
                row.bg(theme::warning_soft())
            })
            .child(div().text_color(color).child(icon))
            .child(div().text_sm().child(label))
    };
    let pulling = matches!(
        pull_status,
        Some(ModelPullEvent::Started { .. } | ModelPullEvent::Progress { .. })
    );
    container = container
        .child(row(0, "Ollama is reachable at 127.0.0.1:11434".to_string()))
        .child(row(1, format!("Model '{}' is downloaded", report.model)))
        .when(first_blocking == Some(1) && !pulling, |section| {
            section.child(div().pl_6().child(action(
                "settings-download-model",
                "Download model",
                !busy,
                cx,
                |model, cx| model.pull_embedding_model(cx),
            )))
        })
        .child(row(2, "A test embedding call succeeds".to_string()));
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
        container = container.child(div().text_xs().text_color(color).child(text));
    }
    if let Some(error) = &report.error {
        container = container.child(
            div()
                .text_xs()
                .text_color(theme::danger())
                .child(format!("Last error: {error}")),
        );
    }
    if first_blocking == Some(2) {
        container = container.child(div().text_xs().text_color(theme::text_muted()).child(
            "Ollama is running and the model is downloaded, but a real embedding call still \
                 fails — this usually means Ollama itself needs attention: update it \
                 (`ollama pull` again, or reinstall) if it's old, check `ollama ps`/its logs \
                 while reindexing, and make sure the machine has enough free RAM/VRAM for this \
                 model.",
        ));
    }
    container
}

fn settings_agent_timeouts() -> gpui::Div {
    let roles = [
        (AgentRole::FeatureAgent, "FeatureAgent (Feature Forge)"),
        (
            AgentRole::BackendAgent,
            "BackendAgent (Code Implementation)",
        ),
        (AgentRole::TestAgent, "TestAgent (Suite & Verification)"),
        (
            AgentRole::TaskSpecifier,
            "TaskSpecifier (Batch Specification)",
        ),
        (
            AgentRole::ContractAgent,
            "ContractAgent (Interfaces & Schemas)",
        ),
        (AgentRole::Integrator, "Integrator (Merge & Reconciliation)"),
        (
            AgentRole::LearningAgent,
            "LearningAgent (Knowledge Synthesis)",
        ),
        (
            AgentRole::IntakeRouter,
            "IntakeRouter (Triage & Validation)",
        ),
    ];

    let mut rows = div().flex().flex_col().gap_1();

    // Table Header
    rows = rows.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme::surface())
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::text_muted())
            .child(div().w(px(260.0)).child("AGENT ROLE"))
            .child(div().w(px(100.0)).child("BASE TIMEOUT"))
            .child(div().w(px(120.0)).child("INACTIVITY LIMIT"))
            .child(div().w(px(110.0)).child("MAX CEILING"))
            .child(div().w(px(120.0)).child("STREAM EXTENSION")),
    );

    for (role, label) in roles {
        let policy = AgentTimeoutPolicy::for_role(role);
        let color = theme::agent_color(role);
        let soft = theme::agent_soft_color(role);

        rows = rows.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface_raised())
                .text_xs()
                .child(
                    div()
                        .w(px(260.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .px_1()
                                .py_0p5()
                                .rounded_sm()
                                .bg(soft)
                                .text_color(color)
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(role.as_str()),
                        )
                        .child(div().text_color(theme::text()).child(label)),
                )
                .child(
                    div()
                        .w(px(100.0))
                        .text_color(theme::text())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(format_ms_minutes(policy.base_timeout_ms)),
                )
                .child(
                    div()
                        .w(px(120.0))
                        .text_color(theme::warning())
                        .child(format!("{}s silence", policy.inactivity_window_ms / 1000)),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .text_color(theme::accent())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(format_ms_minutes(policy.max_timeout_ms)),
                )
                .child(
                    div()
                        .w(px(120.0))
                        .text_color(theme::success())
                        .child(format!(
                            "+{}s / frame",
                            policy.extension_increment_ms / 1000
                        )),
                ),
        );
    }

    let companion_policy = AgentTimeoutPolicy::companion();
    rows = rows.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_raised())
            .text_xs()
            .child(
                div()
                    .w(px(260.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_1()
                            .py_0p5()
                            .rounded_sm()
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("companion"),
                    )
                    .child(
                        div()
                            .text_color(theme::text())
                            .child("Companion (Interactive Chat)"),
                    ),
            )
            .child(
                div()
                    .w(px(100.0))
                    .text_color(theme::text())
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(format_ms_minutes(companion_policy.base_timeout_ms)),
            )
            .child(
                div()
                    .w(px(120.0))
                    .text_color(theme::warning())
                    .child(format!(
                        "{}s silence",
                        companion_policy.inactivity_window_ms / 1000
                    )),
            )
            .child(
                div()
                    .w(px(110.0))
                    .text_color(theme::accent())
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(format_ms_minutes(companion_policy.max_timeout_ms)),
            )
            .child(
                div()
                    .w(px(120.0))
                    .text_color(theme::success())
                    .child(format!(
                        "+{}s / frame",
                        companion_policy.extension_increment_ms / 1000
                    )),
            ),
    );

    card("Agent execution timeouts & Progressive streaming")
        .child(div().text_xs().text_color(theme::text_muted()).child(
            "Active execution deadlines dynamically extend as the CLI delivers output frames (thinking tokens, schema deltas, tool events). An inactivity window aborts hung processes; an absolute ceiling prevents runaway execution.",
        ))
        .child(rows)
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

fn section_header(kicker: &'static str, title: &'static str, count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_end()
        .justify_between()
        .child(
            div()
                .child(div().text_xs().text_color(theme::accent()).child(kicker))
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(count.to_string()),
        )
}

pub(crate) fn card(title: impl Into<SharedString>) -> gpui::Div {
    div()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.into()),
        )
}

fn empty(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .text_sm()
        .text_color(theme::text_muted())
        .child(message)
}

pub(crate) fn action(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .self_start()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if enabled {
            theme::accent()
        } else {
            theme::surface()
        })
        .text_color(if enabled {
            theme::canvas()
        } else {
            theme::text_muted()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(label)
}

pub(crate) fn toggle(
    id: &'static str,
    label: &'static str,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    let value = value.into();
    div()
        .id(id)
        .debug_selector(move || id.into())
        .self_start()
        .min_w(px(220.0))
        .max_w(px(360.0))
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .when(enabled, |item| {
            item.cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(value))
}
