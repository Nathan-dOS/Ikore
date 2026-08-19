use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use jsonschema::validator_for;
use reqwest::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::adapter::{AdapterCall, LlmAdapter, ProtocolAdapter, TokenDeltaSink};
use super::catalog::estimate_cost_usd;
use super::credentials::CredentialStore;
use super::domain::{
    Clock, GatewayFuture, JitterSource, LlmEventEnvelope, LlmEventSink, LlmGateway, LlmRequest,
    LlmResponse, LlmStreamEvent, ProviderProtocol, ProviderRoute, ResponseFormat, RoutingMode,
};
use super::error::{LlmError, LlmErrorKind};
use super::profiles::profile_by_id;
use super::repository::TestProviderConnectionResponse;
use super::repository::{endpoint_origin, LlmRepository, ResolvedRouteTarget, SqliteLlmRepository};
use super::resilience::{cooldown_ms, eligible_at};

pub struct ProviderRouter {
    repository: SqliteLlmRepository,
    credentials: Arc<dyn CredentialStore>,
    adapters: HashMap<ProviderProtocol, Arc<dyn LlmAdapter>>,
    event_sink: Arc<dyn LlmEventSink>,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn JitterSource>,
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ProviderRouter {
    pub fn new(
        repository: SqliteLlmRepository,
        client: Client,
        credentials: Arc<dyn CredentialStore>,
        event_sink: Arc<dyn LlmEventSink>,
        clock: Arc<dyn Clock>,
        jitter: Arc<dyn JitterSource>,
    ) -> Self {
        let mut adapters: HashMap<ProviderProtocol, Arc<dyn LlmAdapter>> = HashMap::new();
        for protocol in [
            ProviderProtocol::OpenaiResponses,
            ProviderProtocol::OpenaiChat,
            ProviderProtocol::AnthropicMessages,
        ] {
            adapters.insert(
                protocol,
                Arc::new(ProtocolAdapter::new(client.clone(), protocol)),
            );
        }
        Self {
            repository,
            credentials,
            adapters,
            event_sink,
            clock,
            jitter,
            semaphores: Mutex::new(HashMap::new()),
        }
    }

    pub async fn test_connection(
        &self,
        connection_id: &str,
    ) -> Result<TestProviderConnectionResponse, LlmError> {
        let connection = self.repository.find_resolved_connection(connection_id)?;
        let profile = profile_by_id(connection.provider_id.as_str()).ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Profile da conexão não existe.",
            )
        })?;
        let credential = if matches!(profile.auth_style, super::profiles::AuthStyle::None) {
            None
        } else {
            self.credentials.get(&connection.credential_ref)?
        };
        if credential.is_none() && !matches!(profile.auth_style, super::profiles::AuthStyle::None) {
            return Err(
                LlmError::new(LlmErrorKind::MissingApiKey, "Conexão sem API key.").with_target(
                    &profile.id,
                    connection.id.as_str(),
                    &connection.default_model,
                ),
            );
        }
        let request = LlmRequest {
            execution_id: uuid::Uuid::new_v4().to_string().into(),
            generation_id: uuid::Uuid::new_v4().to_string().into(),
            project_id: "connection-test".to_string(),
            pulsation_id: None,
            job_id: None,
            job_chain: "connection-test".to_string(),
            agent_role: super::domain::AgentRole::IntakeRouter,
            stage: "connection_test".to_string(),
            system_prompt: "Responda de forma concisa.".to_string(),
            user_prompt: "Responda apenas OK.".to_string(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 8,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(30),
            stream: false,
            legacy_provider: profile.id.clone(),
            legacy_model: connection.default_model.clone(),
        };
        let adapter = self.adapters.get(&profile.protocol).ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Adapter de protocolo não registrado.",
            )
        })?;
        let started = Instant::now();
        let output = timeout(
            Duration::from_secs(30),
            adapter.execute(
                AdapterCall {
                    profile: &profile,
                    connection: &connection,
                    model: &connection.default_model,
                    credential: credential.as_deref(),
                    request: &request,
                },
                &CancellationToken::new(),
                &NoopDeltaSink,
            ),
        )
        .await
        .map_err(|_| {
            LlmError::new(
                LlmErrorKind::Timeout,
                "Teste da conexão excedeu 30 segundos.",
            )
        })??;
        Ok(TestProviderConnectionResponse {
            connection_id: connection.id.0,
            reachable: !output.content.trim().is_empty(),
            provider_id: profile.id,
            model: connection.default_model,
            latency_ms: started.elapsed().as_millis() as u64,
            message: "Conexão respondeu com sucesso.".to_string(),
        })
    }

    async fn execute_inner(
        &self,
        request: &LlmRequest,
        cancellation: &CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        if cancellation.is_cancelled() {
            return Err(LlmError::new(
                LlmErrorKind::Cancelled,
                "Execução LLM cancelada.",
            ));
        }
        let mode = RoutingMode::parse(&self.repository.routing_mode()?);
        let bound_route = self.repository.resolve_route(
            &request.project_id,
            request.agent_role,
            &request.stage,
        )?;
        let bound_targets = self.repository.resolved_targets(&bound_route.id)?;
        let affinity_hash = affinity_hash(request);
        let bound_decision = self.repository.decide(
            &bound_route,
            &bound_targets,
            &affinity_hash,
            self.clock.now(),
        )?;

        let (execution_route, mut decision, mut attempt_targets, model_override) = match mode {
            RoutingMode::On => (
                bound_route.clone(),
                bound_decision,
                ordered_targets(
                    &bound_targets,
                    &bound_route,
                    &self.repository,
                    &affinity_hash,
                    self.clock.now(),
                )?,
                None,
            ),
            RoutingMode::Shadow => {
                let legacy = self.legacy_execution(request)?;
                (
                    bound_route.clone(),
                    bound_decision,
                    legacy.0,
                    Some(legacy.1),
                )
            }
            RoutingMode::Off => {
                let legacy_route = self
                    .repository
                    .list_routes()?
                    .into_iter()
                    .find(|route| route.id == "legacy-default")
                    .ok_or_else(|| {
                        LlmError::new(LlmErrorKind::Persistence, "Rota legacy-default ausente.")
                    })?;
                let targets = self.repository.resolved_targets(&legacy_route.id)?;
                let legacy_decision = self.repository.decide(
                    &legacy_route,
                    &targets,
                    &affinity_hash,
                    self.clock.now(),
                )?;
                let legacy = self.legacy_execution(request)?;
                (legacy_route, legacy_decision, legacy.0, Some(legacy.1))
            }
        };

        let eligibility_targets = if mode == RoutingMode::On {
            bound_targets.clone()
        } else {
            attempt_targets.clone()
        };
        let execution_gates =
            self.runtime_gate_reasons(&attempt_targets, request, mode == RoutingMode::On)?;
        let decision_gates = if mode == RoutingMode::Shadow {
            self.runtime_gate_reasons(&bound_targets, request, true)?
        } else {
            execution_gates.clone()
        };
        let missing_credential_error = attempt_targets.iter().find_map(|target| {
            (execution_gates
                .get(target.connection.id.as_str())
                .is_some_and(|reason| reason == "missing_api_key"))
            .then(|| {
                LlmError::new(LlmErrorKind::MissingApiKey, "Conexão sem API key.").with_target(
                    target.connection.provider_id.as_str(),
                    target.connection.id.as_str(),
                    if target.target.model_id.trim().is_empty() {
                        &target.connection.default_model
                    } else {
                        &target.target.model_id
                    },
                )
            })
        });
        attempt_targets
            .retain(|target| !execution_gates.contains_key(target.connection.id.as_str()));
        apply_runtime_gates_to_decision(&mut decision, &decision_gates);

        self.repository.create_execution(
            request.execution_id.as_str(),
            request.generation_id.as_str(),
            &request.project_id,
            request.pulsation_id.as_deref(),
            request.job_id.as_deref(),
            &execution_route,
            request.agent_role,
            &request.stage,
            mode.as_str(),
            &decision,
        )?;
        let emitter = ExecutionEmitter::new(
            self.event_sink.clone(),
            request.execution_id.0.clone(),
            request.generation_id.0.clone(),
        );
        emitter.emit(LlmStreamEvent::RouteStarted {
            route_id: execution_route.id.clone(),
        });

        if attempt_targets.is_empty() {
            let next_eligible_at = self
                .repository
                .next_eligible_at(&eligibility_targets, self.clock.now())?;
            let all_missing_credentials = !decision.candidates.is_empty()
                && decision
                    .candidates
                    .iter()
                    .all(|candidate| candidate.gate_reason.as_deref() == Some("missing_api_key"));
            let mut error = if mode != RoutingMode::On || all_missing_credentials {
                missing_credential_error.unwrap_or_else(|| {
                    LlmError::new(
                        LlmErrorKind::AllTargetsExhausted,
                        "Nenhum alvo LLM está elegível nesta rota.",
                    )
                })
            } else {
                let mut error = LlmError::new(
                    LlmErrorKind::AllTargetsExhausted,
                    "Nenhum alvo LLM está elegível nesta rota.",
                );
                if let Some(missing) = missing_credential_error {
                    error.causes.push(missing.safe_cause());
                }
                error
            };
            error.next_eligible_at = next_eligible_at.clone();
            for candidate in &decision.candidates {
                if let Some(reason) = &candidate.gate_reason {
                    error.causes.push(format!(
                        "{}@{}: {}",
                        candidate.connection_id, candidate.model_id, reason
                    ));
                }
            }
            self.repository.finish_execution(
                request.execution_id.as_str(),
                "failed",
                next_eligible_at.as_deref(),
                None,
                false,
            )?;
            emitter.emit(LlmStreamEvent::RouteFailed {
                error_code: error.kind.code().to_string(),
                next_eligible_at,
            });
            return Err(error);
        }

        let route_deadline = request
            .deadline
            .min(Instant::now() + Duration::from_millis(execution_route.route_timeout_ms));
        let max_attempts = if mode == RoutingMode::On {
            execution_route.max_attempts.min(3) as usize
        } else {
            1
        };
        let mut attempted = HashSet::new();
        let mut causes = Vec::new();
        let mut schema_failovers = 0_u8;
        let mut next_eligible_at: Option<String> = None;

        for (index, target) in attempt_targets.into_iter().enumerate() {
            if index >= max_attempts || Instant::now() >= route_deadline {
                break;
            }
            let model = model_override
                .as_deref()
                .unwrap_or(if target.target.model_id.trim().is_empty() {
                    target.connection.default_model.as_str()
                } else {
                    target.target.model_id.as_str()
                })
                .to_string();
            if !attempted.insert((target.connection.id.0.clone(), model.clone())) {
                continue;
            }
            if !self.repository.claim_breaker_probe(
                target.connection.provider_id.as_str(),
                &target.connection.endpoint_url,
                self.clock.now(),
            )? {
                causes.push(format!("{}: breaker indisponível", target.connection.id));
                continue;
            }

            let profile =
                profile_by_id(target.connection.provider_id.as_str()).ok_or_else(|| {
                    LlmError::new(
                        LlmErrorKind::InvalidRequest,
                        "Profile da conexão não existe.",
                    )
                })?;
            let credential = if matches!(profile.auth_style, super::profiles::AuthStyle::None) {
                None
            } else {
                self.credentials.get(&target.connection.credential_ref)?
            };
            if credential.is_none()
                && !matches!(profile.auth_style, super::profiles::AuthStyle::None)
            {
                let error = LlmError::new(LlmErrorKind::MissingApiKey, "Conexão sem API key.")
                    .with_target(&profile.id, target.connection.id.as_str(), &model);
                causes.push(error.safe_cause());
                continue;
            }

            let semaphore = self.connection_semaphore(
                target.connection.id.as_str(),
                target.connection.max_concurrency,
            );
            let permit = tokio::select! {
                _ = cancellation.cancelled() => return self.cancel_execution(request, &emitter),
                result = semaphore.acquire_owned() => result.map_err(|_| LlmError::new(LlmErrorKind::Persistence,"Semáforo da conexão foi fechado."))?,
            };
            let ordinal = index as u32 + 1;
            let attempt_id = self.repository.start_attempt(
                request.execution_id.as_str(),
                ordinal,
                &target,
                &model,
            )?;
            emitter.emit(LlmStreamEvent::AttemptStarted {
                ordinal,
                connection_id: target.connection.id.0.clone(),
                provider_id: target.connection.provider_id.0.clone(),
                model: model.clone(),
            });
            let delta_sink = BatchingDeltaSink::new(
                emitter.clone(),
                self.repository.clone(),
                request.execution_id.0.clone(),
            );
            let adapter = self.adapters.get(&profile.protocol).ok_or_else(|| {
                LlmError::new(
                    LlmErrorKind::InvalidRequest,
                    "Adapter de protocolo não registrado.",
                )
            })?;
            let mut effective_request = request.clone();
            if mode == RoutingMode::Off {
                effective_request.stream = false;
            }
            let started = Instant::now();
            let remaining = route_deadline.saturating_duration_since(Instant::now());
            let attempt_budget =
                Duration::from_millis(execution_route.attempt_timeout_ms).min(remaining);
            let outcome = timeout(
                attempt_budget,
                adapter.execute(
                    AdapterCall {
                        profile: &profile,
                        connection: &target.connection,
                        model: &model,
                        credential: credential.as_deref(),
                        request: &effective_request,
                    },
                    cancellation,
                    &delta_sink,
                ),
            )
            .await;
            drop(permit);
            delta_sink.flush();
            let visible_partial = delta_sink.visible_output();
            let latency_ms = started.elapsed().as_millis() as u64;

            let mut result = match outcome {
                Ok(result) => result,
                Err(_) => Err(LlmError::new(
                    LlmErrorKind::Timeout,
                    "Tentativa LLM excedeu o timeout.",
                )
                .with_target(&profile.id, target.connection.id.as_str(), &model)),
            };
            if let Err(error) = &mut result {
                if error.kind != LlmErrorKind::Cancelled && error.visible_partial.is_none() {
                    error.visible_partial = visible_partial.clone();
                }
            }
            match result {
                Ok(output) => {
                    if let Err(mut error) =
                        validate_output(&effective_request.response_format, &output.content)
                    {
                        error.visible_partial = visible_partial.clone();
                        self.repository.finish_attempt(
                            &attempt_id,
                            "failed",
                            Some(error.kind.code()),
                            None,
                            None,
                            latency_ms,
                            output.usage.input_tokens,
                            output.usage.output_tokens,
                            None,
                        )?;
                        emitter.emit(LlmStreamEvent::AttemptFailed {
                            ordinal,
                            error_code: error.kind.code().to_string(),
                        });
                        causes.push(error.safe_cause());
                        if error.visible_partial.is_some() {
                            self.repository.finish_execution(
                                request.execution_id.as_str(),
                                "stream_failed",
                                next_eligible_at.as_deref(),
                                error.visible_partial.as_deref(),
                                false,
                            )?;
                            emitter.emit(LlmStreamEvent::RouteFailed {
                                error_code: "stream_failed".to_string(),
                                next_eligible_at: next_eligible_at.clone(),
                            });
                            return Err(error);
                        }
                        if schema_failovers >= 1 || mode != RoutingMode::On {
                            break;
                        }
                        schema_failovers += 1;
                        continue;
                    }
                    let cost = estimate_cost(
                        &profile.id,
                        &model,
                        output.usage.input_tokens,
                        output.usage.output_tokens,
                    );
                    self.repository.finish_attempt(
                        &attempt_id,
                        "success",
                        None,
                        None,
                        None,
                        latency_ms,
                        output.usage.input_tokens,
                        output.usage.output_tokens,
                        cost,
                    )?;
                    let origin = endpoint_origin(&target.connection.endpoint_url);
                    let previous_breaker = self.repository.breaker_state(&profile.id, &origin)?;
                    self.repository.record_health_success(
                        &profile.id,
                        &origin,
                        self.clock.now(),
                    )?;
                    if previous_breaker
                        .as_deref()
                        .is_some_and(|state| state != "closed")
                    {
                        emitter.emit(LlmStreamEvent::ResilienceChanged {
                            scope: "breaker".to_string(),
                            scope_id: format!("{}@{}", profile.id, origin),
                            from_state: previous_breaker.unwrap_or_else(|| "unknown".to_string()),
                            to_state: "closed".to_string(),
                            retry_at: None,
                        });
                    }
                    if target.connection.cooldown_until.is_some() {
                        emitter.emit(LlmStreamEvent::ResilienceChanged {
                            scope: "connection".to_string(),
                            scope_id: target.connection.id.0.clone(),
                            from_state: "cooldown".to_string(),
                            to_state: "active".to_string(),
                            retry_at: None,
                        });
                    }
                    self.repository
                        .clear_connection_penalties(target.connection.id.as_str())?;
                    self.repository.set_affinity(
                        &affinity_hash,
                        &execution_route.id,
                        target.connection.id.as_str(),
                        &model,
                        self.clock.now(),
                    )?;
                    self.repository.finish_execution(
                        request.execution_id.as_str(),
                        "completed",
                        None,
                        Some(&output.content),
                        true,
                    )?;
                    emitter.emit(LlmStreamEvent::RouteCompleted {
                        provider_id: profile.id.clone(),
                        connection_id: target.connection.id.0.clone(),
                        model: model.clone(),
                    });
                    return Ok(LlmResponse {
                        execution_id: request.execution_id.clone(),
                        generation_id: request.generation_id.clone(),
                        route_id: execution_route.id.clone().into(),
                        connection_id: target.connection.id.clone(),
                        provider_id: target.connection.provider_id.clone(),
                        model,
                        request_payload: output.request_payload,
                        response_payload: output.response_payload,
                        content: output.content,
                        usage: output.usage,
                        estimated_cost_usd: cost,
                        finish_reason: output.finish_reason,
                    });
                }
                Err(error) => {
                    let visible = error.visible_partial.clone();
                    self.repository.finish_attempt(
                        &attempt_id,
                        "failed",
                        Some(error.kind.code()),
                        error.http_status,
                        error.provider_code.as_deref(),
                        latency_ms,
                        None,
                        None,
                        None,
                    )?;
                    emitter.emit(LlmStreamEvent::AttemptFailed {
                        ordinal,
                        error_code: error.kind.code().to_string(),
                    });
                    if error.kind == LlmErrorKind::Cancelled {
                        return self.cancel_execution(request, &emitter);
                    }
                    self.apply_resilience(
                        &target,
                        &model,
                        &error,
                        ordinal,
                        &mut next_eligible_at,
                        &emitter,
                    )?;
                    causes.push(error.safe_cause());
                    if visible.is_some() {
                        self.repository.finish_execution(
                            request.execution_id.as_str(),
                            "stream_failed",
                            next_eligible_at.as_deref(),
                            visible.as_deref(),
                            false,
                        )?;
                        emitter.emit(LlmStreamEvent::RouteFailed {
                            error_code: "stream_failed".to_string(),
                            next_eligible_at: next_eligible_at.clone(),
                        });
                        return Err(error);
                    }
                    if !error.recoverable() || mode != RoutingMode::On {
                        break;
                    }
                }
            }
        }

        if next_eligible_at.is_none() {
            next_eligible_at = self
                .repository
                .next_eligible_at(&eligibility_targets, self.clock.now())?;
        }
        let mut error = LlmError::new(
            LlmErrorKind::AllTargetsExhausted,
            "Todos os alvos elegíveis da rota foram esgotados.",
        );
        error.causes = causes;
        error.next_eligible_at = next_eligible_at.clone();
        self.repository.finish_execution(
            request.execution_id.as_str(),
            "failed",
            next_eligible_at.as_deref(),
            None,
            false,
        )?;
        emitter.emit(LlmStreamEvent::RouteFailed {
            error_code: error.kind.code().to_string(),
            next_eligible_at,
        });
        Err(error)
    }

    fn legacy_execution(
        &self,
        request: &LlmRequest,
    ) -> Result<(Vec<ResolvedRouteTarget>, String), LlmError> {
        let connection_id = if request.legacy_provider == "anthropic" {
            "legacy-anthropic"
        } else {
            "legacy-openai"
        };
        let targets = self.repository.resolved_targets("legacy-default")?;
        let target = targets
            .into_iter()
            .find(|target| target.connection.id.as_str() == connection_id)
            .ok_or_else(|| {
                LlmError::new(LlmErrorKind::Persistence, "Conexão legada não encontrada.")
            })?;
        Ok((vec![target], request.legacy_model.clone()))
    }

    fn connection_semaphore(&self, connection_id: &str, limit: u32) -> Arc<Semaphore> {
        let mut semaphores = self
            .semaphores
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        semaphores
            .entry(connection_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(limit.max(1) as usize)))
            .clone()
    }

    fn runtime_gate_reasons(
        &self,
        targets: &[ResolvedRouteTarget],
        request: &LlmRequest,
        enforce_capabilities: bool,
    ) -> Result<HashMap<String, String>, LlmError> {
        let mut gates = HashMap::new();
        for target in targets {
            let Some(profile) = profile_by_id(target.connection.provider_id.as_str()) else {
                gates.insert(
                    target.connection.id.0.clone(),
                    "provider_profile_missing".to_string(),
                );
                continue;
            };
            if !matches!(profile.auth_style, super::profiles::AuthStyle::None)
                && self
                    .credentials
                    .get(&target.connection.credential_ref)?
                    .is_none()
            {
                gates.insert(
                    target.connection.id.0.clone(),
                    "missing_api_key".to_string(),
                );
                continue;
            }
            if enforce_capabilities {
                let capability_gate = if request.stream && !profile.capabilities.streaming {
                    Some("capability_streaming")
                } else {
                    match &request.response_format {
                        ResponseFormat::Text => None,
                        ResponseFormat::JsonObject
                            if !profile.capabilities.json_object
                                && !profile.capabilities.json_schema =>
                        {
                            Some("capability_json_object")
                        }
                        ResponseFormat::JsonSchema { .. } if !profile.capabilities.json_schema => {
                            Some("capability_json_schema")
                        }
                        _ => None,
                    }
                };
                if let Some(reason) = capability_gate {
                    gates.insert(target.connection.id.0.clone(), reason.to_string());
                }
            }
        }
        Ok(gates)
    }

    fn apply_resilience(
        &self,
        target: &ResolvedRouteTarget,
        model: &str,
        error: &LlmError,
        ordinal: u32,
        next_eligible: &mut Option<String>,
        emitter: &ExecutionEmitter,
    ) -> Result<(), LlmError> {
        match error.kind {
            LlmErrorKind::Authentication | LlmErrorKind::Billing => {
                self.repository
                    .mark_connection_blocked(target.connection.id.as_str(), error.kind.code())?;
                emitter.emit(LlmStreamEvent::ResilienceChanged {
                    scope: "connection".to_string(),
                    scope_id: target.connection.id.0.clone(),
                    from_state: "active".to_string(),
                    to_state: "blocked".to_string(),
                    retry_at: None,
                });
            }
            LlmErrorKind::RateLimited => {
                let delay = error.retry_after_ms.unwrap_or_else(|| {
                    cooldown_ms(
                        target.connection.backoff_level,
                        self.jitter.factor(target.connection.id.as_str(), ordinal),
                    )
                });
                let until = eligible_at(self.clock.now(), delay).to_rfc3339();
                self.repository
                    .apply_cooldown(target.connection.id.as_str(), &until)?;
                if next_eligible
                    .as_ref()
                    .is_none_or(|current| current > &until)
                {
                    *next_eligible = Some(until.clone());
                }
                emitter.emit(LlmStreamEvent::ResilienceChanged {
                    scope: "connection".to_string(),
                    scope_id: target.connection.id.0.clone(),
                    from_state: "active".to_string(),
                    to_state: "cooldown".to_string(),
                    retry_at: Some(until),
                });
            }
            LlmErrorKind::ModelNotFound => {
                self.repository.lockout_model(
                    target.connection.id.as_str(),
                    model,
                    error.kind.code(),
                )?;
                emitter.emit(LlmStreamEvent::ResilienceChanged {
                    scope: "model".to_string(),
                    scope_id: format!("{}@{}", target.connection.id, model),
                    from_state: "active".to_string(),
                    to_state: "locked".to_string(),
                    retry_at: None,
                });
            }
            kind if kind.affects_health() => {
                let provider_id = target.connection.provider_id.as_str();
                let origin = endpoint_origin(&target.connection.endpoint_url);
                let from_state = self
                    .repository
                    .breaker_state(provider_id, &origin)?
                    .unwrap_or_else(|| "closed".to_string());
                self.repository.record_health_failure(
                    provider_id,
                    &origin,
                    target.provider_local,
                    self.clock.now(),
                )?;
                let to_state = self
                    .repository
                    .breaker_state(provider_id, &origin)?
                    .unwrap_or_else(|| "closed".to_string());
                if from_state != to_state {
                    emitter.emit(LlmStreamEvent::ResilienceChanged {
                        scope: "breaker".to_string(),
                        scope_id: format!("{}@{}", provider_id, origin),
                        from_state,
                        to_state,
                        retry_at: None,
                    });
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn cancel_execution<T>(
        &self,
        request: &LlmRequest,
        emitter: &ExecutionEmitter,
    ) -> Result<T, LlmError> {
        let error = LlmError::new(LlmErrorKind::Cancelled, "Execução LLM cancelada.");
        self.repository.finish_execution(
            request.execution_id.as_str(),
            "cancelled",
            None,
            None,
            false,
        )?;
        emitter.emit(LlmStreamEvent::RouteFailed {
            error_code: error.kind.code().to_string(),
            next_eligible_at: None,
        });
        Err(error)
    }
}

struct NoopDeltaSink;

impl TokenDeltaSink for NoopDeltaSink {
    fn push(&self, _delta: &str) {}
}

impl LlmGateway for ProviderRouter {
    fn execute<'a>(
        &'a self,
        request: &'a LlmRequest,
        cancellation: &'a CancellationToken,
    ) -> GatewayFuture<'a> {
        Box::pin(async move { self.execute_inner(request, cancellation).await })
    }
}

fn ordered_targets(
    targets: &[ResolvedRouteTarget],
    route: &ProviderRoute,
    repository: &SqliteLlmRepository,
    affinity_hash: &str,
    now: chrono::DateTime<Utc>,
) -> Result<Vec<ResolvedRouteTarget>, LlmError> {
    let decision = repository.decide(route, targets, affinity_hash, now)?;
    let mut ordered = Vec::new();
    for target in decision.ordered_targets {
        if let Some(resolved) = targets.iter().find(|item| {
            item.target.connection_id == target.connection_id
                && item.target.model_id == target.model_id
        }) {
            ordered.push(resolved.clone());
        }
    }
    Ok(ordered)
}

fn apply_runtime_gates_to_decision(
    decision: &mut super::domain::RouteDecision,
    gates: &HashMap<String, String>,
) {
    for candidate in &mut decision.candidates {
        if candidate.eligible {
            if let Some(reason) = gates.get(&candidate.connection_id) {
                candidate.eligible = false;
                candidate.gate_reason = Some(reason.clone());
            }
        }
    }
    decision
        .ordered_targets
        .retain(|target| !gates.contains_key(&target.connection_id));
    decision.selected_connection_id = decision
        .ordered_targets
        .first()
        .map(|target| target.connection_id.clone());
    decision.selected_model_id = decision
        .ordered_targets
        .first()
        .map(|target| target.model_id.clone());
}

fn affinity_hash(request: &LlmRequest) -> String {
    let mut hasher = Sha256::new();
    hasher.update(request.project_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(request.pulsation_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"\0");
    hasher.update(request.agent_role.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(request.job_chain.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn validate_output(format: &ResponseFormat, content: &str) -> Result<(), LlmError> {
    match format {
        ResponseFormat::Text => Ok(()),
        ResponseFormat::JsonObject => {
            let parsed: Value = serde_json::from_str(content).map_err(|_| {
                LlmError::new(
                    LlmErrorKind::InvalidResponse,
                    "Provider não retornou JSON válido.",
                )
            })?;
            if parsed.is_object() {
                Ok(())
            } else {
                Err(LlmError::new(
                    LlmErrorKind::InvalidResponse,
                    "Output JSON não é um objeto.",
                ))
            }
        }
        ResponseFormat::JsonSchema { schema, .. } => {
            let parsed: Value = serde_json::from_str(content).map_err(|_| {
                LlmError::new(
                    LlmErrorKind::InvalidResponse,
                    "Provider não retornou JSON válido.",
                )
            })?;
            let validator = validator_for(schema).map_err(|_| {
                LlmError::new(LlmErrorKind::OutputSchema, "Schema de output é inválido.")
            })?;
            validator.validate(&parsed).map_err(|error| {
                LlmError::new(
                    LlmErrorKind::OutputSchema,
                    format!("Output viola o schema: {error}"),
                )
            })
        }
    }
}

fn estimate_cost(
    provider: &str,
    model: &str,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
) -> Option<f64> {
    estimate_cost_usd(provider, model, input_tokens, output_tokens)
}

#[derive(Clone)]
struct ExecutionEmitter {
    sink: Arc<dyn LlmEventSink>,
    execution_id: String,
    generation_id: String,
    sequence: Arc<AtomicU64>,
}

impl ExecutionEmitter {
    fn new(sink: Arc<dyn LlmEventSink>, execution_id: String, generation_id: String) -> Self {
        Self {
            sink,
            execution_id,
            generation_id,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    fn emit(&self, event: LlmStreamEvent) {
        self.sink.emit(&LlmEventEnvelope {
            protocol_version: 1,
            execution_id: self.execution_id.clone(),
            generation_id: self.generation_id.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
            event,
        });
    }
}

struct DeltaState {
    pending: String,
    visible: String,
    timer_active: bool,
}

struct BatchingDeltaSink {
    state: Arc<Mutex<DeltaState>>,
    emitter: ExecutionEmitter,
    repository: SqliteLlmRepository,
    execution_id: String,
}

impl BatchingDeltaSink {
    fn new(
        emitter: ExecutionEmitter,
        repository: SqliteLlmRepository,
        execution_id: String,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(DeltaState {
                pending: String::new(),
                visible: String::new(),
                timer_active: false,
            })),
            emitter,
            repository,
            execution_id,
        }
    }

    fn flush(&self) {
        flush_delta_state(
            &self.state,
            &self.emitter,
            &self.repository,
            &self.execution_id,
        );
    }

    fn visible_output(&self) -> Option<String> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (!state.visible.is_empty()).then(|| state.visible.clone())
    }
}

impl TokenDeltaSink for BatchingDeltaSink {
    fn push(&self, delta: &str) {
        let mut schedule = false;
        let mut flush_now = false;
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.pending.push_str(delta);
            state.visible.push_str(delta);
            if state.pending.len() >= 1024 {
                flush_now = true;
            } else if !state.timer_active {
                state.timer_active = true;
                schedule = true;
            }
        }
        if flush_now {
            self.flush();
        } else if schedule {
            let state = self.state.clone();
            let emitter = self.emitter.clone();
            let repository = self.repository.clone();
            let execution_id = self.execution_id.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(25)).await;
                flush_delta_state(&state, &emitter, &repository, &execution_id);
            });
        }
    }
}

fn flush_delta_state(
    state: &Arc<Mutex<DeltaState>>,
    emitter: &ExecutionEmitter,
    repository: &SqliteLlmRepository,
    execution_id: &str,
) {
    let (delta, visible) = {
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.timer_active = false;
        (std::mem::take(&mut state.pending), state.visible.clone())
    };
    if !delta.is_empty() {
        let _ = repository.update_visible_output(execution_id, &visible);
        emitter.emit(LlmStreamEvent::TokenDelta { text: delta });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::domain::{AgentRole, ExecutionId, GenerationId};

    #[test]
    fn affinity_hash_is_stable_and_scoped() {
        let request = LlmRequest {
            execution_id: ExecutionId::new("e"),
            generation_id: GenerationId::new("g"),
            project_id: "p".into(),
            pulsation_id: Some("pulse".into()),
            job_id: Some("j".into()),
            job_chain: "chain".into(),
            agent_role: AgentRole::Integrator,
            stage: "Integrator".into(),
            system_prompt: String::new(),
            user_prompt: String::new(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 10,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(1),
            stream: false,
            legacy_provider: "openai".into(),
            legacy_model: "m".into(),
        };
        assert_eq!(affinity_hash(&request), affinity_hash(&request));
        let mut changed = request.clone();
        changed.project_id = "other".into();
        assert_ne!(affinity_hash(&request), affinity_hash(&changed));
    }
}
