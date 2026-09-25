use std::future::Future;
use std::pin::Pin;

use reqwest::{Client, Response};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::domain::{
    LlmRequest, LlmUsage, ProviderProtocol, ResolvedConnection, ResponseFormat,
    effective_response_format,
};
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
    pub observed_models: Vec<String>,
}

pub trait TokenDeltaSink: Send + Sync {
    fn push(&self, delta: &str);
    fn observe_usage(&self, _usage: &crate::usage::UsageMeasurement) {}
}

pub struct AccountingSink<'a> {
    pub inner: &'a dyn TokenDeltaSink,
    pub recorder: &'a crate::usage::api::UsageRecorder,
}

impl TokenDeltaSink for AccountingSink<'_> {
    fn push(&self, delta: &str) {
        self.inner.push(delta);
    }
    fn observe_usage(&self, usage: &crate::usage::UsageMeasurement) {
        self.recorder.observe(usage);
    }
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
                        LlmError::new(
                            LlmErrorKind::MissingApiKey,
                            "The connection has no API key.",
                        )
                        .with_target(
                            &call.profile.id,
                            call.connection.id.as_str(),
                            call.model,
                        )
                    })?;
                    request = request.bearer_auth(credential);
                }
                AuthStyle::Anthropic => {
                    let credential = call.credential.ok_or_else(|| {
                        LlmError::new(
                            LlmErrorKind::MissingApiKey,
                            "The connection has no API key.",
                        )
                        .with_target(
                            &call.profile.id,
                            call.connection.id.as_str(),
                            call.model,
                        )
                    })?;
                    request = request
                        .header("x-api-key", credential)
                        .header("anthropic-version", "2023-06-01");
                }
                AuthStyle::None => {}
            }

            let response = tokio::select! {
                _ = cancellation.cancelled() => return Err(LlmError::new(LlmErrorKind::Cancelled, "LLM execution cancelled.")),
                result = request.send() => result.map_err(network_error)?,
            };
            let response = validate_status(
                response,
                call.profile,
                call.connection,
                call.model,
                delta_sink,
            )
            .await?;
            if call.request.stream && call.profile.capabilities.streaming {
                consume_stream(
                    response,
                    &payload,
                    self.protocol,
                    &call.profile.id,
                    call.model,
                    cancellation,
                    delta_sink,
                )
                .await
            } else {
                consume_complete(response, &payload, self.protocol, cancellation, delta_sink).await
            }
        })
    }
}

fn build_payload(call: &AdapterCall<'_>) -> Value {
    let stream = call.request.stream && call.profile.capabilities.streaming;
    let response_format = effective_response_format(call.request, call.profile);
    match call.profile.protocol {
        ProviderProtocol::OpenaiResponses => {
            let text_format = match &response_format {
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
            if stream && call.profile.capabilities.usage {
                payload["stream_options"] = json!({"include_usage":true});
            }
            match &response_format {
                ResponseFormat::JsonSchema { name, schema }
                    if call.profile.capabilities.json_schema =>
                {
                    payload["response_format"] = json!({"type":"json_schema","json_schema":{"name":name,"schema":schema,"strict":true}});
                }
                ResponseFormat::JsonObject | ResponseFormat::JsonSchema { .. }
                    if call.profile.capabilities.json_object =>
                {
                    payload["response_format"] = json!({"type":"json_object"});
                }
                _ => {}
            }
            if call.profile.id == "openrouter" && payload.get("response_format").is_some() {
                // A model may have both schema-capable and incompatible upstream endpoints.
                payload["provider"] = json!({"require_parameters":true});
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
    delta_sink: &dyn TokenDeltaSink,
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
    delta_sink.observe_usage(&crate::usage::api::parse_usage(profile.protocol, &parsed));
    let message = parsed
        .pointer("/error/message")
        .or_else(|| parsed.pointer("/message"))
        .and_then(Value::as_str)
        .unwrap_or("The provider returned an HTTP error.");
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
    delta_sink: &dyn TokenDeltaSink,
) -> Result<AdapterOutput, LlmError> {
    let body = tokio::select! {
        _ = cancellation.cancelled() => return Err(LlmError::new(LlmErrorKind::Cancelled,"LLM execution cancelled.")),
        result = response.text() => result.map_err(network_error)?,
    };
    let parsed: Value = serde_json::from_str(&body).map_err(|_| {
        LlmError::new(
            LlmErrorKind::InvalidResponse,
            "The provider returned invalid JSON.",
        )
    })?;
    delta_sink.observe_usage(&crate::usage::api::parse_usage(protocol, &parsed));
    let (content, usage, finish_reason) = parse_complete(protocol, &parsed)?;
    let observed_models = provider_models(&parsed);
    Ok(AdapterOutput {
        request_payload: request_payload.clone(),
        response_payload: parsed,
        content,
        usage,
        finish_reason,
        observed_models,
    })
}

async fn consume_stream(
    mut response: Response,
    request_payload: &Value,
    protocol: ProviderProtocol,
    provider: &str,
    requested_model: &str,
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
        let parsed_events = parser.push(&chunk);
        delta_sink.observe_usage(parser.usage());
        let events = parsed_events.map_err(|mut error| {
            if !content.is_empty() {
                error.visible_partial = Some(content.clone());
            }
            error
        })?;
        if let Some(error) = verify_stream_models(
            provider,
            requested_model,
            parser.observed_models(),
        ) {
            return Err(error.with_observed_models(parser.observed_models().to_vec()));
        }
        apply_stream_events(events, &mut content, &mut usage, &mut completed, delta_sink);
    }
    let parsed_events = parser.finish();
    delta_sink.observe_usage(parser.usage());
    let final_events = parsed_events.map_err(|mut error| {
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
    if parser.observed_models().is_empty() {
        return Err(LlmError::new(
            LlmErrorKind::ModelUnverified,
            format!("Provider `{provider}` did not report which model produced the generation."),
        )
        .with_target(provider, "", requested_model));
    }
    if let Some(error) = verify_stream_models(
        provider,
        requested_model,
        parser.observed_models(),
    ) {
        return Err(error.with_observed_models(parser.observed_models().to_vec()));
    }
    if !completed || content.trim().is_empty() {
        let mut error = LlmError::new(
            LlmErrorKind::StreamFailed,
            "The stream ended before a valid completion.",
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
        observed_models: parser.observed_models().to_vec(),
    })
}

fn verify_stream_models(provider: &str, requested: &str, observed: &[String]) -> Option<LlmError> {
    for model in observed {
        match crate::terminals::model_identity::verify_model(
            Some(provider),
            Some(requested),
            Some(model),
        ) {
            crate::terminals::model_identity::ModelVerification::Verified => {}
            crate::terminals::model_identity::ModelVerification::Mismatch => {
                return Some(
                    LlmError::new(
                        LlmErrorKind::ModelMismatch,
                        format!("Provider `{provider}` reported `{model}`; expected `{requested}`."),
                    )
                    .with_target(provider, "", requested),
                );
            }
            crate::terminals::model_identity::ModelVerification::Unverified => {
                return Some(
                    LlmError::new(
                        LlmErrorKind::ModelUnverified,
                        format!("Provider `{provider}` could not verify requested model `{requested}`."),
                    )
                    .with_target(provider, "", requested),
                );
            }
        }
    }
    None
}

fn provider_models(payload: &Value) -> Vec<String> {
    fn visit(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for key in ["model", "model_id", "modelId", "modelID", "modelVersion"] {
                    if let Some(model) = object
                        .get(key)
                        .and_then(Value::as_str)
                        .filter(|model| !model.trim().is_empty())
                        && !found.iter().any(|item| item == model)
                    {
                        found.push(model.to_owned());
                    }
                }
                for value in object.values() {
                    visit(value, found);
                }
            }
            Value::Array(values) => values.iter().for_each(|value| visit(value, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    visit(payload, &mut found);
    found
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
            "The provider call timed out."
        } else {
            "Network error while calling the provider."
        },
    )
}

fn stream_error(error: reqwest::Error, partial: &str) -> LlmError {
    let mut result = network_error(error);
    result.kind = LlmErrorKind::StreamFailed;
    result.message = "Failure while reading the provider stream.".to_string();
    if !partial.is_empty() {
        result.visible_partial = Some(partial.to_string());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::domain::{AgentExecutionTarget, AgentRole, ExecutionId, GenerationId};
    use std::time::{Duration, Instant};

    fn request() -> LlmRequest {
        LlmRequest {
            execution_id: ExecutionId::new("e"),
            generation_id: GenerationId::new("g"),
            project_id: "p".to_string(),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            job_chain: "j".to_string(),
            target: AgentExecutionTarget::canonical(AgentRole::BackendAgent),
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
            dispatch: Default::default(),
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
    fn openrouter_chat_payload_uses_structured_json_and_stream_usage_options() {
        let profile = super::super::profiles::profile_by_id("openrouter").unwrap();
        let connection = ResolvedConnection {
            id: "openrouter-connection".to_string().into(),
            provider_id: "openrouter".to_string().into(),
            credential_ref: "openrouter-connection".to_string(),
            endpoint_url: profile.endpoint.clone(),
            default_model: profile.default_model.clone(),
            enabled: true,
            state: "active".to_string(),
            cooldown_until: None,
            backoff_level: 0,
            max_concurrency: 1,
        };
        let mut req = request();
        req.stream = true;
        let payload = build_payload(&AdapterCall {
            profile: &profile,
            connection: &connection,
            model: "openrouter/free",
            credential: Some("sk-or-v1-fixture"),
            request: &req,
        });
        assert_eq!(
            payload
                .pointer("/response_format/type")
                .and_then(Value::as_str),
            Some("json_object")
        );
        assert_eq!(
            payload
                .pointer("/stream_options/include_usage")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(payload["model"], "openrouter/free");
        assert_eq!(payload["provider"]["require_parameters"], true);
        assert!(payload.get("input").is_none());
        assert!(!payload.to_string().contains("sk-or-v1-fixture"));
    }

    #[tokio::test]
    async fn openrouter_http_auth_complete_stream_usage_and_errors() {
        use std::sync::Mutex;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        #[derive(Default)]
        struct Deltas(Mutex<String>);
        impl TokenDeltaSink for Deltas {
            fn push(&self, delta: &str) {
                self.0.lock().unwrap().push_str(delta);
            }
        }

        let profile = super::super::profiles::profile_by_id("openrouter").unwrap();
        let schema = json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false});
        for (status, stream, body, expected_error) in [
            (200, false, json!({"model":"openrouter/free","choices":[{"message":{"content":"{\"ok\":true}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":4}}).to_string(), None),
            (200, true, ": OPENROUTER PROCESSING\n\ndata: {\"model\":\"openrouter/free\",\"choices\":[{\"delta\":{\"content\":\"{\\\"ok\\\":\"}}]}\n\ndata: {\"model\":\"openrouter/free\",\"choices\":[{\"delta\":{\"content\":\"true}\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"model\":\"openrouter/free\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":4}}\n\ndata: [DONE]\n\n".into(), None),
            (401, false, r#"{"error":{"message":"Invalid API key","code":401}}"#.into(), Some(LlmErrorKind::Authentication)),
            (429, false, r#"{"error":{"message":"Rate limited","code":429}}"#.into(), Some(LlmErrorKind::RateLimited)),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/api/v1/chat/completions", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    let header_end = loop {
                        let mut chunk = [0_u8; 4096];
                        let count = socket.read(&mut chunk).await.unwrap();
                        assert!(count > 0, "request must have complete headers");
                        bytes.extend_from_slice(&chunk[..count]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break end; }
                    };
                    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                    assert!(headers.starts_with("POST /api/v1/chat/completions HTTP/1.1"));
                    assert!(headers.lines().any(|line| line.eq_ignore_ascii_case("authorization: Bearer sk-or-v1-local-fixture")));
                    assert!(!headers.to_ascii_lowercase().contains("x-api-key:"));
                    let length: usize = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse().unwrap())
                    }).unwrap();
                    while bytes.len() < header_end + 4 + length {
                        let mut chunk = [0_u8; 4096];
                        let count = socket.read(&mut chunk).await.unwrap();
                        assert!(count > 0);
                        bytes.extend_from_slice(&chunk[..count]);
                    }
                    let payload: Value = serde_json::from_slice(&bytes[header_end + 4..]).unwrap();
                    let content_type = if stream { "text/event-stream" } else { "application/json" };
                    socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                    for chunk in body.as_bytes().chunks(11) {
                        socket.write_all(chunk).await.unwrap();
                        tokio::task::yield_now().await;
                    }
                    payload
                }).await.expect("loopback fixture must finish within its deadline")
            });
            // Only this test's resolved connection uses loopback; the production profile stays fixed.
            let connection = ResolvedConnection {
                id: "openrouter-http".into(), provider_id: "openrouter".into(),
                credential_ref: "fixture".into(), endpoint_url: endpoint,
                default_model: "openrouter/free".into(), enabled: true, state: "active".into(),
                cooldown_until: None, backoff_level: 0, max_concurrency: 1,
            };
            let mut req = request();
            req.stream = stream;
            req.response_format = ResponseFormat::JsonSchema { name: "fixture".into(), schema: schema.clone() };
            let sink = Deltas::default();
            let adapter = ProtocolAdapter::new(Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap(), profile.protocol);
            let result = adapter.execute(AdapterCall { profile: &profile, connection: &connection, model: "openrouter/free", credential: Some("sk-or-v1-local-fixture"), request: &req }, &CancellationToken::new(), &sink).await;
            let payload = server.await.unwrap();
            assert_eq!(payload["model"], "openrouter/free");
            assert_eq!(payload["messages"][0]["role"], "system");
            assert_eq!(payload["stream"], stream);
            assert_eq!(payload["response_format"]["json_schema"]["schema"], schema);
            assert_eq!(payload["response_format"]["json_schema"]["strict"], true);
            assert_eq!(payload["provider"]["require_parameters"], true);
            assert!(!payload.to_string().contains("sk-or-v1-local-fixture"));
            if let Some(kind) = expected_error {
                assert_eq!(result.unwrap_err().kind, kind);
            } else {
                let output = result.unwrap();
                assert_eq!(output.content, "{\"ok\":true}");
                assert_eq!(output.usage.input_tokens, Some(7));
                assert_eq!(output.usage.output_tokens, Some(4));
                if stream {
                    assert_eq!(payload["stream_options"]["include_usage"], true);
                    assert_eq!(*sink.0.lock().unwrap(), output.content);
                }
            }
        }
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
                    assert_eq!(
                        payload["response_format"]["type"],
                        if profile.capabilities.json_schema {
                            "json_schema"
                        } else {
                            "json_object"
                        },
                        "{} must preserve the schema or JSON-object fallback",
                        profile.id
                    );
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
