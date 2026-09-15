//! Direct selection adapters. Options are read again at confirmation time.
use crate::{
    AppModel,
    agents::TerminalEventFilter,
    app_model::{RuntimeConfigUpdateExt, runtime_config_update},
    select_menu::Choice,
};
use gpui::{App, Context};
use hvc_core::{
    llm::{AgentRole, RoutingMode},
    terminals::{
        AgentExecutionMode, CliBackend, CliModelSelectionSource, SessionPolicy, ThinkingLevel,
    },
};

pub(crate) fn segments(id: &str) -> Option<Vec<(&'static str, &'static str)>> {
    if id.starts_with("agent-mode:") || id.starts_with("free-form-mode:") {
        return Some(vec![("api", "API Key Mode"), ("cli", "CLI Mode")]);
    }
    if id == "chat-transport" {
        return Some(vec![("api", "API"), ("cli", "CLI")]);
    }
    match id {
        "settings-context-strategy" => Some(vec![("graph", "Graph"), ("rag", "Graph + RAG")]),
        "learning-refinement-mode" => Some(vec![("observe", "Observe"), ("propose", "Propose")]),
        _ => None,
    }
}

const BACKENDS: [CliBackend; 5] = [
    CliBackend::Codex,
    CliBackend::Claude,
    CliBackend::Gemini,
    CliBackend::Opencode,
    CliBackend::Custom,
];
const FILTERS: [TerminalEventFilter; 7] = [
    TerminalEventFilter::All,
    TerminalEventFilter::Activity,
    TerminalEventFilter::Tool,
    TerminalEventFilter::Stdout,
    TerminalEventFilter::Stderr,
    TerminalEventFilter::Warning,
    TerminalEventFilter::Error,
];
fn backend(value: &str) -> Option<CliBackend> {
    BACKENDS.into_iter().find(|b| b.label() == value)
}
fn role(value: &str) -> Option<AgentRole> {
    AgentRole::ALL.into_iter().find(|r| r.as_str() == value)
}
fn modes(current: AgentExecutionMode) -> Vec<Choice> {
    vec![
        Choice::new("api", "API", current == AgentExecutionMode::ApiDirect),
        Choice::new("cli", "CLI", current == AgentExecutionMode::CliProcess),
    ]
}

pub(crate) fn custom_mode_reason(
    backend: CliBackend,
    executable: Option<&str>,
    acknowledged: bool,
) -> Option<&'static str> {
    if backend != CliBackend::Custom {
        None
    } else if executable.is_none_or(|path| path.trim().is_empty()) {
        Some("Save a Custom executable before enabling CLI.")
    } else if !acknowledged {
        Some("Acknowledge the saved Custom executable before enabling CLI.")
    } else {
        None
    }
}
fn numbers(current: impl ToString, values: impl IntoIterator<Item = impl ToString>) -> Vec<Choice> {
    let current = current.to_string();
    let mut values = values
        .into_iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    if !values.contains(&current) {
        values.push(current.clone());
    }
    values
        .into_iter()
        .map(|v| Choice::new(&v, &v, v == current))
        .collect()
}

impl AppModel {
    fn agent_is_running(&self, role: AgentRole, cx: &App) -> bool {
        self.agents.read(cx).snapshot.as_ref().is_some_and(|s| {
            s.agents.iter().any(|a| {
                a.config.agent_role == role && a.recent_runs.iter().any(|r| r.status.is_active())
            })
        })
    }
    fn free_form_is_running(&self, id: &str, cx: &App) -> bool {
        self.agents.read(cx).snapshot.as_ref().is_some_and(|s| {
            s.free_form_slots
                .iter()
                .any(|a| a.slot.id == id && a.recent_runs.iter().any(|r| r.status.is_active()))
        })
    }
    pub(crate) fn choice_options(&self, id: &str, cx: &App) -> Vec<Choice> {
        if id.starts_with("usage-") {
            return self.usage_choices(id, cx);
        }
        let (field, key) = id.split_once(':').unwrap_or((id, ""));
        if field == "agent-filter" || field == "free-form-filter" {
            let filter_key = if field == "free-form-filter" {
                format!("free-form:{key}")
            } else {
                key.to_string()
            };
            let current = self
                .agents
                .read(cx)
                .event_filters
                .get(&filter_key)
                .copied()
                .unwrap_or(TerminalEventFilter::All);
            return FILTERS
                .into_iter()
                .map(|f| Choice::new(f.label(), f.label(), f == current))
                .collect();
        }
        if matches!(
            field,
            "agent-mode"
                | "agent-backend"
                | "agent-thinking"
                | "free-form-mode"
                | "free-form-backend"
                | "free-form-thinking"
                | "free-form-session"
        ) {
            let data = if field.starts_with("agent-") {
                role(key)
                    .and_then(|role| self.current_agent_config(role, cx))
                    .map(|c| (c.execution_mode, c.backend, c.model, c.model_profile, None))
            } else {
                self.current_free_form_slot(key, cx).map(|c| {
                    (
                        c.execution_mode,
                        c.backend,
                        c.model,
                        c.model_profile,
                        Some(c.session_policy),
                    )
                })
            };
            let Some((mode, selected_backend, model, profile, session)) = data else {
                return vec![];
            };
            if field.ends_with("-mode") {
                let reason = if field.starts_with("agent-") {
                    role(key)
                        .and_then(|role| self.current_agent_config(role, cx))
                        .and_then(|c| {
                            custom_mode_reason(
                                c.backend,
                                c.custom_executable.as_deref(),
                                c.custom_risk_ack,
                            )
                        })
                } else {
                    self.current_free_form_slot(key, cx).and_then(|c| {
                        custom_mode_reason(
                            c.backend,
                            c.custom_executable.as_deref(),
                            c.custom_risk_ack,
                        )
                    })
                };
                let mut options = modes(mode);
                options[1].reason = reason.map(str::to_string);
                return options;
            }
            if field.ends_with("-backend") {
                return BACKENDS
                    .into_iter()
                    .map(|b| Choice::new(b.label(), b.label(), b == selected_backend))
                    .collect();
            }
            if field.ends_with("-session") {
                return vec![
                    Choice::new(
                        "fresh",
                        "New per run",
                        session == Some(SessionPolicy::FreshPerRun),
                    ),
                    Choice::new(
                        "resume",
                        "Explicit resume",
                        session == Some(SessionPolicy::ResumeExplicit),
                    ),
                ]
                .into_iter()
                .chain(
                    (session == Some(SessionPolicy::ResumePerPulsation))
                        .then(|| Choice::new("pulsation", "Per pulsation (current)", true)),
                )
                .collect();
            }
            return std::iter::once(Choice::new("", "Default", profile.is_none()))
                .chain(
                    self.model_profiles(selected_backend, model.as_deref(), cx)
                        .into_iter()
                        .map(|p| Choice::new(&p, &p, profile.as_ref() == Some(&p))),
                )
                .collect();
        }
        let chat = self.chat.read(cx);
        match field {
            "chat-transport" => {
                return modes(
                    if chat.selected_transport == hvc_core::chat_service::ChatTransportKind::Api {
                        AgentExecutionMode::ApiDirect
                    } else {
                        AgentExecutionMode::CliProcess
                    },
                );
            }
            "chat-cli-backend" => {
                return BACKENDS[..4]
                    .iter()
                    .map(|b| Choice::new(b.label(), b.label(), *b == chat.selected_cli_backend))
                    .collect();
            }
            "chat-cli-profile" => {
                return std::iter::once(Choice::new(
                    "",
                    "Default",
                    chat.selected_cli_model_profile.is_none(),
                ))
                .chain(
                    self.model_profiles(
                        chat.selected_cli_backend,
                        chat.selected_cli_model.as_deref(),
                        cx,
                    )
                    .into_iter()
                    .map(|p| {
                        Choice::new(&p, &p, chat.selected_cli_model_profile.as_ref() == Some(&p))
                    }),
                )
                .collect();
            }
            _ => {}
        }
        if matches!(field, "chat-cycle-connection" | "chat-connections") {
            return chat
                .eligible_connections()
                .map(|c| {
                    Choice::new(
                        &c.id,
                        &c.display_name,
                        chat.selected_connection_id
                            .as_ref()
                            .is_some_and(|id| id.as_ref() == c.id),
                    )
                })
                .collect();
        }
        let settings = self.settings.read(cx);
        let Some(snapshot) = settings.snapshot.as_ref() else {
            return vec![];
        };
        let config = &snapshot.config;
        let policy = &config.dynamic_context_policy;
        let prep = &snapshot.preparation;
        match field {
            "chat-cycle-connection" | "chat-connections" | "api-cycle-connection" => snapshot
                .connections
                .iter()
                .map(|c| {
                    Choice::new(
                        &c.id,
                        &c.display_name,
                        if field.starts_with("chat") {
                            chat.selected_connection_id
                                .as_ref()
                                .is_some_and(|id| id.as_ref() == c.id)
                        } else {
                            settings
                                .selected_connection_id
                                .as_ref()
                                .is_some_and(|id| id.as_ref() == c.id)
                        },
                    )
                })
                .collect(),
            "api-cycle-route" => snapshot
                .routes
                .iter()
                .map(|r| {
                    Choice::new(
                        &r.id,
                        &r.name,
                        settings
                            .selected_route_id
                            .as_ref()
                            .is_some_and(|id| id.as_ref() == r.id),
                    )
                })
                .collect(),
            "settings-provider" | "api-provider" => snapshot
                .profiles
                .iter()
                .map(|p| {
                    Choice::new(
                        &p.id,
                        &p.display_name,
                        settings.connection_provider_id.as_ref() == p.id,
                    )
                })
                .collect(),
            "settings-route-strategy" => ["priority", "lkgp", "score"]
                .into_iter()
                .map(|v| Choice::new(v, v, settings.route_strategy.as_ref() == v))
                .collect(),
            "settings-routing-mode" => [RoutingMode::Off, RoutingMode::Shadow, RoutingMode::On]
                .into_iter()
                .map(|v| Choice::new(v.as_str(), v.as_str(), config.routing_v2_mode == v))
                .collect(),
            "settings-adaptive-mode" => ["off", "shadow", "on"]
                .into_iter()
                .map(|v| Choice::new(v, v, config.adaptive_context_mode.as_str() == v))
                .collect(),
            "settings-context-strategy" => vec![
                Choice::new(
                    "graph",
                    "Graph",
                    config.context_strategy == hvc_core::agent_runtime::ContextStrategy::YamlGraph,
                ),
                Choice::new(
                    "rag",
                    "Graph + RAG",
                    config.context_strategy
                        == hvc_core::agent_runtime::ContextStrategy::YamlGraphRag,
                ),
            ],
            "learning-refinement-mode" => ["observe", "propose"]
                .into_iter()
                .map(|v| Choice::new(v, v, prep.learning_refinement_mode.as_str() == v))
                .collect(),
            "composition-max-in-flight" => {
                let current = snapshot
                    .composition
                    .as_ref()
                    .map(|c| c.settings.max_in_flight_units)
                    .unwrap_or(1);
                // 2 became selectable with the M6 parity gate (parallel result
                // equal to the sequential one); it stays off by default.
                vec![
                    Choice::new("1", "1 (sequential)", current == 1),
                    Choice::new("2", "2 (independent units together)", current == 2),
                ]
            }
            "composition-override-kind" => hvc_core::orchestration::composition::ExecutorKind::ALL
                .iter()
                .map(|kind| {
                    Choice::new(
                        kind.as_str(),
                        kind.as_str(),
                        settings.composition_override_kind.as_ref() == kind.as_str(),
                    )
                })
                .collect(),
            "composition-override-mode" => ["api_direct", "cli_process"]
                .into_iter()
                .map(|v| Choice::new(v, v, settings.composition_override_mode.as_ref() == v))
                .collect(),
            "composition-override-backend" => BACKENDS
                .iter()
                .map(|b| {
                    Choice::new(
                        b.as_str(),
                        b.label(),
                        settings.composition_override_backend.as_ref() == b.as_str(),
                    )
                })
                .collect(),
            "composition-override-thinking" => ["default", "low", "medium", "high"]
                .into_iter()
                .map(|v| Choice::new(v, v, settings.composition_override_thinking.as_ref() == v))
                .collect(),
            "brownfield-llm-budget" => numbers(prep.brownfield_llm_budget, [0, 1, 2, 4, 8]),
            "brownfield-expansion-radius" => numbers(prep.brownfield_expansion_radius, [1, 2]),
            "settings-embedding-top-k" => numbers(config.embedding_top_k, [3, 5, 10, 20]),
            "settings-adaptive-initial-k" => numbers(policy.initial_k, 1..=policy.max_k),
            "settings-adaptive-max-k" => numbers(policy.max_k, 1..=3),
            "settings-adaptive-similarity" => {
                numbers(policy.min_similarity, [0.50, 0.65, 0.75, 0.85])
            }
            "settings-adaptive-min-candidates" => {
                numbers(policy.min_candidates_count, 1..=policy.max_notes_budget)
            }
            "settings-adaptive-max-notes" => numbers(policy.max_notes_budget, [4, 8, 12, 20]),
            "settings-adaptive-beam" => numbers(policy.beam_width, [4, 8, 12]),
            "settings-adaptive-files" => numbers(policy.max_source_files, [3, 5, 8, 12]),
            "settings-adaptive-code" => {
                numbers(policy.max_code_chars, [16000, 32000, 48000, 80000])
            }
            "settings-adaptive-timeout" => {
                numbers(policy.timeout_ms, [500, 1000, 1500, 3000, 5000])
            }
            "settings-route-limits" => {
                let current = format!(
                    "{}/{}/{}",
                    settings.route_max_attempts,
                    settings.route_attempt_timeout_ms,
                    settings.route_timeout_ms
                );
                let mut options = [(1, 15000, 30000), (2, 30000, 60000), (3, 45000, 120000)]
                    .into_iter()
                    .map(|(a, t, r)| {
                        let v = format!("{a}/{t}/{r}");
                        Choice::new(
                            &v,
                            format!("{a} attempts · {t} ms/attempt · {r} ms total"),
                            v == current,
                        )
                    })
                    .collect::<Vec<_>>();
                if !options.iter().any(|o| o.selected) {
                    options.push(Choice::new(&current, format!("Current: {current}"), true));
                }
                options
            }
            "settings-binding-role" => [
                "intake_router",
                "task_specifier",
                "contract_agent",
                "backend_agent",
                "test_agent",
                "integrator",
                "learning_agent",
            ]
            .into_iter()
            .map(|r| Choice::new(r, r, settings.binding_agent_role.as_ref() == r))
            .collect(),
            "settings-binding-stage" => [
                "Intake",
                "Specification",
                "Contract",
                "Code",
                "Test",
                "Integrator",
                "Learning",
            ]
            .into_iter()
            .map(|r| Choice::new(r, r, settings.binding_stage.as_ref() == r))
            .collect(),
            _ => vec![],
        }
    }

    pub(crate) fn apply_choice(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        // Re-read the live option set. Stale menus cannot select vanished or disallowed options.
        let Some(choice) = self
            .choice_options(id, cx)
            .into_iter()
            .find(|c| c.value == value)
        else {
            return;
        };
        if choice.selected || choice.reason.is_some() {
            return;
        }
        if id.starts_with("usage-") {
            self.apply_usage_choice(id, value, cx);
            return;
        }
        let (field, key) = id.split_once(':').unwrap_or((id, ""));
        if field == "agent-filter" || field == "free-form-filter" {
            if let Some(filter) = FILTERS.into_iter().find(|f| f.label() == value) {
                self.agents.update(cx, |a, _| {
                    a.event_filters.insert(
                        if field == "free-form-filter" {
                            format!("free-form:{key}")
                        } else {
                            key.into()
                        },
                        filter,
                    );
                });
                cx.notify();
            }
            return;
        }
        if field.starts_with("agent-") || field.starts_with("free-form-") {
            if self.agents.read(cx).busy {
                return;
            }
            let mode = if value == "cli" {
                AgentExecutionMode::CliProcess
            } else {
                AgentExecutionMode::ApiDirect
            };
            if field.starts_with("agent-") {
                let Some(role) = role(key) else {
                    return;
                };
                let Some(mut config) = self.current_agent_config(role, cx) else {
                    return;
                };
                if self.agent_is_running(role, cx) {
                    return;
                }
                match field {
                    "agent-mode" => config.execution_mode = mode,
                    "agent-backend" => {
                        let Some(b) = backend(value) else {
                            return;
                        };
                        config.backend = b;
                        config.model = None;
                        config.model_profile = None;
                        config.model_source = CliModelSelectionSource::Default;
                        config.thinking_level = ThinkingLevel::Default;
                        if b == CliBackend::Custom {
                            config.execution_mode = AgentExecutionMode::ApiDirect;
                        }
                    }
                    "agent-thinking" => {
                        config.model_profile = (!value.is_empty()).then(|| value.into());
                        config.thinking_level = ThinkingLevel::Default;
                    }
                    _ => return,
                }
                self.save_agent_config(config, cx);
            } else {
                let Some(mut slot) = self.current_free_form_slot(key, cx) else {
                    return;
                };
                if self.free_form_is_running(key, cx) {
                    return;
                }
                match field {
                    "free-form-mode" => slot.execution_mode = mode,
                    "free-form-backend" => {
                        let Some(b) = backend(value) else {
                            return;
                        };
                        slot.backend = b;
                        slot.model = None;
                        slot.model_profile = None;
                        slot.model_source = CliModelSelectionSource::Default;
                        slot.thinking_level = ThinkingLevel::Default;
                        if b == CliBackend::Custom {
                            slot.execution_mode = AgentExecutionMode::ApiDirect;
                            slot.custom_risk_ack = false;
                        }
                    }
                    "free-form-thinking" => {
                        slot.model_profile = (!value.is_empty()).then(|| value.into());
                        slot.thinking_level = ThinkingLevel::Default;
                    }
                    "free-form-session" => {
                        slot.session_policy = if value == "fresh" {
                            SessionPolicy::FreshPerRun
                        } else {
                            SessionPolicy::ResumeExplicit
                        }
                    }
                    _ => return,
                }
                self.save_free_form_slot(slot, cx);
            }
            return;
        }
        if field.starts_with("chat-") {
            if self.chat.read(cx).streaming || self.chat.read(cx).busy {
                return;
            }
            match field {
                "chat-transport" => self.set_chat_transport(
                    if value == "api" {
                        hvc_core::chat_service::ChatTransportKind::Api
                    } else {
                        hvc_core::chat_service::ChatTransportKind::Cli
                    },
                    cx,
                ),
                "chat-cli-backend" => {
                    if let Some(b) = backend(value) {
                        self.set_chat_cli_backend(b, cx)
                    }
                }
                "chat-cli-profile" => {
                    self.set_chat_cli_profile((!value.is_empty()).then(|| value.into()), cx)
                }
                "chat-cycle-connection" | "chat-connections" => {
                    self.set_chat_connection(value.into(), cx)
                }
                _ => {}
            }
            return;
        }
        if self.settings.read(cx).busy || (field.starts_with("api-") && self.agents.read(cx).busy) {
            return;
        }
        match field {
            "api-cycle-route" => self.select_settings_route(value.into(), cx),
            "api-cycle-connection" => self.select_settings_connection(value.into(), cx),
            "api-provider" | "settings-provider" => self.select_settings_provider(value.into(), cx),
            "settings-route-strategy" => self.set_route_strategy(value.into(), cx),
            "settings-routing-mode" => self.set_routing_mode(
                match value {
                    "on" => RoutingMode::On,
                    "shadow" => RoutingMode::Shadow,
                    _ => RoutingMode::Off,
                },
                cx,
            ),
            "settings-adaptive-mode" => self.update_runtime_config(
                runtime_config_update().with_adaptive_context_mode(
                    hvc_core::agent_runtime::AdaptiveContextMode::parse(value).unwrap(),
                ),
                "Adaptive context mode updated.",
                cx,
            ),
            "settings-context-strategy" => self.toggle_context_strategy(cx),
            "learning-refinement-mode" => self.toggle_learning_refinement_mode(cx),
            "composition-max-in-flight" => {
                self.set_composition_max_in_flight(value.parse().unwrap_or(1), cx)
            }
            "composition-override-kind" => self.settings.update(cx, |s, _| {
                s.composition_override_kind = value.to_owned().into()
            }),
            "composition-override-mode" => self.settings.update(cx, |s, _| {
                s.composition_override_mode = value.to_owned().into()
            }),
            "composition-override-backend" => self.settings.update(cx, |s, _| {
                s.composition_override_backend = value.to_owned().into()
            }),
            "composition-override-thinking" => self.settings.update(cx, |s, _| {
                s.composition_override_thinking = value.to_owned().into()
            }),
            "brownfield-expansion-radius" => self.toggle_brownfield_expansion_radius(cx),
            "brownfield-llm-budget" => self.set_brownfield_budget(value.parse().unwrap(), cx),
            "settings-embedding-top-k" => self.update_runtime_config(
                runtime_config_update().with_embedding_top_k(value.parse().unwrap()),
                "Embedding Top K updated.",
                cx,
            ),
            "settings-binding-role" => {
                self.settings
                    .update(cx, |s, _| s.binding_agent_role = value.to_string().into());
                cx.notify();
            }
            "settings-binding-stage" => {
                self.settings
                    .update(cx, |s, _| s.binding_stage = value.to_string().into());
                cx.notify();
            }
            "settings-route-limits" => {
                let parts = value
                    .split('/')
                    .map(|n| n.parse::<u64>().unwrap())
                    .collect::<Vec<_>>();
                self.settings.update(cx, |s, _| {
                    s.route_max_attempts = parts[0] as _;
                    s.route_attempt_timeout_ms = parts[1];
                    s.route_timeout_ms = parts[2];
                });
                cx.notify();
            }
            _ if field.starts_with("settings-adaptive-") => {
                self.update_dynamic_context_policy(
                    |p| match field {
                        "settings-adaptive-initial-k" => p.initial_k = value.parse().unwrap(),
                        "settings-adaptive-max-k" => {
                            p.max_k = value.parse().unwrap();
                            p.initial_k = p.initial_k.min(p.max_k);
                        }
                        "settings-adaptive-similarity" => p.min_similarity = value.parse().unwrap(),
                        "settings-adaptive-min-candidates" => {
                            p.min_candidates_count = value.parse().unwrap()
                        }
                        "settings-adaptive-max-notes" => {
                            p.max_notes_budget = value.parse().unwrap()
                        }
                        "settings-adaptive-beam" => p.beam_width = value.parse().unwrap(),
                        "settings-adaptive-files" => p.max_source_files = value.parse().unwrap(),
                        "settings-adaptive-code" => p.max_code_chars = value.parse().unwrap(),
                        "settings-adaptive-timeout" => p.timeout_ms = value.parse().unwrap(),
                        _ => {}
                    },
                    cx,
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::*;
    use gpui::AppContext;
    use hvc_core::llm::{
        AgentExecutionTarget, FreeFormProviderRouteBinding,
        UpsertFreeFormProviderRouteBindingRequest,
    };
    use hvc_core::persistence_service::{
        ResolveCliFallbackRequest, ResolveCliFallbackResponse, StartFreeFormApiRunRequest,
        StartFreeFormApiRunResponse,
    };
    use hvc_core::terminals::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    struct RecordingBackend {
        calls: Mutex<Vec<UpdateCliAgentConfigRequest>>,
        config: Mutex<CliAgentConfig>,
        fail: AtomicBool,
    }
    #[allow(unused_variables)]
    impl AgentsBackend for RecordingBackend {
        fn snapshot(&self, project_id: String) -> AgentsFuture<AgentsDashboardSnapshot> {
            panic!("Unexpected backend operation")
        }
        fn refresh_snapshot(&self, project_id: String) -> AgentsFuture<AgentsDashboardSnapshot> {
            panic!("Unexpected backend operation")
        }
        fn model_catalog(
            &self,
            project_id: String,
            backend: CliBackend,
            refresh: bool,
        ) -> AgentsFuture<CliModelCatalogSnapshot> {
            panic!("Unexpected backend operation")
        }
        fn start_manual(&self, request: StartManualRunRequest) -> AgentsFuture<TerminalRun> {
            panic!("Unexpected backend operation")
        }
        fn start_free_form(&self, request: StartFreeFormRunRequest) -> AgentsFuture<TerminalRun> {
            panic!("Unexpected backend operation")
        }
        fn start_free_form_api(
            &self,
            request: StartFreeFormApiRunRequest,
        ) -> AgentsFuture<StartFreeFormApiRunResponse> {
            panic!("Unexpected backend operation")
        }
        fn cancel_api_execution(&self, execution_id: String) -> AgentsFuture<bool> {
            panic!("Unexpected backend operation")
        }
        fn upsert_free_form_binding(
            &self,
            request: UpsertFreeFormProviderRouteBindingRequest,
        ) -> AgentsFuture<FreeFormProviderRouteBinding> {
            panic!("Unexpected backend operation")
        }
        fn resume_manual(&self, request: ResumeManualRunRequest) -> AgentsFuture<TerminalRun> {
            panic!("Unexpected backend operation")
        }
        fn create_free_form(
            &self,
            request: CreateFreeFormSlotRequest,
        ) -> AgentsFuture<hvc_core::terminals::TerminalSlot> {
            panic!("Unexpected backend operation")
        }
        fn update_free_form(
            &self,
            request: UpdateFreeFormSlotRequest,
        ) -> AgentsFuture<hvc_core::terminals::TerminalSlot> {
            panic!("Unexpected backend operation")
        }
        fn delete_free_form(
            &self,
            project_id: String,
            slot_id: String,
            expected_version: u64,
        ) -> AgentsFuture<()> {
            panic!("Unexpected backend operation")
        }
        fn reorder_free_form(&self, project_id: String, slot_ids: Vec<String>) -> AgentsFuture<()> {
            panic!("Unexpected backend operation")
        }
        fn cancel(&self, run_id: String) -> AgentsFuture<bool> {
            panic!("Unexpected backend operation")
        }
        fn events(
            &self,
            run_id: String,
            after_sequence: u64,
            limit: usize,
        ) -> AgentsFuture<Vec<TerminalEvent>> {
            panic!("Unexpected backend operation")
        }
        fn clear_history(
            &self,
            project_id: String,
            role: Option<AgentRole>,
        ) -> AgentsFuture<usize> {
            panic!("Unexpected backend operation")
        }
        fn clear_slot_history(&self, project_id: String, slot_id: String) -> AgentsFuture<usize> {
            panic!("Unexpected backend operation")
        }
        fn export_history(&self, project_id: String, run_id: String) -> AgentsFuture<String> {
            panic!("Unexpected backend operation")
        }
        fn export_api_execution(
            &self,
            project_id: String,
            target: AgentExecutionTarget,
            execution_id: String,
        ) -> AgentsFuture<String> {
            panic!("Unexpected backend operation")
        }
        fn resolve_fallback(
            &self,
            request: ResolveCliFallbackRequest,
        ) -> AgentsFuture<ResolveCliFallbackResponse> {
            panic!("Unexpected backend operation")
        }
        fn update_config(
            &self,
            request: UpdateCliAgentConfigRequest,
        ) -> AgentsFuture<CliAgentConfig> {
            let mut config = self.config.lock().unwrap().clone();
            config.backend = request.backend;
            config.execution_mode = request.execution_mode;
            config.model = request.model.clone();
            config.model_profile = request.model_profile.clone();
            config.version += 1;
            self.calls.lock().unwrap().push(request);
            let fail = self.fail.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    Err(crate::errors::UiCommandError::from_chat_parts(
                        "save_failed",
                        "Fixture failure",
                    ))
                } else {
                    Ok(config)
                }
            })
        }
    }
    #[gpui::test]
    fn direct_selection_saves_once_noop_preserves_model_and_failure_preserves_configuration(
        cx: &mut gpui::TestAppContext,
    ) {
        let model = cx.new(crate::layout_tests::fixture);
        let backend = model.update(cx, |m, cx| {
            m.agents.update(cx, |a, _| {
                let c = &mut a.snapshot.as_mut().unwrap().agents[0].config;
                c.model = Some("preserved-model".into());
                c.model_profile = Some("high".into());
            });
            let config = m
                .current_agent_config(AgentRole::TaskSpecifier, cx)
                .unwrap();
            let backend = Arc::new(RecordingBackend {
                calls: Mutex::new(vec![]),
                config: Mutex::new(config),
                fail: AtomicBool::new(false),
            });
            m.agents_backend = Some(backend.clone());
            m.apply_choice(
                "agent-backend:task_specifier",
                CliBackend::Codex.label(),
                cx,
            );
            backend
        });
        cx.run_until_parked();
        assert!(backend.calls.lock().unwrap().is_empty());
        model.read_with(cx, |m, cx| {
            assert_eq!(
                m.current_agent_config(AgentRole::TaskSpecifier, cx)
                    .unwrap()
                    .model
                    .as_deref(),
                Some("preserved-model")
            )
        });
        model.update(cx, |m, cx| {
            m.apply_choice(
                "agent-backend:task_specifier",
                CliBackend::Custom.label(),
                cx,
            )
        });
        cx.run_until_parked();
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].backend, CliBackend::Custom);
        assert!(calls[0].model.is_none());
        assert!(calls[0].model_profile.is_none());
        assert_eq!(calls[0].execution_mode, AgentExecutionMode::ApiDirect);
        drop(calls);
        model.update(cx, |m, cx| {
            m.apply_choice(
                "agent-backend:task_specifier",
                CliBackend::Custom.label(),
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
        backend.fail.store(true, Ordering::SeqCst);
        model.update(cx, |m, cx| {
            m.apply_choice(
                "agent-backend:task_specifier",
                CliBackend::Claude.label(),
                cx,
            )
        });
        cx.run_until_parked();
        assert_eq!(backend.calls.lock().unwrap().len(), 2);
        model.read_with(cx, |m, cx| {
            assert_eq!(
                m.current_agent_config(AgentRole::TaskSpecifier, cx)
                    .unwrap()
                    .backend,
                CliBackend::Custom
            );
            assert!(m.agents.read(cx).error.is_some());
        });
    }
    #[gpui::test]
    fn options_preserve_nonpreset_values_and_filter_keys(cx: &mut gpui::TestAppContext) {
        let model = cx.new(crate::layout_tests::fixture);
        model.update(cx, |m, cx| {
            m.settings.update(cx, |s, _| {
                s.snapshot
                    .as_mut()
                    .unwrap()
                    .config
                    .dynamic_context_policy
                    .timeout_ms = 2345
            });
            let options = m.choice_options("settings-adaptive-timeout", cx);
            assert!(options.iter().any(|c| c.value == "2345" && c.selected));
            m.apply_choice("free-form-filter:slot-a", "error", cx);
            assert_eq!(
                m.agents.read(cx).event_filters.get("free-form:slot-a"),
                Some(&TerminalEventFilter::Error)
            );
            m.chat.update(cx, |chat, _| {
                chat.selected_cli_backend = CliBackend::Codex;
                chat.selected_cli_model = Some("existing".into());
            });
            m.apply_choice("chat-cli-backend", CliBackend::Codex.label(), cx);
            assert_eq!(
                m.chat.read(cx).selected_cli_model.as_deref(),
                Some("existing")
            );
        });
    }
}
