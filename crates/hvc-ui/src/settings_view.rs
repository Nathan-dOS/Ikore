use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};

use crate::app_model::AppModel;
use crate::settings::SettingsSection;
use crate::theme;
use crate::widgets::{dropdown_item, dropdown_menu, dropdown_select};

pub(crate) fn settings_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let snapshot = settings.snapshot.clone();
    let loading = settings.loading;
    let busy = settings.busy;
    let selected_connection = settings.selected_connection_id.clone();
    let selected_route = settings.selected_route_id.clone();
    let notice = settings.notice.clone();
    let error = settings.error.clone();
    let section = settings.settings_section;
    let Some(snapshot) = snapshot else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::text_muted())
            .child(if loading {
                "Carregando configuração LLM…"
            } else {
                "Configuração indisponível. Use Recarregar para tentar novamente."
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
                    "Conexões",
                    snapshot.connections.len(),
                ))
                .child(action(
                    "settings-new-connection",
                    "Nova conexão",
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
                            list.child(empty("Nenhuma conexão configurada."))
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
                .when_some(notice, |panel, notice| panel.child(banner(notice, false)))
                .when_some(error, |panel, error| {
                    panel.child(banner(error.presentable().into(), true))
                })
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
                        }),
                ),
        )
        .into_any_element()
}

fn settings_connections(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let selected_connection = settings.selected_connection_id.clone();
    if selected_connection.is_none() && !settings.creating_connection {
        return empty("Selecione ou crie uma conexão.").into_any_element();
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
    let provider_open = model.is_dropdown_open("settings-provider");
    let mut provider_div = div().w(px(220.0)).child(dropdown_select(
        "settings-provider",
        "PROVIDER",
        provider_id.clone(),
        provider_open,
        !busy,
        snapshot.profiles.len(),
        cx,
        |model, cx| model.toggle_dropdown("settings-provider", cx),
    ));
    if provider_open {
        let mut items = Vec::new();
        for profile in &snapshot.profiles {
            let p_id = profile.id.clone();
            let p_id_capture = profile.id.clone();
            items.push(dropdown_item(
                format!("settings-provider:{p_id}"),
                profile.display_name.clone(),
                Some(profile.id.clone()),
                provider_id.as_ref() == p_id.as_str(),
                cx,
                move |model, cx| model.select_settings_provider(p_id_capture.clone(), cx),
            ));
        }
        provider_div = provider_div.child(dropdown_menu("settings-provider-menu", items));
    }

    div()
        .child(
            card("Conexão selecionada / nova")
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(provider_div)
                        .child(div().flex_1().child(connection_name)),
                )
                .child(connection_model)
                .when(endpoint_allowed, |section| {
                    section.child(connection_endpoint)
                })
                .when(!endpoint_allowed, |section| {
                    section.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Endpoint fixo pelo perfil selecionado."),
                    )
                })
                .child(connection_secret)
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action(
                            "settings-save-connection",
                            "Salvar conexão",
                            !busy,
                            cx,
                            |model, cx| model.save_settings_connection(cx),
                        ))
                        .child(action(
                            "settings-test-connection",
                            "Testar",
                            !busy && selected_connection.is_some(),
                            cx,
                            |model, cx| model.test_settings_connection(cx),
                        ))
                        .child(action(
                            "settings-delete-connection",
                            "Excluir",
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
    let strategy_open = model.is_dropdown_open("settings-route-strategy");
    let strategies = ["priority", "lkgp", "score"];
    let mut strategy_div = div().child(dropdown_select(
        "settings-route-strategy",
        "ESTRATÉGIA",
        route_strategy.clone(),
        strategy_open,
        !busy,
        strategies.len(),
        cx,
        |model, cx| model.toggle_dropdown("settings-route-strategy", cx),
    ));
    if strategy_open {
        let mut items = Vec::new();
        for strat in strategies {
            let s = strat.to_string();
            let s_capture = strat.to_string();
            let label = match strat {
                "priority" => "Priority (ordem fixa)",
                "lkgp" => "LKGP (menor latência)",
                "score" => "Score (ponderação)",
                other => other,
            };
            items.push(dropdown_item(
                format!("settings-route-strategy:{s}"),
                label,
                None::<&str>,
                route_strategy.as_ref() == s.as_str(),
                cx,
                move |model, cx| model.set_route_strategy(s_capture.clone(), cx),
            ));
        }
        strategy_div = strategy_div.child(dropdown_menu("settings-route-strategy-menu", items));
    }

    div().flex().flex_col().gap_4()
        .when(!show_form, |panel| panel.child(empty("Selecione ou crie uma rota.")))
        .when(show_form, |panel| panel
                .child(
                    card("Rota selecionada / nova")
                        .child(route_name)
                        .child(strategy_div)
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "Limites: {} tentativa(s) · {} ms/tentativa · {} ms/rota · {} alvo(s)",
                            route_limits.0, route_limits.1, route_limits.2, route_target_count
                        )))
                        .child(div().flex().gap_2()
                            .child(action("settings-route-limits", "Alternar limites", !busy, cx, |model, cx| model.cycle_route_limits(cx)))
                            .child(action("settings-target-up", "Alvo ↑", !busy && selected_connection.is_some(), cx, |model, cx| model.move_selected_route_target(-1, cx)))
                            .child(action("settings-target-down", "Alvo ↓", !busy && selected_connection.is_some(), cx, |model, cx| model.move_selected_route_target(1, cx))))
                        .child(div().flex().gap_2()
                            .child(action("settings-save-route", "Salvar rota", !busy && selected_connection.is_some(), cx, |model, cx| model.save_settings_route(cx)))
                            .child(action("settings-delete-route", "Excluir", !busy && selected_route.is_some(), cx, |model, cx| model.delete_settings_route(cx)))),
                )
        )
                .child(
                    card("Binding exato")
                        .child(div().text_sm().child(format!("Agente: {binding_role} · estágio: {binding_stage}")))
                        .child(div().text_xs().text_color(theme::text_muted()).child(format!("{} binding(s) persistido(s)", snapshot.bindings.len())))
                        .child(div().flex().gap_2()
                            .child(action("settings-binding-role", "Alternar agente", !busy, cx, |model, cx| model.cycle_binding_role(cx)))
                            .child(action("settings-binding-stage", "Alternar estágio", !busy, cx, |model, cx| model.cycle_binding_stage(cx)))
                            .child(action("settings-save-binding", "Vincular rota", !busy && selected_route.is_some(), cx, |model, cx| model.save_settings_binding(cx)))),
                )
                .child(
                    card("Resiliência")
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "{} estado(s) · snapshot {}",
                            snapshot.resilience.states.len(), snapshot.resilience.generated_at
                        )))
                        .child(action("settings-reset-resilience", "Limpar cooldowns", !busy, cx, |model, cx| model.reset_settings_resilience(cx))),
                )
        .into_any_element()
}

fn settings_runtime(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let settings = model.settings.read(cx);
    let snapshot = settings.snapshot.clone().expect("settings snapshot");
    let busy = settings.busy;
    let mode = snapshot.config.routing_v2_mode.as_str();
    let strategy = snapshot.config.context_strategy.as_str();
    let adaptive_mode = snapshot.config.adaptive_context_mode.as_str();
    let adaptive_policy = snapshot.config.dynamic_context_policy.clone();
    let embedding_model = model.settings_embedding_model_input.clone();
    let routing_mode_open = model.is_dropdown_open("settings-routing-mode");
    let modes = [
        (hvc_core::llm::RoutingMode::Off, "off", "Off (desativado)"),
        (
            hvc_core::llm::RoutingMode::Shadow,
            "shadow",
            "Shadow (auditoria)",
        ),
        (hvc_core::llm::RoutingMode::On, "on", "On (enforcing)"),
    ];
    let mut routing_mode_div = div().child(dropdown_select(
        "settings-routing-mode",
        "ROUTING V2",
        mode,
        routing_mode_open,
        !busy,
        modes.len(),
        cx,
        |model, cx| model.toggle_dropdown("settings-routing-mode", cx),
    ));
    if routing_mode_open {
        let mut items = Vec::new();
        for (m_enum, m_str, m_label) in modes {
            items.push(dropdown_item(
                format!("settings-routing-mode:{m_str}"),
                m_label,
                None::<&str>,
                mode == m_str,
                cx,
                move |model, cx| model.set_routing_mode(m_enum, cx),
            ));
        }
        routing_mode_div =
            routing_mode_div.child(dropdown_menu("settings-routing-mode-menu", items));
    }

    let auto_open_review = model.auto_open_review;
    div().flex().flex_col().gap_4()
                .child(
                    div().grid().grid_cols(3).gap_2()
                        .child(routing_mode_div)
                        .child(toggle("settings-context-strategy", "CONTEXTO", strategy, !busy, cx, |model, cx| model.toggle_context_strategy(cx)))
                        .child(toggle("settings-embeddings", "EMBEDDINGS", if snapshot.config.embedding_enabled { "on" } else { "off" }, !busy, cx, |model, cx| model.toggle_embeddings(cx))),
                )
                .child(
                    card("Revisão de checkpoints")
                        .child(toggle("settings-auto-open-review", "AUTO-OPEN REVIEW (Recommended)", if auto_open_review { "on" } else { "off" }, true, cx, |model, cx| model.set_auto_open_review(!model.auto_open_review, cx)))
                        .child(div().text_xs().text_color(theme::text_muted()).child("Off: a barra de fluxo mostra \"Review now\" quando um checkpoint aparece. On: a aplicação abre a revisão automaticamente, a menos que você esteja compondo uma mensagem no Companion ou com um overlay aberto.")),
                )
                .child(
                    card("Preparação de pulsações · local e opcional")
                        .child(toggle("preparation-enabled", "TASKREFINE / PRESPEC", if snapshot.preparation.preparation_enabled {"on"}else{"off"}, !busy,cx,|model,cx|model.save_preparation_settings(true,cx)))
                        .child(div().text_sm().child(format!("Threshold atual: {}. Alterações valem para a próxima avaliação. Desligar não confirma uma revisão pendente.",snapshot.preparation.vagueness_threshold.get())))
                        .child(model.preparation_threshold_input.clone())
                        .child(action("preparation-save", "Salvar threshold (0–100)",!busy,cx,|model,cx|model.save_preparation_settings(false,cx)))
                        .child(div().text_sm().child(format!("Orçamento atual do handoff: {} caracteres. Aumente aqui se artefatos/respostas obrigatórios não couberem.",snapshot.config.context_chars_limit)))
                        .child(model.context_limit_input.clone())
                        .child(action("context-budget-save","Salvar orçamento de contexto",!busy,cx,|model,cx|model.save_context_limit(cx)))
                        .child(toggle("learning-enabled", "LEARNING CONSULTIVO", if snapshot.preparation.learning_enabled {"on"}else{"off"}, !busy,cx,|model,cx|model.toggle_learning(cx)))
                        .child(toggle("learning-refinement-mode", "REFINAMENTO DO GRAFO", snapshot.preparation.learning_refinement_mode.as_str(), !busy && snapshot.preparation.learning_enabled,cx,|model,cx|model.toggle_learning_refinement_mode(cx)))
                        .child(div().text_xs().text_color(theme::text_muted()).child("Learning roda após Done ou Failed. Observe audita sem criar propostas; Propose permite sugestões por IDs do host, mas toda escrita ainda exige aprovação humana e CAS."))
                        .child(toggle("brownfield-llm-budget", "BROWNFIELD · CHAMADAS POR PULSAÇÃO", snapshot.preparation.brownfield_llm_budget.to_string(), !busy,cx,|model,cx|model.cycle_brownfield_llm_budget(cx)))
                        .child(toggle("brownfield-expansion-radius", "BROWNFIELD · RAIO", format!("k={}", snapshot.preparation.brownfield_expansion_radius), !busy,cx,|model,cx|model.toggle_brownfield_expansion_radius(cx)))
                        .child(div().text_xs().text_color(theme::text_muted()).child("Enriquecimento é oportunista: 0 não chama LLM; k=1 usa vizinhos diretos e k=2 permite um segundo salto. Cada lote publica no máximo duas propostas.")),
                )
                .child(
                    card("RAG / YAML Graph")
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "Provider: {} · top K: {} · {} key(s) no keyring",
                            snapshot.config.embedding_provider.as_str(), snapshot.config.embedding_top_k,
                            snapshot.key_statuses.iter().filter(|status| status.has_api_key).count()
                        )))
                        .child(embedding_model)
                        .child(div().flex().gap_2()
                            .child(action("settings-embedding-top-k", "Alternar top K", !busy, cx, |model, cx| model.cycle_embedding_top_k(cx)))
                            .child(action("settings-reindex", "Reindexar workspace", !busy, cx, |model, cx| model.reindex_embeddings(cx)))),
                )
                .child(
                    card("Navegação adaptativa do Segundo Cérebro")
                        .child(div().text_xs().text_color(theme::text_muted()).child(
                            "Shadow mede a política sem alterar o prompt; On ativa expansão dinâmica e resolução exata de código.",
                        ))
                        .child(toggle("settings-adaptive-mode", "ATIVAÇÃO", adaptive_mode, !busy, cx, |model, cx| model.cycle_adaptive_context_mode(cx)))
                        .child(
                            div().grid().grid_cols(3).gap_2()
                                .child(toggle("settings-adaptive-initial-k", "K INICIAL", adaptive_policy.initial_k.to_string(), !busy, cx, |model, cx| model.cycle_dynamic_initial_k(cx)))
                                .child(toggle("settings-adaptive-max-k", "K MÁXIMO", adaptive_policy.max_k.to_string(), !busy, cx, |model, cx| model.cycle_dynamic_max_k(cx)))
                                .child(toggle("settings-adaptive-similarity", "SIMILARIDADE", format!("{:.2}", adaptive_policy.min_similarity), !busy, cx, |model, cx| model.cycle_dynamic_min_similarity(cx)))
                                .child(toggle("settings-adaptive-min-candidates", "MÍN. NOTAS", adaptive_policy.min_candidates_count.to_string(), !busy, cx, |model, cx| model.cycle_dynamic_min_candidates(cx)))
                                .child(toggle("settings-adaptive-max-notes", "MÁX. NOTAS", adaptive_policy.max_notes_budget.to_string(), !busy, cx, |model, cx| model.cycle_dynamic_max_notes(cx)))
                                .child(toggle("settings-adaptive-beam", "BEAM", adaptive_policy.beam_width.to_string(), !busy, cx, |model, cx| model.cycle_dynamic_beam_width(cx)))
                                .child(toggle("settings-adaptive-files", "ARQUIVOS", adaptive_policy.max_source_files.to_string(), !busy, cx, |model, cx| model.cycle_dynamic_max_source_files(cx)))
                                .child(toggle("settings-adaptive-code", "CÓDIGO", format!("{} chars", adaptive_policy.max_code_chars), !busy, cx, |model, cx| model.cycle_dynamic_max_code_chars(cx)))
                                .child(toggle("settings-adaptive-timeout", "TIMEOUT", format!("{} ms", adaptive_policy.timeout_ms), !busy, cx, |model, cx| model.cycle_dynamic_timeout(cx))),
                        ),
                )

        .into_any_element()
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

fn card(title: &'static str) -> gpui::Div {
    div()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
}

fn empty(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .text_sm()
        .text_color(theme::text_muted())
        .child(message)
}

fn banner(message: SharedString, error: bool) -> impl IntoElement {
    div()
        .p_3()
        .rounded_md()
        .bg(if error {
            theme::danger_soft()
        } else {
            theme::accent_soft()
        })
        .text_color(if error {
            theme::danger()
        } else {
            theme::accent()
        })
        .child(message)
}

fn action(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
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

fn toggle(
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
