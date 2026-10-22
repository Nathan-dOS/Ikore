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

#[derive(Clone)]
pub struct AdapterOutput {
    pub request_payload: Value,
    pub response_payload: Value,
    pub content: String,
    pub usage: LlmUsage,
    pub finish_reason: Option<String>,
    pub observed_models: Vec<String>,
    pub output_redaction_count: u64,
    pub output_redaction_categories: Vec<String>,
}

impl std::fmt::Debug for AdapterOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdapterOutput")
            .field("content_bytes", &self.content.len())
            .field("usage", &self.usage)
            .field("observed_model_count", &self.observed_models.len())
            .field("output_redaction_count", &self.output_redaction_count)
            .finish_non_exhaustive()
    }
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
            let mut prepared_request = call.request.clone();
            sanitize_request_schema(&mut prepared_request)?;
            let prepared = match &call.request.dispatch.prepared_input {
                Some(prepared) => {
                    prepared
                        .verify_matches(&call.request.system_prompt, &call.request.user_prompt)
                        .map_err(|_| sanitization_error())?;
                    prepared.clone()
                }
                None => crate::secret_redactor::PreparedModelInput::new(
                    &call.request.system_prompt,
                    &call.request.user_prompt,
                )
                .map_err(|_| sanitization_error())?,
            };
            prepared_request.system_prompt = prepared.system_prompt().to_owned();
            prepared_request.user_prompt = prepared.user_prompt().to_owned();
            prepared_request.dispatch.prepared_input = Some(prepared);
            let call = AdapterCall {
                request: &prepared_request,
                ..call
            };
            let payload = build_payload(&call);
            if serde_json::to_vec(&payload)
                .map_err(|_| sanitization_error())?
                .len()
                > crate::secret_redactor::MAX_SANITIZATION_BYTES
            {
                return Err(sanitization_error());
            }
            validate_final_payload(&payload, &call)?;
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
            let mut output = if call.request.stream && call.profile.capabilities.streaming {
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
            }?;
            let projection = sanitize_model_text_counted(&output.content)?;
            output.output_redaction_count = projection.count;
            output.output_redaction_categories = projection.categories;
            // Operational bytes remain available only to the host parser. All
            // observable projections are independently qualified before use.
            let mut observed_payload = output.response_payload.clone();
            sanitize_json_strings(&mut observed_payload)?;
            if call.request.stream && call.profile.capabilities.streaming {
                delta_sink.push(&projection.text);
            }
            Ok(output)
        })
    }
}

pub(crate) fn sanitization_error() -> LlmError {
    LlmError::new(
        LlmErrorKind::Sanitization,
        "Content unavailable after sanitization.",
    )
}

pub(crate) fn add_stream_bytes(received: &mut usize, incoming: usize) -> Result<(), LlmError> {
    let total = received
        .checked_add(incoming)
        .ok_or_else(sanitization_error)?;
    if total > crate::secret_redactor::MAX_SANITIZATION_BYTES {
        return Err(sanitization_error());
    }
    *received = total;
    Ok(())
}

pub(crate) fn sanitize_request_schema(request: &mut LlmRequest) -> Result<(), LlmError> {
    if let ResponseFormat::JsonSchema { schema, .. } = &mut request.response_format {
        sanitize_json_strings(schema)?;
    }
    if let Some(schema) = &mut request.dispatch.strict_schema {
        sanitize_json_strings(schema)?;
    }
    Ok(())
}

pub(crate) async fn bounded_response_text(mut response: Response) -> Result<String, LlmError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if bytes.len().saturating_add(chunk.len()) > crate::secret_redactor::MAX_SANITIZATION_BYTES
        {
            return Err(sanitization_error());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| {
        LlmError::new(
            LlmErrorKind::InvalidResponse,
            "The provider returned invalid UTF-8.",
        )
    })
}

/// Inspect decoded strings, so JSON escaping cannot hide multiline secrets.
pub(crate) fn sanitize_json_strings(value: &mut Value) -> Result<(), LlmError> {
    sanitize_json_strings_inner(value, &mut OutputRedactionStats::default())
}

#[derive(Default)]
struct OutputRedactionStats {
    count: u64,
    categories: std::collections::BTreeSet<String>,
}

impl OutputRedactionStats {
    fn add(&mut self, result: &crate::secret_redactor::RedactionResult) -> Result<(), LlmError> {
        let count = self
            .count
            .checked_add(result.count)
            .ok_or_else(sanitization_error)?;
        if count > crate::secret_redactor::MAX_REDACTION_SPANS as u64 {
            return Err(sanitization_error());
        }
        self.count = count;
        self.categories.extend(result.categories.iter().cloned());
        Ok(())
    }
}

fn sanitize_json_strings_inner(
    value: &mut Value,
    stats: &mut OutputRedactionStats,
) -> Result<(), LlmError> {
    match value {
        Value::String(text) => {
            *text = sanitize_model_text_inner(text, stats)?;
        }
        Value::Array(values) => {
            for value in values {
                sanitize_json_strings_inner(value, stats)?;
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (key, value) in original {
                let (key, value) = if let Value::String(text) = value {
                    sanitize_json_string_pair(key, text, stats)?
                } else {
                    let key = crate::secret_redactor::redact_secrets(&key)
                        .map_err(|_| sanitization_error())?;
                    stats.add(&key)?;
                    let mut value = value;
                    sanitize_json_strings_inner(&mut value, stats)?;
                    (key.text, value)
                };
                if values.insert(key, value).is_some() {
                    return Err(sanitization_error());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn sanitize_json_string_pair(
    key: String,
    text: String,
    stats: &mut OutputRedactionStats,
) -> Result<(String, Value), LlmError> {
    // Preserve the decoded assignment context for arbitrary password/token
    // values. The engine consolidates overlapping detectors on this pair,
    // instead of counting a contextual value and its token prefix twice.
    let pair = Value::Object(serde_json::Map::from_iter([(key, Value::String(text))]));
    let encoded = serde_json::to_string(&pair).map_err(|_| sanitization_error())?;
    let result =
        crate::secret_redactor::redact_secrets(&encoded).map_err(|_| sanitization_error())?;
    stats.add(&result)?;
    let Value::Object(projected) =
        serde_json::from_str::<Value>(&result.text).map_err(|_| sanitization_error())?
    else {
        return Err(sanitization_error());
    };
    if projected.len() != 1 {
        return Err(sanitization_error());
    }
    let (key, value) = projected
        .into_iter()
        .next()
        .ok_or_else(sanitization_error)?;
    let Value::String(text) = value else {
        return Err(sanitization_error());
    };
    Ok((key, Value::String(sanitize_model_text_inner(&text, stats)?)))
}

pub(crate) fn sanitize_model_text(text: &str) -> Result<String, LlmError> {
    sanitize_model_text_inner(text, &mut OutputRedactionStats::default())
}

pub(crate) fn sanitize_model_text_counted(
    text: &str,
) -> Result<crate::secret_redactor::RedactionResult, LlmError> {
    let mut stats = OutputRedactionStats::default();
    let text = sanitize_model_text_inner(text, &mut stats)?;
    Ok(crate::secret_redactor::RedactionResult {
        text,
        count: stats.count,
        categories: stats.categories.into_iter().collect(),
    })
}

fn sanitize_model_text_inner(
    text: &str,
    stats: &mut OutputRedactionStats,
) -> Result<String, LlmError> {
    if text.len() > crate::secret_redactor::MAX_SANITIZATION_BYTES {
        return Err(sanitization_error());
    }
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        let original = value.clone();
        sanitize_json_strings_inner(&mut value, stats)?;
        if value == original {
            return Ok(text.to_owned());
        }
        return serde_json::to_string(&value).map_err(|_| sanitization_error());
    }
    let result = crate::secret_redactor::redact_secrets(text).map_err(|_| sanitization_error())?;
    stats.add(&result)?;
    Ok(result.text)
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

fn validate_final_payload(payload: &Value, call: &AdapterCall<'_>) -> Result<(), LlmError> {
    let prepared = call
        .request
        .dispatch
        .prepared_input
        .as_ref()
        .ok_or_else(sanitization_error)?;
    prepared
        .verify_matches(&call.request.system_prompt, &call.request.user_prompt)
        .map_err(|_| sanitization_error())?;
    let prompt_slots = match call.profile.protocol {
        ProviderProtocol::OpenaiResponses => [
            ("/instructions", prepared.system_prompt()),
            ("/input", prepared.user_prompt()),
        ],
        ProviderProtocol::OpenaiChat => [
            ("/messages/0/content", prepared.system_prompt()),
            ("/messages/1/content", prepared.user_prompt()),
        ],
        ProviderProtocol::AnthropicMessages => [
            ("/system", prepared.system_prompt()),
            ("/messages/0/content", prepared.user_prompt()),
        ],
    };
    // Exceptions belong only to these exact prepared prompt slots. Removing
    // them from this validation copy prevents a source authorization from
    // becoming an allowlist for equal values in model names or other metadata.
    let mut metadata = payload.clone();
    for (pointer, expected) in prompt_slots {
        let slot = metadata
            .pointer_mut(pointer)
            .ok_or_else(sanitization_error)?;
        if slot.as_str() != Some(expected) {
            return Err(sanitization_error());
        }
        *slot = Value::String("prepared prompt".into());
    }
    let mut projected = metadata.clone();
    sanitize_json_strings(&mut projected)?;
    if projected != metadata {
        return Err(sanitization_error());
    }
    Ok(())
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
    let body = bounded_response_text(response).await?;
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
        result = bounded_response_text(response) => result?,
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
        output_redaction_count: 0,
        output_redaction_categories: Vec::new(),
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
    let mut received_bytes = 0usize;
    loop {
        let chunk = tokio::select! {
            _ = cancellation.cancelled() => return Err(LlmError::new(LlmErrorKind::Cancelled,"Stream LLM cancelado.")),
            result = response.chunk() => result.map_err(|error| stream_error(error, &content))?,
        };
        let Some(chunk) = chunk else { break };
        add_stream_bytes(&mut received_bytes, chunk.len())?;
        let parsed_events = parser.push(&chunk);
        delta_sink.observe_usage(parser.usage());
        let events = parsed_events?;
        if let Some(error) =
            verify_stream_models(provider, requested_model, parser.observed_models())
        {
            return Err(error.with_observed_models(parser.observed_models().to_vec()));
        }
        apply_stream_events(events, &mut content, &mut usage, &mut completed, delta_sink);
        if content.len() > crate::secret_redactor::MAX_SANITIZATION_BYTES {
            return Err(sanitization_error());
        }
    }
    let parsed_events = parser.finish();
    delta_sink.observe_usage(parser.usage());
    let final_events = parsed_events?;
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
    if let Some(error) = verify_stream_models(provider, requested_model, parser.observed_models()) {
        return Err(error.with_observed_models(parser.observed_models().to_vec()));
    }
    if !completed || content.trim().is_empty() {
        let error = LlmError::new(
            LlmErrorKind::StreamFailed,
            "The stream ended before a valid completion.",
        );
        return Err(error);
    }
    Ok(AdapterOutput {
        request_payload: request_payload.clone(),
        response_payload: json!({"streamed":true,"complete":true}),
        content,
        usage,
        finish_reason: Some("stop".to_string()),
        observed_models: parser.observed_models().to_vec(),
        output_redaction_count: 0,
        output_redaction_categories: Vec::new(),
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
                        format!(
                            "Provider `{provider}` reported `{model}`; expected `{requested}`."
                        ),
                    )
                    .with_target(provider, "", requested),
                );
            }
            crate::terminals::model_identity::ModelVerification::Unverified => {
                return Some(
                    LlmError::new(
                        LlmErrorKind::ModelUnverified,
                        format!(
                            "Provider `{provider}` could not verify requested model `{requested}`."
                        ),
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
    _sink: &dyn TokenDeltaSink,
) {
    for event in events {
        match event {
            ParsedStreamEvent::Delta(delta) => {
                content.push_str(&delta);
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

fn stream_error(error: reqwest::Error, _partial: &str) -> LlmError {
    let mut result = network_error(error);
    result.kind = LlmErrorKind::StreamFailed;
    result.message = "Failure while reading the provider stream.".to_string();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::domain::{AgentExecutionTarget, AgentRole, ExecutionId, GenerationId};
    use std::time::{Duration, Instant};

    #[test]
    fn stream_transport_budget_counts_metadata_and_rejects_overflow_before_parsing() {
        let mut received = crate::secret_redactor::MAX_SANITIZATION_BYTES - 1;
        add_stream_bytes(&mut received, 1).unwrap();
        assert_eq!(received, crate::secret_redactor::MAX_SANITIZATION_BYTES);
        for incoming in [1, usize::MAX] {
            let error = add_stream_bytes(&mut received, incoming).unwrap_err();
            assert_eq!(error.kind, LlmErrorKind::Sanitization);
            assert_eq!(received, crate::secret_redactor::MAX_SANITIZATION_BYTES);
        }
    }

    #[test]
    fn decoded_nested_json_and_unicode_escapes_are_sanitized() {
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        let private =
            "-----BEGIN PRIVATE KEY-----\nsynthetic-private-body\n-----END PRIVATE KEY-----";
        let encoded = format!(
            r#"{{"token":"\u0073k-proj-syntheticabcdefghijklmnop","source":{}}}"#,
            serde_json::to_string(private).unwrap()
        );
        let mut response = json!({"output_text":encoded});
        sanitize_json_strings(&mut response).unwrap();
        let projected: Value =
            serde_json::from_str(response["output_text"].as_str().unwrap()).unwrap();
        assert!(!projected.to_string().contains(secret));
        assert!(!projected.to_string().contains("synthetic-private-body"));
        assert!(crate::secret_redactor::contains_redaction_marker(
            projected["token"].as_str().unwrap()
        ));
    }

    #[test]
    fn json_sensitive_fields_keep_decoded_assignment_context_and_count_once() {
        for raw in [
            r#"{"password":"arbitrary multi word secret"}"#,
            r#"{"TWILIO_AUTH_TOKEN":"0123456789abcdef0123456789abcdef"}"#,
            r#"{"\u0070assword":"arbitrary \u006dulti word secret"}"#,
            r#"{"password":"sk-proj-syntheticabcdefghijklmnop"}"#,
            r#"{"outer":"{\"password\":\"arbitrary multi word secret\"}"}"#,
        ] {
            let projected = sanitize_model_text_counted(raw).unwrap();
            assert_eq!(projected.count, 1);
            assert!(crate::secret_redactor::contains_redaction_marker(
                &projected.text
            ));
            assert!(!projected.text.contains("arbitrary"));
            assert!(!projected.text.contains("0123456789abcdef"));
            assert!(!projected.text.contains("sk-proj-syntheticabcdefghijklmnop"));
            assert_eq!(
                sanitize_model_text_counted(&projected.text).unwrap().count,
                0
            );
        }
        let schema = r#"{"type":"object","properties":{"password":{"type":"string"}},"required":["password"],"additionalProperties":false}"#;
        let projected = sanitize_model_text_counted(schema).unwrap();
        assert_eq!(projected.text, schema);
        assert_eq!(projected.count, 0);
    }

    #[test]
    fn output_counts_use_logical_content_without_duplicate_response_payload() {
        let private =
            "-----BEGIN PRIVATE KEY-----\nsynthetic-private-body\n-----END PRIVATE KEY-----";
        let logical = json!({"outer":json!({"token":"sk-proj-syntheticabcdefghijklmnop","source":private}).to_string()}).to_string();
        let projection = sanitize_model_text_counted(&logical).unwrap();
        assert_eq!(projection.count, 2);
        let mut duplicated_payload =
            json!({"output_text":logical,"also_returned":projection.text.clone()});
        sanitize_json_strings(&mut duplicated_payload).unwrap();
        assert_eq!(projection.count, 2);
        assert_eq!(
            sanitize_model_text_counted(&projection.text).unwrap().count,
            0
        );
    }

    #[test]
    fn decoded_json_uses_one_aggregate_redaction_budget() {
        let mut stats = OutputRedactionStats {
            count: crate::secret_redactor::MAX_REDACTION_SPANS as u64 - 1,
            ..OutputRedactionStats::default()
        };
        let raw = r#"{"first":{"password":"arbitrary multi word secret"},"second":{"token":"another arbitrary secret"}}"#;
        let error = sanitize_model_text_inner(raw, &mut stats).unwrap_err();
        assert_eq!(error.kind, LlmErrorKind::Sanitization);
        assert_eq!(
            stats.count,
            crate::secret_redactor::MAX_REDACTION_SPANS as u64
        );
    }

    #[test]
    fn fragmented_stream_text_is_private_until_complete_projection() {
        struct Sink(std::sync::Mutex<String>);
        impl TokenDeltaSink for Sink {
            fn push(&self, text: &str) {
                self.0.lock().unwrap().push_str(text);
            }
        }
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        for split in 1..secret.len() {
            let sink = Sink(Default::default());
            let (mut content, mut usage, mut completed) =
                (String::new(), LlmUsage::default(), false);
            apply_stream_events(
                vec![ParsedStreamEvent::Delta(secret[..split].into())],
                &mut content,
                &mut usage,
                &mut completed,
                &sink,
            );
            assert!(sink.0.lock().unwrap().is_empty());
            apply_stream_events(
                vec![
                    ParsedStreamEvent::Delta(secret[split..].into()),
                    ParsedStreamEvent::Completed,
                ],
                &mut content,
                &mut usage,
                &mut completed,
                &sink,
            );
            assert!(completed);
            assert!(sink.0.lock().unwrap().is_empty());
            assert!(!sanitize_model_text(&content).unwrap().contains(secret));
        }
    }

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

    fn fixture_connection(profile: &ProviderProfile, endpoint: &str) -> ResolvedConnection {
        ResolvedConnection {
            id: "fixture".into(),
            provider_id: profile.id.clone().into(),
            credential_ref: "fixture".into(),
            endpoint_url: endpoint.into(),
            default_model: profile.default_model.clone(),
            enabled: true,
            state: "active".into(),
            cooldown_until: None,
            backoff_level: 0,
            max_concurrency: 1,
        }
    }

    #[test]
    fn final_payload_checks_metadata_without_extending_source_exceptions() {
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        let authorized = 0..secret.len();
        for profile in super::super::profiles::profiles() {
            let connection = fixture_connection(&profile, &profile.endpoint);
            let mut req = request();
            req.user_prompt = secret.into();
            req.dispatch.prepared_input = Some(
                crate::secret_redactor::PreparedModelInput::with_authorized_ranges(
                    &req.system_prompt,
                    &req.user_prompt,
                    "fixture-policy",
                    &[],
                    std::slice::from_ref(&authorized),
                )
                .unwrap(),
            );
            let call = AdapterCall {
                profile: &profile,
                connection: &connection,
                model: &profile.default_model,
                credential: None,
                request: &req,
            };
            let payload = build_payload(&call);
            let original = payload.clone();
            validate_final_payload(&payload, &call).unwrap();
            assert_eq!(payload, original);
            assert!(payload.to_string().contains(secret));

            for field in ["model", "extra_metadata", secret] {
                let mut contaminated = payload.clone();
                contaminated[field] = json!(secret);
                assert_eq!(
                    validate_final_payload(&contaminated, &call)
                        .unwrap_err()
                        .kind,
                    LlmErrorKind::Sanitization
                );
            }
            let mut hidden = payload.clone();
            hidden["extra_metadata"] =
                json!(r#"{"token":"\u0073k-proj-syntheticabcdefghijklmnop"}"#);
            assert_eq!(
                validate_final_payload(&hidden, &call).unwrap_err().kind,
                LlmErrorKind::Sanitization
            );
            let user_pointer = match profile.protocol {
                ProviderProtocol::OpenaiResponses => "/input",
                ProviderProtocol::OpenaiChat => "/messages/1/content",
                ProviderProtocol::AnthropicMessages => "/messages/0/content",
            };
            let mut changed_prompt = payload.clone();
            *changed_prompt.pointer_mut(user_pointer).unwrap() = json!("changed prompt");
            assert_eq!(
                validate_final_payload(&changed_prompt, &call)
                    .unwrap_err()
                    .kind,
                LlmErrorKind::Sanitization
            );
        }
    }

    #[test]
    fn final_payload_rechecks_prepared_and_lowered_schema_metadata() {
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        for profile_id in ["openai", "openrouter"] {
            let profile = super::super::profiles::profile_by_id(profile_id).unwrap();
            let connection = fixture_connection(&profile, &profile.endpoint);
            let mut req = request();
            req.response_format = ResponseFormat::JsonSchema {
                name: "fixture".into(),
                schema: json!({"type":"object","description":secret}),
            };
            req.dispatch.strict_schema =
                Some(json!({"type":"object","description":secret,"additionalProperties":false}));
            sanitize_request_schema(&mut req).unwrap();
            req.dispatch.prepared_input = Some(
                crate::secret_redactor::PreparedModelInput::new(
                    &req.system_prompt,
                    &req.user_prompt,
                )
                .unwrap(),
            );
            let call = AdapterCall {
                profile: &profile,
                connection: &connection,
                model: &profile.default_model,
                credential: None,
                request: &req,
            };
            let payload = build_payload(&call);
            validate_final_payload(&payload, &call).unwrap();
            assert!(!payload.to_string().contains(secret));
            let format_pointer = match profile.protocol {
                ProviderProtocol::OpenaiResponses => "/text/format",
                ProviderProtocol::OpenaiChat => "/response_format/json_schema",
                ProviderProtocol::AnthropicMessages => unreachable!(),
            };
            for field in ["name", "schema/description"] {
                let mut contaminated = payload.clone();
                *contaminated
                    .pointer_mut(&format!("{format_pointer}/{field}"))
                    .unwrap() = json!(secret);
                assert_eq!(
                    validate_final_payload(&contaminated, &call)
                        .unwrap_err()
                        .kind,
                    LlmErrorKind::Sanitization
                );
            }
        }
    }

    #[tokio::test]
    async fn secret_model_and_schema_names_are_rejected_before_http() {
        struct SilentSink;
        impl TokenDeltaSink for SilentSink {
            fn push(&self, _: &str) {
                panic!("invalid input must never produce output");
            }
        }
        let secret = "sk-proj-syntheticabcdefghijklmnop";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        for profile_id in ["openai", "openrouter", "anthropic"] {
            let profile = super::super::profiles::profile_by_id(profile_id).unwrap();
            let connection = fixture_connection(&profile, &endpoint);
            let adapter = ProtocolAdapter::new(
                Client::builder().no_proxy().build().unwrap(),
                profile.protocol,
            );
            for secret_in_model in [true, false] {
                if !secret_in_model && profile.protocol == ProviderProtocol::AnthropicMessages {
                    continue;
                }
                let mut req = request();
                req.response_format = ResponseFormat::JsonSchema {
                    name: if secret_in_model { "fixture" } else { secret }.into(),
                    schema: json!({"type":"object"}),
                };
                let error = adapter
                    .execute(
                        AdapterCall {
                            profile: &profile,
                            connection: &connection,
                            model: if secret_in_model {
                                secret
                            } else {
                                &profile.default_model
                            },
                            credential: Some("fixture-credential"),
                            request: &req,
                        },
                        &CancellationToken::new(),
                        &SilentSink,
                    )
                    .await
                    .unwrap_err();
                assert_eq!(error.kind, LlmErrorKind::Sanitization);
                assert!(!error.message.contains(secret));
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            }
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
        let operational_edit = r#"{"edits":[{"path":"src/config.rs","old":"sk-proj-syntheticabcdefghijklmnop","new":"configured-by-environment"}]}"#;
        for (status, stream, body, expected_error) in [
            (200, false, json!({"model":"openrouter/free","choices":[{"message":{"content":"{\"ok\":true}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":4}}).to_string(), None),
            (200, true, ": OPENROUTER PROCESSING\n\ndata: {\"model\":\"openrouter/free\",\"choices\":[{\"delta\":{\"content\":\"{\\\"ok\\\":\"}}]}\n\ndata: {\"model\":\"openrouter/free\",\"choices\":[{\"delta\":{\"content\":\"true}\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"model\":\"openrouter/free\",\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":4}}\n\ndata: [DONE]\n\n".into(), None),
            (200, true, format!("data: {}\n\ndata: [DONE]\n\n", json!({"model":"openrouter/free","choices":[{"delta":{"content":operational_edit},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":4}})), None),
            (401, false, r#"{"error":{"message":"Invalid API key","code":401}}"#.into(), Some(LlmErrorKind::Authentication)),
            (429, false, r#"{"error":{"message":"Rate limited","code":429}}"#.into(), Some(LlmErrorKind::RateLimited)),
        ] {
            let expected_content = if body.contains("sk-proj-syntheticabcdefghijklmnop") { operational_edit } else { "{\"ok\":true}" };
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
            req.system_prompt = "Authorization: Bearer synthetic-system-token".into();
            req.user_prompt = "OPENAI_API_KEY=sk-proj-syntheticabcdefghijklmnop".into();
            req.response_format = ResponseFormat::JsonSchema { name: "fixture".into(), schema: schema.clone() };
            let sink = Deltas::default();
            let adapter = ProtocolAdapter::new(Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap(), profile.protocol);
            let result = adapter.execute(AdapterCall { profile: &profile, connection: &connection, model: "openrouter/free", credential: Some("sk-or-v1-local-fixture"), request: &req }, &CancellationToken::new(), &sink).await;
            if expected_error.is_none() { assert!(result.is_ok(), "adapter stopped before completion: {:?}", result.as_ref().err().map(|error| (&error.kind, &error.message))); }
            let payload = server.await.unwrap();
            assert_eq!(payload["model"], "openrouter/free");
            assert_eq!(payload["messages"][0]["role"], "system");
            assert_eq!(payload["stream"], stream);
            assert!(!payload.to_string().contains("synthetic-system-token"));
            assert!(!payload.to_string().contains("sk-proj-syntheticabcdefghijklmnop"));
            assert_eq!(payload["response_format"]["json_schema"]["schema"], schema);
            assert_eq!(payload["response_format"]["json_schema"]["strict"], true);
            assert_eq!(payload["provider"]["require_parameters"], true);
            assert!(!payload.to_string().contains("sk-or-v1-local-fixture"));
            if let Some(kind) = expected_error {
                assert_eq!(result.unwrap_err().kind, kind);
            } else {
                let output = result.unwrap();
                assert_eq!(output.content, expected_content);
                assert_eq!(output.usage.input_tokens, Some(7));
                assert_eq!(output.usage.output_tokens, Some(4));
                if stream {
                    assert_eq!(payload["stream_options"]["include_usage"], true);
                    assert_eq!(*sink.0.lock().unwrap(), sanitize_model_text(expected_content).unwrap());
                }
                if expected_content == operational_edit {
                    let secret = "sk-proj-syntheticabcdefghijklmnop";
                    if stream {
                        // SSE retains its exact logical content separately;
                        // the payload is a host completion receipt, not the wire body.
                        assert_eq!(output.response_payload, json!({"streamed":true,"complete":true}));
                    } else {
                        assert!(output.response_payload.to_string().contains(secret));
                    }
                    assert!(!format!("{output:?}").contains(secret));
                    let response = super::super::domain::LlmResponse {
                        execution_id: req.execution_id.clone(), generation_id: req.generation_id.clone(),
                        route_id: "fixture-route".into(), connection_id: connection.id.clone(), provider_id: connection.provider_id.clone(),
                        model: "openrouter/free".into(), request_payload: output.request_payload, response_payload: output.response_payload,
                        content: output.content, usage: output.usage, estimated_cost_usd: None, finish_reason: output.finish_reason,
                    };
                    assert_eq!(response.content, operational_edit);
                    let edits: Value = serde_json::from_str(&response.content).unwrap();
                    assert_eq!(edits["edits"][0]["old"],secret);
                    assert!(!format!("{response:?}").contains(secret));
                    assert!(!sink.0.lock().unwrap().contains(secret));
                    let root = tempfile::tempdir().unwrap();
                    let path = root.path().join("observations.db");
                    let db = rusqlite::Connection::open(&path).unwrap();
                    db.execute_batch("CREATE TABLE llm_route_executions(id TEXT PRIMARY KEY,status TEXT,next_eligible_at TEXT,visible_output TEXT,visible_output_complete INTEGER,finished_at TEXT); INSERT INTO llm_route_executions(id) VALUES('e');").unwrap();
                    super::super::repository::SqliteLlmRepository::new(&path).finish_execution("e","completed",None,Some(&response.content),true).unwrap();
                    let observed: String = db.query_row("SELECT visible_output FROM llm_route_executions WHERE id='e'",[],|row|row.get(0)).unwrap();
                    assert!(!observed.contains(secret));
                    assert!(crate::secret_redactor::contains_redaction_marker(&observed));
                    assert_eq!(response.content,operational_edit);
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
