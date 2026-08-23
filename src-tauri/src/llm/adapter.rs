use std::future::Future;
use std::pin::Pin;

use reqwest::{Client, Response};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::domain::{LlmRequest, LlmUsage, ProviderProtocol, ResolvedConnection, ResponseFormat};
use super::error::{LlmError, LlmErrorKind};
use super::profiles::{AuthStyle, ProviderProfile};
use super::resilience::parse_retry_after;
use super::streaming::{ParsedStreamEvent, SseParser};

#[derive(Debug, Clone)]
pub struct AdapterCall<'a> {
    pub profile: &'a ProviderProfile,
    pub connection: &'a ResolvedConnection,
    pub model: &'a str,
    pub credential: Option<&'a str>,
    pub request: &'a LlmRequest,
}

#[derive(Debug, Clone)]
pub struct AdapterOutput {
    pub request_payload: Value,
    pub response_payload: Value,
    pub content: String,
    pub usage: LlmUsage,
    pub finish_reason: Option<String>,
}

pub trait TokenDeltaSink: Send + Sync {
    fn push(&self, delta: &str);
}

pub type AdapterFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AdapterOutput, LlmError>> + Send + 'a>>;

pub trait LlmAdapter: Send + Sync {
    fn protocol(&self) -> ProviderProtocol;
    fn execute<'a>(
        &'a self,
        call: AdapterCall<'a>,
        cancellation: &'a CancellationToken,
        delta_sink: &'a dyn TokenDeltaSink,
    ) -> AdapterFuture<'a>;
}

#[derive(Debug, Clone)]
pub struct ProtocolAdapter {
    client: Client,
    protocol: ProviderProtocol,
}

impl ProtocolAdapter {
    pub fn new(client: Client, protocol: ProviderProtocol) -> Self {
        Self { client, protocol }
    }
}

impl LlmAdapter for ProtocolAdapter {
    fn protocol(&self) -> ProviderProtocol {
        self.protocol
    }

    fn execute<'a>(
        &'a self,
        call: AdapterCall<'a>,
        cancellation: &'a CancellationToken,
        delta_sink: &'a dyn TokenDeltaSink,
    ) -> AdapterFuture<'a> {
        Box::pin(async move {
            let payload = build_payload(&call);
            let mut request = self
                .client
                .post(&call.connection.endpoint_url)
                .json(&payload);
            match call.profile.auth_style {
                AuthStyle::Bearer => {
                    let credential = call.credential.ok_or_else(|| {
                        LlmError::new(LlmErrorKind::MissingApiKey, "Conexão sem API key.")
                            .with_target(&call.profile.id, call.connection.id.as_str(), call.model)
                    })?;
                    request = request.bearer_auth(credential);
                }
                AuthStyle::Anthropic => {
                    let credential = call.credential.ok_or_else(|| {
                        LlmError::new(LlmErrorKind::MissingApiKey, "Conexão sem API key.")
                            .with_target(&call.profile.id, call.connection.id.as_str(), call.model)
                    })?;
                    request = request
                        .header("x-api-key", credential)
                        .header("anthropic-version", "2023-06-01");
                }
                AuthStyle::None => {}
            }

            let response = tokio::select! {
                _ = cancellation.cancelled() => return Err(LlmError::new(LlmErrorKind::Cancelled, "Execução LLM cancelada.")),
                result = request.send() => result.map_err(network_error)?,
            };
            let response =
                validate_status(response, call.profile, call.connection, call.model).await?;
            if call.request.stream && call.profile.capabilities.streaming {
                consume_stream(response, &payload, self.protocol, cancellation, delta_sink).await
            } else {
                consume_complete(response, &payload, self.protocol, cancellation).await
            }
        })
    }
}

fn build_payload(call: &AdapterCall<'_>) -> Value {
    let stream = call.request.stream && call.profile.capabilities.streaming;
    match call.profile.protocol {
        ProviderProtocol::OpenaiResponses => {
            let text_format = match &call.request.response_format {
                ResponseFormat::Text => None,
                ResponseFormat::JsonObject => Some(json!({"type":"json_object"})),
                ResponseFormat::JsonSchema { name, schema } => Some(json!({
                    "type":"json_schema", "name":name, "schema":schema, "strict":true
                })),
            };
            let mut payload = json!({
                "model": call.model,
                "instructions": call.request.system_prompt,
                "input": call.request.user_prompt,
                "max_output_tokens": call.request.max_output_tokens,
                "temperature": call.request.temperature,
                "stream": stream
            });
            if let Some(format) = text_format {
                payload["text"] = json!({"format": format});
            }
            payload
        }
        ProviderProtocol::OpenaiChat => {
            let mut payload = json!({
                "model": call.model,
                "messages": [
                    {"role":"system","content":call.request.system_prompt},
                    {"role":"user","content":call.request.user_prompt}
                ],
                "max_tokens": call.request.max_output_tokens,
                "temperature": call.request.temperature,
                "stream": stream
            });
            if call.profile.capabilities.json_schema {
                if let ResponseFormat::JsonSchema { name, schema } = &call.request.response_format {
                    payload["response_format"] = json!({"type":"json_schema","json_schema":{"name":name,"schema":schema,"strict":true}});
                }
            } else if call.profile.capabilities.json_object
                && call.request.response_format.requires_structured_output()
            {
                payload["response_format"] = json!({"type":"json_object"});
            }
            if call.profile.id == "groq" {
                for parameter in &call.profile.forbidden_parameters {
                    payload
                        .as_object_mut()
                        .map(|object| object.remove(parameter));
                }
            }
            payload
        }
        ProviderProtocol::AnthropicMessages => json!({
            "model": call.model,
            "system": call.request.system_prompt,
            "messages": [{"role":"user","content":call.request.user_prompt}],
            "max_tokens": call.request.max_output_tokens,
            "temperature": call.request.temperature,
            "stream": stream
        }),
    }
}

async fn validate_status(
    response: Response,
    profile: &ProviderProfile,
    connection: &ResolvedConnection,
    model: &str,
) -> Result<Response, LlmError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status().as_u16();
    let retry_after_ms = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|header| header.to_str().ok())
        .and_then(|value| parse_retry_after(value, chrono::Utc::now()));
    let body = response.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let message = parsed
        .pointer("/error/message")
        .or_else(|| parsed.pointer("/message"))
        .and_then(Value::as_str)
        .unwrap_or("Provider retornou erro HTTP.");
    let provider_code = parsed
        .pointer("/error/code")
        .or_else(|| parsed.pointer("/error/type"))
        .and_then(Value::as_str);
    Err(profile
        .classify_http_error(status, provider_code, message, retry_after_ms)
        .with_target(&profile.id, connection.id.as_str(), model))
}

async fn consume_complete(
    response: Response,
    request_payload: &Value,
    protocol: ProviderProtocol,
    cancellation: &CancellationToken,
) -> Result<AdapterOutput, LlmError> {
    let body = tokio::select! {
        _ = cancellation.cancelled() => return Err(LlmError::new(LlmErrorKind::Cancelled,"Execução LLM cancelada.")),
        result = response.text() => result.map_err(network_error)?,
    };
    let parsed: Value = serde_json::from_str(&body).map_err(|_| {
        LlmError::new(
            LlmErrorKind::InvalidResponse,
            "Provider retornou JSON inválido.",
        )
    })?;
    let (content, usage, finish_reason) = parse_complete(protocol, &parsed)?;
    Ok(AdapterOutput {
        request_payload: request_payload.clone(),
        response_payload: parsed,
        content,
        usage,
        finish_reason,
    })
}

async fn consume_stream(
    mut response: Response,
    request_payload: &Value,
    protocol: ProviderProtocol,
    cancellation: &CancellationToken,
    delta_sink: &dyn TokenDeltaSink,
) -> Result<AdapterOutput, LlmError> {
    let mut parser = SseParser::new(protocol);
    let mut content = String::new();
    let mut usage = LlmUsage::default();
    let mut completed = false;
    loop {
        let chunk = tokio::select! {
            _ = cancellation.cancelled() => return Err(LlmError::new(LlmErrorKind::Cancelled,"Stream LLM cancelado.")),
            result = response.chunk() => result.map_err(|error| stream_error(error, &content))?,
        };
        let Some(chunk) = chunk else { break };
        let events = parser.push(&chunk).map_err(|mut error| {
            if !content.is_empty() {
                error.visible_partial = Some(content.clone());
            }
            error
        })?;
        apply_stream_events(events, &mut content, &mut usage, &mut completed, delta_sink);
    }
    let final_events = parser.finish().map_err(|mut error| {
        if !content.is_empty() {
            error.visible_partial = Some(content.clone());
        }
        error
    })?;
    apply_stream_events(
        final_events,
        &mut content,
        &mut usage,
        &mut completed,
        delta_sink,
    );
    if !completed || content.trim().is_empty() {
        let mut error = LlmError::new(
            LlmErrorKind::StreamFailed,
            "Stream terminou antes da conclusão válida.",
        );
        if !content.is_empty() {
            error.visible_partial = Some(content);
        }
        return Err(error);
    }
    Ok(AdapterOutput {
        request_payload: request_payload.clone(),
        response_payload: json!({"streamed":true,"complete":true}),
        content,
        usage,
        finish_reason: Some("stop".to_string()),
    })
}

fn apply_stream_events(
    events: Vec<ParsedStreamEvent>,
    content: &mut String,
    usage: &mut LlmUsage,
    completed: &mut bool,
    sink: &dyn TokenDeltaSink,
) {
    for event in events {
        match event {
            ParsedStreamEvent::Delta(delta) => {
                content.push_str(&delta);
                sink.push(&delta);
            }
            ParsedStreamEvent::Completed => *completed = true,
            ParsedStreamEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                if input_tokens.is_some() {
                    usage.input_tokens = input_tokens;
                }
                if output_tokens.is_some() {
                    usage.output_tokens = output_tokens;
                }
            }
            ParsedStreamEvent::ToolCallStarted { .. }
            | ParsedStreamEvent::ToolArgumentsDelta { .. }
            | ParsedStreamEvent::ToolCallCompleted { .. } => {}
        }
    }
}

fn parse_complete(
    protocol: ProviderProtocol,
    parsed: &Value,
) -> Result<(String, LlmUsage, Option<String>), LlmError> {
    let (content, input_tokens, output_tokens, finish_reason) = match protocol {
        ProviderProtocol::OpenaiResponses => {
            let content = parsed
                .get("output_text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    parsed
                        .get("output")
                        .and_then(Value::as_array)
                        .and_then(|items| {
                            items
                                .iter()
                                .flat_map(|item| {
                                    item.get("content")
                                        .and_then(Value::as_array)
                                        .into_iter()
                                        .flatten()
                                })
                                .find_map(|part| {
                                    part.get("text")
                                        .and_then(Value::as_str)
                                        .map(ToOwned::to_owned)
                                })
                        })
                })
                .unwrap_or_default();
            (
                content,
                parsed
                    .pointer("/usage/input_tokens")
                    .and_then(Value::as_i64),
                parsed
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_i64),
                parsed
                    .get("status")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            )
        }
        ProviderProtocol::OpenaiChat => {
            let choice = parsed
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|items| items.first());
            (
                choice
                    .and_then(|item| item.pointer("/message/content"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                parsed
                    .pointer("/usage/prompt_tokens")
                    .and_then(Value::as_i64),
                parsed
                    .pointer("/usage/completion_tokens")
                    .and_then(Value::as_i64),
                choice
                    .and_then(|item| item.get("finish_reason"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            )
        }
        ProviderProtocol::AnthropicMessages => {
            let content = parsed
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find(|item| item.get("type").and_then(Value::as_str) == Some("text"))
                })
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            (
                content,
                parsed
                    .pointer("/usage/input_tokens")
                    .and_then(Value::as_i64),
                parsed
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_i64),
                parsed
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            )
        }
    };
    if content.trim().is_empty() {
        return Err(LlmError::new(
            LlmErrorKind::InvalidResponse,
            "Provider retornou resposta vazia.",
        ));
    }
    Ok((
        content,
        LlmUsage {
            input_tokens,
            output_tokens,
        },
        finish_reason,
    ))
}

fn network_error(error: reqwest::Error) -> LlmError {
    let kind = if error.is_timeout() {
        LlmErrorKind::Timeout
    } else {
        LlmErrorKind::Network
    };
    LlmError::new(
        kind,
        if kind == LlmErrorKind::Timeout {
            "Tempo limite na chamada do provider."
        } else {
            "Falha de rede na chamada do provider."
        },
    )
}

fn stream_error(error: reqwest::Error, partial: &str) -> LlmError {
    let mut result = network_error(error);
    result.kind = LlmErrorKind::StreamFailed;
    result.message = "Falha durante leitura do stream do provider.".to_string();
    if !partial.is_empty() {
        result.visible_partial = Some(partial.to_string());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::domain::{AgentRole, ExecutionId, GenerationId};
    use std::time::{Duration, Instant};

    fn request() -> LlmRequest {
        LlmRequest {
            execution_id: ExecutionId::new("e"),
            generation_id: GenerationId::new("g"),
            project_id: "p".to_string(),
            pulsation_id: None,
            job_id: None,
            job_chain: "j".to_string(),
            agent_role: AgentRole::BackendAgent,
            stage: "Code".to_string(),
            system_prompt: "system".to_string(),
            user_prompt: "user".to_string(),
            response_format: ResponseFormat::JsonObject,
            max_output_tokens: 100,
            temperature: 0.2,
            deadline: Instant::now() + Duration::from_secs(1),
            stream: false,
            legacy_provider: "openai".to_string(),
            legacy_model: "model".to_string(),
        }
    }

    #[test]
    fn openai_responses_payload_uses_text_format() {
        let profile = super::super::profiles::profile_by_id("openai").unwrap();
        let connection = ResolvedConnection {
            id: "c".to_string().into(),
            provider_id: "openai".to_string().into(),
            credential_ref: "c".to_string(),
            endpoint_url: profile.endpoint.clone(),
            default_model: "m".to_string(),
            enabled: true,
            state: "active".to_string(),
            cooldown_until: None,
            backoff_level: 0,
            max_concurrency: 1,
        };
        let req = request();
        let payload = build_payload(&AdapterCall {
            profile: &profile,
            connection: &connection,
            model: "m",
            credential: Some("secret"),
            request: &req,
        });
        assert_eq!(
            payload.pointer("/text/format/type").and_then(Value::as_str),
            Some("json_object")
        );
        assert_eq!(
            payload.get("max_output_tokens").and_then(Value::as_u64),
            Some(req.max_output_tokens as u64)
        );
        assert!(payload.get("messages").is_none());
    }

    #[test]
    fn every_profile_builds_a_protocol_specific_payload() {
        let mut req = request();
        req.response_format = ResponseFormat::JsonSchema {
            name: "fixture".to_string(),
            schema: json!({"type":"object"}),
        };
        for profile in super::super::profiles::profiles() {
            let connection = ResolvedConnection {
                id: format!("{}-connection", profile.id).into(),
                provider_id: profile.id.clone().into(),
                credential_ref: "fixture".to_string(),
                endpoint_url: profile.endpoint.clone(),
                default_model: profile.default_model.clone(),
                enabled: true,
                state: "active".to_string(),
                cooldown_until: None,
                backoff_level: 0,
                max_concurrency: 1,
            };
            let payload = build_payload(&AdapterCall {
                profile: &profile,
                connection: &connection,
                model: &profile.default_model,
                credential: Some("write-only"),
                request: &req,
            });
            assert_eq!(
                payload.get("model").and_then(Value::as_str),
                Some(profile.default_model.as_str())
            );
            match profile.protocol {
                ProviderProtocol::OpenaiResponses => {
                    assert!(payload.get("input").is_some());
                    assert_eq!(
                        payload.pointer("/text/format/type").and_then(Value::as_str),
                        Some("json_schema")
                    );
                }
                ProviderProtocol::OpenaiChat => {
                    assert!(payload.get("messages").is_some());
                    if profile.id == "groq" {
                        for forbidden in profile.forbidden_parameters {
                            assert!(payload.get(forbidden).is_none());
                        }
                    }
                }
                ProviderProtocol::AnthropicMessages => {
                    assert!(payload.get("system").is_some());
                    assert!(payload.get("response_format").is_none());
                }
            }
        }
    }

    #[test]
    fn every_profile_classifies_common_http_failures() {
        for profile in super::super::profiles::profiles() {
            assert_eq!(
                profile.classify_http_error(401, None, "auth", None).kind,
                LlmErrorKind::Authentication
            );
            assert_eq!(
                profile.classify_http_error(402, None, "billing", None).kind,
                LlmErrorKind::Billing
            );
            assert_eq!(
                profile
                    .classify_http_error(429, None, "rate", Some(2000))
                    .kind,
                LlmErrorKind::RateLimited
            );
            assert_eq!(
                profile
                    .classify_http_error(503, None, "unavailable", None)
                    .kind,
                LlmErrorKind::ProviderUnavailable
            );
        }
    }

    #[test]
    fn complete_parsers_extract_usage_for_all_protocols() {
        let (chat, usage, _) = parse_complete(
            ProviderProtocol::OpenaiChat,
            &json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}),
        ).unwrap();
        assert_eq!(chat, "ok");
        assert_eq!(usage.input_tokens, Some(3));
        let (anthropic, usage, _) = parse_complete(
            ProviderProtocol::AnthropicMessages,
            &json!({"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":4,"output_tokens":1}}),
        ).unwrap();
        assert_eq!(anthropic, "ok");
        assert_eq!(usage.output_tokens, Some(1));
        let (responses, usage, _) = parse_complete(
            ProviderProtocol::OpenaiResponses,
            &json!({"output_text":"ok","usage":{"input_tokens":5,"output_tokens":2},"status":"completed"}),
        ).unwrap();
        assert_eq!(responses, "ok");
        assert_eq!(usage.input_tokens, Some(5));
    }
}
