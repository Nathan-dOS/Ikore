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

use super::adapter::AccountingSink;
use super::adapter::{AdapterCall, LlmAdapter, ProtocolAdapter, TokenDeltaSink};
use super::catalog::{estimate_cost_usd, model_entry};
use super::compression::redact;
use super::credentials::CredentialStore;
use super::domain::{
    AgentExecutionTarget, Clock, DispatchPolicy, GatewayFuture, JitterSource, LlmEventEnvelope,
    LlmEventSink, LlmGateway, LlmRequest, LlmResponse, LlmStreamEvent, ProviderProtocol,
    ProviderRoute, ResponseFormat, RoutingMode, effective_response_format,
};
use super::error::{LlmError, LlmErrorKind};
use super::profiles::profile_by_id;
use super::repository::TestProviderConnectionResponse;
use super::repository::{LlmRepository, ResolvedRouteTarget, SqliteLlmRepository, endpoint_origin};
use super::resilience::{cooldown_ms, eligible_at};
use crate::usage::{UsageContext, UsageTransport, api::UsageRecorder};

const CONNECTION_TEST_MAX_OUTPUT_TOKENS: u32 = 16;

fn verify_api_models(
    provider: &str,
    requested: &str,
    observed: &[String],
) -> Result<(), LlmError> {
    if observed.is_empty() {
        return Err(
            LlmError::new(
                LlmErrorKind::ModelUnverified,
                format!("Provider `{provider}` did not report the generated model identity."),
            )
            .with_target(provider, "", requested),
        );
    }
    for model in observed {
        match crate::terminals::model_identity::verify_model(
            Some(provider),
            Some(requested),
            Some(model),
        ) {
            crate::terminals::model_identity::ModelVerification::Verified => {}
            crate::terminals::model_identity::ModelVerification::Mismatch => {
                return Err(
                    LlmError::new(
                        LlmErrorKind::ModelMismatch,
                        format!("Provider `{provider}` reported `{model}`; expected `{requested}`."),
                    )
                    .with_target(provider, "", requested),
                );
            }
            crate::terminals::model_identity::ModelVerification::Unverified => {
                return Err(
                    LlmError::new(
                        LlmErrorKind::ModelUnverified,
                        format!("Provider `{provider}` cannot verify requested model `{requested}`."),
                    )
                    .with_target(provider, "", requested),
                );
            }
        }
    }
    Ok(())
}

fn persist_api_model_observations(
    repository: &SqliteLlmRepository,
    attempt_id: &str,
    request: &LlmRequest,
    provider: &str,
    requested: &str,
    observed: &[String],
) -> Result<(), LlmError> {
    let models = if observed.is_empty() {
        vec![None]
    } else {
        observed.iter().map(|model| Some(model.as_str())).collect()
    };
    for (index, model) in models.into_iter().enumerate() {
        repository.record_model_observation(
            attempt_id,
            index as u32 + 1,
            request.generation_id.as_str(),
            request.target.agent_role(),
            &request.target.stable_key(),
            request
                .dispatch
                .unit
                .as_ref()
                .map(|unit| unit.attempt_id.as_str()),
            provider,
            requested,
            model,
        )?;
    }
    Ok(())
}

fn connection_test_request(provider_id: &str, model: &str) -> LlmRequest {
    LlmRequest {
        execution_id: uuid::Uuid::new_v4().to_string().into(),
        generation_id: uuid::Uuid::new_v4().to_string().into(),
        project_id: "connection-test".to_string(),
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        job_chain: "connection-test".to_string(),
        target: AgentExecutionTarget::canonical(super::domain::AgentRole::IntakeRouter),
        stage: "connection_test".to_string(),
        system_prompt: "Responda de forma concisa.".to_string(),
        user_prompt: "Responda apenas OK.".to_string(),
        response_format: ResponseFormat::Text,
        max_output_tokens: CONNECTION_TEST_MAX_OUTPUT_TOKENS,
        temperature: 0.0,
        deadline: Instant::now() + Duration::from_secs(30),
        stream: false,
        legacy_provider: provider_id.to_string(),
        legacy_model: model.to_string(),
        dispatch: DispatchPolicy::default(),
    }
}

pub struct ProviderRouter {
    repository: SqliteLlmRepository,
    credentials: Arc<dyn CredentialStore>,
    adapters: HashMap<ProviderProtocol, Arc<dyn LlmAdapter>>,
    event_sink: Arc<dyn LlmEventSink>,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn JitterSource>,
    semaphores: Mutex<HashMap<String, Arc<Semaphore>>>,
    priority: Arc<super::priority::ConnectionPriority>,
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
            priority: super::priority::ConnectionPriority::new(clock.clone()),
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
                "The connection profile does not exist.",
            )
        })?;
        let credential = if matches!(profile.auth_style, super::profiles::AuthStyle::None) {
            None
        } else {
            self.credentials.get(&connection.credential_ref)?
        };
        if credential.is_none() && !matches!(profile.auth_style, super::profiles::AuthStyle::None) {
            return Err(LlmError::new(
                LlmErrorKind::MissingApiKey,
                "The connection has no API key.",
            )
            .with_target(
                &profile.id,
                connection.id.as_str(),
                &connection.default_model,
            ));
        }
        let request = connection_test_request(&profile.id, &connection.default_model);
        let adapter = self.adapters.get(&profile.protocol).ok_or_else(|| {
            LlmError::new(
                LlmErrorKind::InvalidRequest,
                "No protocol adapter is registered.",
            )
        })?;
        let started = Instant::now();
        let recorder = UsageRecorder::new(
            self.repository.database_path(),
            UsageContext {
                source_kind: "connection_test".into(),
                source_id: request.execution_id.0.clone(),
                pulsation_id: None,
                project_id: None,
                transport: UsageTransport::Api,
                provider_id: profile.id.clone(),
                requested_model: Some(connection.default_model.clone()),
                started_at: Utc::now().to_rfc3339(),
            },
        );
        let outcome = timeout(
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
                &AccountingSink {
                    inner: &NoopDeltaSink,
                    recorder: &recorder,
                },
            ),
        )
        .await;
        recorder
            .finish(matches!(&outcome, Ok(Ok(_))))
            .map_err(|e| {
                LlmError::new(
                    LlmErrorKind::Persistence,
                    format!("Token usage persistence failed: {e}"),
                )
            })?;
        let output = outcome.map_err(|_| {
            LlmError::new(
                LlmErrorKind::Timeout,
                "The connection test took longer than 30 seconds.",
            )
        })??;
        Ok(TestProviderConnectionResponse {
            connection_id: connection.id.0,
            reachable: !output.content.trim().is_empty(),
            provider_id: profile.id,
            model: connection.default_model,
            latency_ms: started.elapsed().as_millis() as u64,
            message: "The connection answered successfully.".to_string(),
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
                "LLM execution cancelled.",
            ));
        }
        let mode = if matches!(&request.target, AgentExecutionTarget::FreeForm { .. }) {
            // FreeForm API execution is explicitly bound per slot. It must never
            // fall through to the global legacy provider/model configuration.
            RoutingMode::On
        } else {
            RoutingMode::parse(&self.repository.routing_mode()?)
        };
        let bound_route = self.repository.resolve_route_for_target(
            &request.project_id,
            &request.target,
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
                        LlmError::new(
                            LlmErrorKind::Persistence,
                            "The legacy-default route is missing.",
                        )
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
                .get(&runtime_target_key(target))
                .is_some_and(|reason| reason == "missing_api_key"))
            .then(|| {
                LlmError::new(
                    LlmErrorKind::MissingApiKey,
                    "The connection has no API key.",
                )
                .with_target(
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
        attempt_targets.retain(|target| !execution_gates.contains_key(&runtime_target_key(target)));
        apply_runtime_gates_to_decision(&mut decision, &decision_gates);

        self.repository.create_execution(
            request.execution_id.as_str(),
            request.generation_id.as_str(),
            &request.project_id,
            request.pulsation_id.as_deref(),
            request.job_id.as_deref(),
            &execution_route,
            &request.target,
            &request.stage,
            mode.as_str(),
            &decision,
        )?;
        let emitter = ExecutionEmitter::new(
            self.event_sink.clone(),
            request.execution_id.0.clone(),
            request.generation_id.0.clone(),
            crate::llm::AgentExecutionContext {
                project_id: request.project_id.clone(),
                pulsation_id: request.pulsation_id.clone(),
                job_id: request.job_id.clone(),
                target: request.target.clone(),
                stage: request.stage.clone(),
            },
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
                        "No LLM target on this route is eligible.",
                    )
                })
            } else {
                let mut error = LlmError::new(
                    LlmErrorKind::AllTargetsExhausted,
                    "No LLM target on this route is eligible.",
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
        let is_learning = request.target.agent_role() == Some(super::AgentRole::LearningAgent);
        let mut max_attempts = if mode == RoutingMode::On && !is_learning {
            execution_route.max_attempts.min(3) as usize
        } else {
            1
        };
        // Composition holds one durable permit per real invocation: the
        // router may not multiply attempts behind that boundary.
        if let Some(limit) = request.dispatch.attempt_limit {
            max_attempts = max_attempts.min(usize::from(limit)).max(1);
        }
        let mut attempted = HashSet::new();
        let mut causes = Vec::new();
        let mut schema_failovers = 0_u8;
        let mut next_eligible_at: Option<String> = None;
        let mut frozen_api_selection: Option<(String, String)> = None;

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
            let selected_provider = target.connection.provider_id.0.clone();
            if let Some((frozen_provider, frozen_model)) = frozen_api_selection.as_ref() {
                let same_provider = selected_provider == *frozen_provider;
                let same_model = same_provider
                    && matches!(
                        crate::terminals::model_identity::verify_model(
                            Some(frozen_provider),
                            Some(frozen_model),
                            Some(&model),
                        ),
                        crate::terminals::model_identity::ModelVerification::Verified
                    );
                if !same_model {
                    let error = LlmError::new(
                        LlmErrorKind::ModelMismatch,
                        format!(
                            "A fallback attempted to change the frozen selection from `{frozen_provider}/{frozen_model}` to `{selected_provider}/{model}`."
                        ),
                    )
                    .with_target(&selected_provider, target.connection.id.as_str(), &model);
                    self.repository.finish_execution(
                        request.execution_id.as_str(),
                        "failed",
                        None,
                        None,
                        false,
                    )?;
                    emitter.emit(LlmStreamEvent::RouteFailed {
                        error_code: error.kind.code().into(),
                        next_eligible_at: None,
                    });
                    return Err(error);
                }
            } else {
                frozen_api_selection = Some((selected_provider.clone(), model.clone()));
            }
            if !attempted.insert((target.connection.id.0.clone(), model.clone())) {
                continue;
            }

            let profile =
                profile_by_id(target.connection.provider_id.as_str()).ok_or_else(|| {
                    LlmError::new(
                        LlmErrorKind::InvalidRequest,
                        "The connection profile does not exist.",
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
                let error = LlmError::new(
                    LlmErrorKind::MissingApiKey,
                    "The connection has no API key.",
                )
                .with_target(&profile.id, target.connection.id.as_str(), &model);
                causes.push(error.safe_cause());
                continue;
            }

            let semaphore = self.connection_semaphore(
                target.connection.id.as_str(),
                target.connection.max_concurrency,
            );
            let call_cancellation = cancellation.child_token();
            let _priority = if is_learning {
                let Some(guard) = self.priority.learning(
                    target.connection.id.as_str(),
                    request.execution_id.as_str(),
                    call_cancellation.clone(),
                ) else {
                    self.repository.finish_execution(
                        request.execution_id.as_str(),
                        "cancelled",
                        None,
                        None,
                        false,
                    )?;
                    emitter.emit(LlmStreamEvent::RouteFailed {
                        error_code: "background_deferred".into(),
                        next_eligible_at: None,
                    });
                    return Err(LlmError::new(
                        LlmErrorKind::BackgroundDeferred,
                        "Learning waits 5 s with no main demand on the connection.",
                    ));
                };
                guard
            } else {
                self.priority.foreground(target.connection.id.as_str())
            };
            if !self.repository.claim_breaker_probe(
                target.connection.provider_id.as_str(),
                &target.connection.endpoint_url,
                self.clock.now(),
            )? {
                causes.push(format!("{}: breaker unavailable", target.connection.id));
                continue;
            }

            let permit = tokio::select! {
                _ = call_cancellation.cancelled() => return self.cancel_execution(request, &emitter),
                result = semaphore.acquire_owned() => result.map_err(|_| LlmError::new(LlmErrorKind::Persistence,"The connection semaphore was closed."))?,
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
                    "No protocol adapter is registered.",
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
            let recorder = UsageRecorder::new(
                self.repository.database_path(),
                UsageContext {
                    source_kind: "llm_attempt".into(),
                    source_id: attempt_id.clone(),
                    pulsation_id: request.pulsation_id.clone(),
                    project_id: Some(request.project_id.clone()),
                    transport: UsageTransport::Api,
                    provider_id: profile.id.clone(),
                    requested_model: Some(model.clone()),
                    started_at: Utc::now().to_rfc3339(),
                },
            );
            let accounting_sink = AccountingSink {
                inner: &delta_sink,
                recorder: &recorder,
            };
            let outcome = tokio::select! {
                _=call_cancellation.cancelled()=>Ok(Err(LlmError::new(LlmErrorKind::Cancelled,"Chamada cancelada por prioridade ou encerramento."))),
                result=timeout(
                attempt_budget,
                adapter.execute(
                    AdapterCall {
                        profile: &profile,
                        connection: &target.connection,
                        model: &model,
                        credential: credential.as_deref(),
                        request: &effective_request,
                    },
                    &call_cancellation,
                    &accounting_sink,
                ),
            )=>result
            };
            recorder
                .finish(matches!(&outcome, Ok(Ok(_))))
                .map_err(|e| {
                    LlmError::new(
                        LlmErrorKind::Persistence,
                        format!("Token usage persistence failed: {e}"),
                    )
                })?;
            drop(permit);
            delta_sink.flush();
            let visible_partial = delta_sink.visible_output();
            let latency_ms = started.elapsed().as_millis() as u64;

            let mut result = match outcome {
                Ok(result) => result,
                Err(_) => {
                    Err(
                        LlmError::new(LlmErrorKind::Timeout, "The LLM attempt timed out.")
                            .with_target(&profile.id, target.connection.id.as_str(), &model),
                    )
                }
            };
            if call_cancellation.is_cancelled() {
                result = Err(LlmError::new(
                    LlmErrorKind::Cancelled,
                    "Chamada cancelada por prioridade ou encerramento.",
                ));
            }
            if let Err(error) = &mut result
                && error.kind != LlmErrorKind::Cancelled
                && error.visible_partial.is_none()
            {
                error.visible_partial = visible_partial.clone();
            }
            match result {
                Ok(output) => {
                    persist_api_model_observations(
                        &self.repository,
                        &attempt_id,
                        request,
                        &profile.id,
                        &model,
                        &output.observed_models,
                    )?;
                    let identity = verify_api_models(&profile.id, &model, &output.observed_models);
                    if let Err(mut error) = identity {
                        error.observed_models = output.observed_models.clone();
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
                        self.repository.finish_execution(
                            request.execution_id.as_str(),
                            "failed",
                            None,
                            None,
                            false,
                        )?;
                        emitter.emit(LlmStreamEvent::RouteFailed {
                            error_code: error.kind.code().to_string(),
                            next_eligible_at: None,
                        });
                        error.connection_id = Some(target.connection.id.0.clone());
                        return Err(error);
                    }
                    let gate = if request.dispatch.host_validated {
                        Ok(())
                    } else {
                        validate_output(
                            &effective_response_format(&effective_request, &profile),
                            &output.content,
                        )
                    };
                    if let Err(mut error) = gate {
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
                    let reported_model = output
                        .observed_models
                        .first()
                        .cloned()
                        .unwrap_or_else(|| model.clone());
                    return Ok(LlmResponse {
                        execution_id: request.execution_id.clone(),
                        generation_id: request.generation_id.clone(),
                        route_id: execution_route.id.clone().into(),
                        connection_id: target.connection.id.clone(),
                        provider_id: target.connection.provider_id.clone(),
                        model: reported_model,
                        request_payload: output.request_payload,
                        response_payload: output.response_payload,
                        content: output.content,
                        usage: output.usage,
                        estimated_cost_usd: cost,
                        finish_reason: output.finish_reason,
                    });
                }
                Err(mut error) => {
                    if matches!(
                        error.kind,
                        LlmErrorKind::ModelMismatch | LlmErrorKind::ModelUnverified
                    ) {
                        error.provider_id = Some(profile.id.clone());
                        error.connection_id = Some(target.connection.id.0.clone());
                        error.model_id = Some(model.clone());
                    }
                    persist_api_model_observations(
                        &self.repository,
                        &attempt_id,
                        request,
                        &profile.id,
                        &model,
                        &error.observed_models,
                    )?;
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
                    if matches!(
                        error.kind,
                        LlmErrorKind::ModelMismatch | LlmErrorKind::ModelUnverified
                    ) {
                        self.repository.finish_execution(
                            request.execution_id.as_str(),
                            "failed",
                            None,
                            None,
                            false,
                        )?;
                        emitter.emit(LlmStreamEvent::RouteFailed {
                            error_code: error.kind.code().into(),
                            next_eligible_at: None,
                        });
                        return Err(error);
                    }
                    self.apply_resilience(
                        &target,
                        &model,
                        &error,
                        ordinal,
                        &mut next_eligible_at,
                        &emitter,
                    )?;
                    // A single-attempt dispatch (Learning, or a composed unit
                    // holding one permit) gets the attempt's own error back:
                    // the caller classifies it, and "all targets exhausted"
                    // would only hide the cause.
                    if is_learning || request.dispatch.attempt_limit == Some(1) {
                        self.repository.finish_execution(
                            request.execution_id.as_str(),
                            "failed",
                            next_eligible_at.as_deref(),
                            None,
                            false,
                        )?;
                        emitter.emit(LlmStreamEvent::RouteFailed {
                            error_code: error.kind.code().into(),
                            next_eligible_at,
                        });
                        return Err(error);
                    }
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
            "Every eligible target on the route was exhausted.",
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

    fn priority_connections(
        &self,
        project: &str,
        role: super::AgentRole,
        stage: &str,
        provider: &str,
    ) -> Result<Vec<String>, LlmError> {
        if RoutingMode::parse(&self.repository.routing_mode()?) != RoutingMode::On {
            return Ok(vec![
                if provider == "anthropic" {
                    "legacy-anthropic"
                } else {
                    "legacy-openai"
                }
                .into(),
            ]);
        }
        let route = self.repository.resolve_route(project, role, stage)?;
        Ok(self
            .repository
            .resolved_targets(&route.id)?
            .into_iter()
            .map(|t| t.connection.id.0)
            .collect())
    }

    pub(crate) fn foreground_demand(
        &self,
        project: &str,
        role: super::AgentRole,
        stage: &str,
        provider: &str,
    ) -> Result<Vec<super::priority::DemandGuard>, LlmError> {
        Ok(self
            .priority_connections(project, role, stage, provider)?
            .iter()
            .map(|id| self.priority.foreground(id))
            .collect())
    }

    pub(crate) fn learning_ready(&self, project: &str, provider: &str) -> Result<bool, LlmError> {
        Ok(self
            .priority_connections(
                project,
                super::AgentRole::LearningAgent,
                "Learning",
                provider,
            )?
            .iter()
            .all(|id| self.priority.ready(id)))
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
                LlmError::new(LlmErrorKind::Persistence, "Legacy connection not found.")
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
    ) -> Result<RuntimeGates, LlmError> {
        let mut gates = HashMap::new();
        for target in targets {
            let Some(profile) = profile_by_id(target.connection.provider_id.as_str()) else {
                gates.insert(
                    runtime_target_key(target),
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
                gates.insert(runtime_target_key(target), "missing_api_key".to_string());
                continue;
            }
            if enforce_capabilities {
                let model = if target.target.model_id.trim().is_empty() {
                    target.connection.default_model.as_str()
                } else {
                    target.target.model_id.as_str()
                };
                let capability_gate = capability_gate_reason(
                    &profile,
                    target.connection.provider_id.as_str(),
                    model,
                    request,
                );
                if let Some(reason) = capability_gate {
                    gates.insert(runtime_target_key(target), reason.to_string());
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
                        scope_id: format!("{provider_id}@{origin}"),
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
        let error = LlmError::new(LlmErrorKind::Cancelled, "LLM execution cancelled.");
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

fn capability_gate_reason(
    profile: &super::profiles::ProviderProfile,
    provider_id: &str,
    model_id: &str,
    request: &LlmRequest,
) -> Option<&'static str> {
    if request.stream && !profile.capabilities.streaming {
        return Some("capability_streaming");
    }

    let response_format = effective_response_format(request, profile);
    let profile_gate = match &response_format {
        ResponseFormat::Text => None,
        ResponseFormat::JsonObject
            if !profile.capabilities.json_object && !profile.capabilities.json_schema =>
        {
            Some("capability_json_object")
        }
        ResponseFormat::JsonSchema { .. } if !profile.capabilities.json_schema => {
            Some("capability_json_schema")
        }
        _ => None,
    };
    if profile_gate.is_some() {
        return profile_gate;
    }

    // OpenRouter capabilities vary by model. Unknown models cannot opt into
    // structured routes without a reviewed catalog entry. Other providers keep
    // their existing profile-level JSON-object behavior.
    if provider_id == "openrouter" && response_format.requires_structured_output() {
        match model_entry(provider_id, model_id) {
            None => return Some("model_not_in_catalog"),
            Some(model) if !model.structured_output => return Some("capability_structured_output"),
            _ => {}
        }
    }
    None
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

type RuntimeGates = HashMap<(String, String), String>;

fn runtime_target_key(target: &ResolvedRouteTarget) -> (String, String) {
    (
        target.connection.id.0.clone(),
        if target.target.model_id.trim().is_empty() {
            target.connection.default_model.clone()
        } else {
            target.target.model_id.clone()
        },
    )
}

fn apply_runtime_gates_to_decision(
    decision: &mut super::domain::RouteDecision,
    gates: &RuntimeGates,
) {
    for candidate in &mut decision.candidates {
        if candidate.eligible
            && let Some(reason) =
                gates.get(&(candidate.connection_id.clone(), candidate.model_id.clone()))
        {
            candidate.eligible = false;
            candidate.gate_reason = Some(reason.clone());
        }
    }
    decision.ordered_targets.retain(|target| {
        // Candidate models have already resolved any default-model fallback.
        !decision.candidates.iter().any(|candidate| {
            candidate.position == target.position
                && candidate.connection_id == target.connection_id
                && gates
                    .contains_key(&(candidate.connection_id.clone(), candidate.model_id.clone()))
        })
    });
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
    hasher.update(request.target.stable_key().as_bytes());
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
                    "The provider did not return valid JSON.",
                )
            })?;
            if parsed.is_object() {
                Ok(())
            } else {
                Err(LlmError::new(
                    LlmErrorKind::InvalidResponse,
                    "The JSON output is not an object.",
                ))
            }
        }
        ResponseFormat::JsonSchema { schema, .. } => {
            let parsed: Value = serde_json::from_str(content).map_err(|_| {
                LlmError::new(
                    LlmErrorKind::InvalidResponse,
                    "The provider did not return valid JSON.",
                )
            })?;
            let validator = validator_for(schema).map_err(|_| {
                LlmError::new(LlmErrorKind::OutputSchema, "The output schema is invalid.")
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
    context: crate::llm::AgentExecutionContext,
}

impl ExecutionEmitter {
    fn new(
        sink: Arc<dyn LlmEventSink>,
        execution_id: String,
        generation_id: String,
        context: crate::llm::AgentExecutionContext,
    ) -> Self {
        Self {
            sink,
            execution_id,
            generation_id,
            sequence: Arc::new(AtomicU64::new(0)),
            context,
        }
    }

    fn emit(&self, event: LlmStreamEvent) {
        self.sink.emit(&LlmEventEnvelope {
            protocol_version: 2,
            execution_id: self.execution_id.clone(),
            generation_id: self.generation_id.clone(),
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
            context: Some(self.context.clone()),
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
        (!state.visible.is_empty()).then(|| redact(&state.visible))
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
            tokio::spawn(async move {
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
        let _ = repository.update_visible_output(execution_id, &redact(&visible));
        emitter.emit(LlmStreamEvent::TokenDelta {
            text: redact(&delta),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::domain::{AgentRole, ExecutionId, GenerationId};

    #[test]
    fn api_model_identity_accepts_only_the_selected_haiku_family() {
        assert!(verify_api_models(
            "anthropic",
            "haiku",
            &["claude-haiku-4-5".to_string()]
        )
        .is_ok());

        let mismatch = verify_api_models(
            "anthropic",
            "haiku",
            &["claude-sonnet-5".to_string()],
        )
        .expect_err("Sonnet must not satisfy a Haiku selection");
        assert_eq!(mismatch.kind.code(), "model_mismatch");
    }

    #[test]
    fn api_model_identity_fails_closed_when_generation_is_unreported() {
        let error = verify_api_models("anthropic", "haiku", &[])
            .expect_err("a response without model identity cannot be accepted");
        assert_eq!(error.kind.code(), "model_unverified");
    }

    #[test]
    fn connection_test_request_preserves_the_openai_minimum_contract() {
        let request = connection_test_request("openai", "gpt-4.1-mini");
        assert_eq!(request.max_output_tokens, 16);
        assert!(!request.stream);
        assert_eq!(request.legacy_provider, "openai");
        assert_eq!(request.legacy_model, "gpt-4.1-mini");
        assert_eq!(request.user_prompt, "Responda apenas OK.");
    }

    #[test]
    fn affinity_hash_is_stable_and_scoped() {
        let request = LlmRequest {
            execution_id: ExecutionId::new("e"),
            generation_id: GenerationId::new("g"),
            project_id: "p".into(),
            pulsation_id: Some("pulse".into()),
            job_id: Some("j".into()),
            agent_run_id: None,
            job_chain: "chain".into(),
            target: AgentExecutionTarget::canonical(AgentRole::Integrator),
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
            dispatch: DispatchPolicy::default(),
        };
        assert_eq!(affinity_hash(&request), affinity_hash(&request));
        let mut changed = request.clone();
        changed.project_id = "other".into();
        assert_ne!(affinity_hash(&request), affinity_hash(&changed));
    }

    #[test]
    fn model_capability_gate_rejects_structured_output_for_incompatible_openrouter_models() {
        let profile = profile_by_id("openrouter").unwrap();
        let mut request = LlmRequest {
            execution_id: ExecutionId::new("e"),
            generation_id: GenerationId::new("g"),
            project_id: "p".into(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "chain".into(),
            target: AgentExecutionTarget::canonical(AgentRole::BackendAgent),
            stage: "Code".into(),
            system_prompt: String::new(),
            user_prompt: String::new(),
            response_format: ResponseFormat::JsonSchema {
                name: "fixture".into(),
                schema: serde_json::json!({"type":"object"}),
            },
            max_output_tokens: 10,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(1),
            stream: false,
            legacy_provider: "openrouter".into(),
            legacy_model: "openrouter/free".into(),
            dispatch: DispatchPolicy::default(),
        };
        assert_eq!(
            capability_gate_reason(
                &profile,
                "openrouter",
                "cohere/north-mini-code:free",
                &request,
            ),
            Some("capability_structured_output")
        );
        assert_eq!(
            capability_gate_reason(&profile, "openrouter", "openrouter/free", &request),
            None
        );
        assert_eq!(
            capability_gate_reason(&profile, "openrouter", "unknown/model", &request),
            Some("model_not_in_catalog")
        );
        for model in [
            "cohere/north-mini-code:free",
            "nvidia/nemotron-3-ultra-550b-a55b:free",
        ] {
            assert_eq!(
                capability_gate_reason(&profile, "openrouter", model, &request),
                Some("capability_structured_output")
            );
        }

        request.response_format = ResponseFormat::JsonObject;
        assert_eq!(
            capability_gate_reason(
                &profile,
                "openrouter",
                "cohere/north-mini-code:free",
                &request
            ),
            Some("capability_structured_output")
        );
        for provider in ["ollama", "deepseek", "groq"] {
            let existing = profile_by_id(provider).unwrap();
            assert_eq!(
                capability_gate_reason(&existing, provider, &existing.default_model, &request),
                None,
                "{provider} must retain JSON-object support"
            );
        }

        request.response_format = ResponseFormat::Text;
        assert_eq!(
            capability_gate_reason(
                &profile,
                "openrouter",
                "cohere/north-mini-code:free",
                &request,
            ),
            None
        );
    }

    #[test]
    fn openrouter_model_gate_preserves_other_models_on_the_same_connection() {
        use super::super::domain::{
            CandidateDecision, ResolvedConnection, RouteDecision, RouteTarget,
        };
        let targets = ["cohere/north-mini-code:free", "openrouter/free"]
            .into_iter()
            .enumerate()
            .map(|(index, model)| RouteTarget {
                position: index as u32,
                connection_id: "shared-openrouter".into(),
                model_id: model.into(),
                enabled: true,
            })
            .collect::<Vec<_>>();
        let mut decision = RouteDecision {
            route_id: "structured-route".into(),
            strategy: "priority".into(),
            affinity_hash: "fixture".into(),
            candidates: targets
                .iter()
                .map(|target| CandidateDecision {
                    position: target.position,
                    connection_id: target.connection_id.clone(),
                    provider_id: "openrouter".into(),
                    model_id: target.model_id.clone(),
                    eligible: true,
                    gate_reason: None,
                    score: None,
                    score_factors: None,
                })
                .collect(),
            ordered_targets: targets.clone(),
            selected_connection_id: Some(targets[0].connection_id.clone()),
            selected_model_id: Some(targets[0].model_id.clone()),
        };
        let resolved = ResolvedRouteTarget {
            provider_local: false,
            target: targets[0].clone(),
            connection: ResolvedConnection {
                id: "shared-openrouter".into(),
                provider_id: "openrouter".into(),
                credential_ref: "fixture".into(),
                endpoint_url: profile_by_id("openrouter").unwrap().endpoint,
                default_model: "openrouter/free".into(),
                enabled: true,
                state: "active".into(),
                cooldown_until: None,
                backoff_level: 0,
                max_concurrency: 1,
            },
        };
        let gates = HashMap::from([(
            runtime_target_key(&resolved),
            "capability_structured_output".into(),
        )]);
        apply_runtime_gates_to_decision(&mut decision, &gates);
        assert!(!decision.candidates[0].eligible);
        assert!(decision.candidates[1].eligible);
        assert_eq!(decision.ordered_targets.len(), 1);
        assert_eq!(
            decision.selected_model_id.as_deref(),
            Some("openrouter/free")
        );
        let mut default_target = resolved;
        default_target.target.model_id.clear();
        assert!(!gates.contains_key(&runtime_target_key(&default_target)));
    }
}
