use std::future::Future;
use std::pin::Pin;

use futures::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};

pub type EmbeddingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<f64>, EmbeddingError>> + Send + 'a>>;
pub type ModelListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<String>, EmbeddingError>> + Send + 'a>>;
pub type ModelPullFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), EmbeddingError>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct EmbeddingError(pub String);

impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EmbeddingError {}

/// Discrete milestones for a model download, pushed to the UI over `AppEventBus` so the
/// Settings screen can show progress without polling.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelPullEvent {
    Started {
        model: String,
    },
    Progress {
        model: String,
        status: String,
        completed: u64,
        total: u64,
    },
    Completed {
        model: String,
    },
    Failed {
        model: String,
        error: String,
    },
}

pub trait ModelPullEventSink: Send + Sync {
    fn emit(&self, event: &ModelPullEvent);
}

pub trait EmbeddingGateway: Send + Sync {
    fn embed<'a>(&'a self, model: &'a str, prompt: &'a str) -> EmbeddingFuture<'a>;

    /// Names of models already downloaded by the provider. Providers that cannot enumerate
    /// installed models (or don't have the concept) report that plainly instead of guessing.
    fn list_models<'a>(&'a self) -> ModelListFuture<'a> {
        Box::pin(async {
            Err(EmbeddingError(
                "Model listing not supported by this provider.".into(),
            ))
        })
    }

    /// Downloads `model`, calling `on_event` for each milestone. Providers that cannot manage
    /// model downloads report that plainly instead of pretending to succeed.
    fn pull_model<'a>(
        &'a self,
        _model: &'a str,
        _on_event: &'a (dyn Fn(ModelPullEvent) + Send + Sync),
    ) -> ModelPullFuture<'a> {
        Box::pin(async {
            Err(EmbeddingError(
                "Model download not supported by this provider.".into(),
            ))
        })
    }
}

/// Ollama identifies installed models with a `:tag` suffix (defaulting to `:latest`) that the
/// app's own `embedding_model` setting never carries — compare only the base name.
pub fn model_names_match(installed: &str, configured: &str) -> bool {
    fn base(name: &str) -> &str {
        name.split(':').next().unwrap_or(name)
    }
    base(installed) == base(configured)
}

fn validate_embedding_model(model: &str) -> Result<(), EmbeddingError> {
    let unavailable = || EmbeddingError("Embedding model unavailable after sanitization.".into());
    let projected = crate::llm::adapter::sanitize_model_text(model).map_err(|_| unavailable())?;
    if projected != model {
        return Err(unavailable());
    }
    Ok(())
}

pub struct OllamaEmbeddingGateway {
    client: Client,
    base_url: String,
}

impl OllamaEmbeddingGateway {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            base_url: "http://127.0.0.1:11434".to_string(),
        }
    }

    #[cfg(test)]
    fn with_base_url(client: Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into(),
        }
    }
}

impl OllamaEmbeddingGateway {
    /// `/api/embeddings` (legacy, singular) never truncates oversized input — on some
    /// model/Ollama-version combinations (observed with `nomic-embed-text-v2-moe`) a long note
    /// makes the server crash with a bare HTTP 500 instead of a useful error. `/api/embed`
    /// (current) truncates by default and reports real failures with an `error` field, so it is
    /// tried first; the legacy endpoint is only a fallback for Ollama installs old enough not to
    /// have `/api/embed` at all (surfaced as a 404).
    async fn embed_legacy(&self, model: &str, prompt: &str) -> Result<Vec<f64>, EmbeddingError> {
        validate_embedding_model(model)?;
        let prompt = crate::secret_redactor::redact_context(prompt)
            .map_err(|_| {
                EmbeddingError("Embedding content unavailable after sanitization.".into())
            })?
            .text;
        let response = self
            .client
            .post(format!("{}/api/embeddings", self.base_url))
            .json(&json!({"model": model, "prompt": prompt}))
            .send()
            .await
            .map_err(|error| EmbeddingError(format!("Failed to call Ollama: {error}")))?;
        if !response.status().is_success() {
            return Err(EmbeddingError(Self::describe_failure(response).await));
        }
        let parsed = response
            .json::<Value>()
            .await
            .map_err(|error| EmbeddingError(format!("Invalid response from Ollama: {error}")))?;
        let embedding = parsed
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| EmbeddingError("Ollama response without `embedding`.".to_string()))?
            .iter()
            .filter_map(Value::as_f64)
            .collect::<Vec<_>>();
        if embedding.is_empty() {
            return Err(EmbeddingError(
                "Ollama returned an empty embedding.".to_string(),
            ));
        }
        Ok(embedding)
    }

    /// Surfaces Ollama's own `error` field (e.g. "input length exceeds..."), which is far more
    /// actionable than a bare status code — falls back to the status code when the body isn't
    /// the JSON shape Ollama normally returns on failure.
    async fn describe_failure(response: reqwest::Response) -> String {
        let status = response.status().as_u16();
        match response.json::<Value>().await {
            Ok(body) => match body.get("error").and_then(Value::as_str) {
                Some(detail) => {
                    let detail = crate::secret_redactor::redact_secrets(detail)
                        .map(|result| result.text)
                        .unwrap_or_else(|_| "details unavailable after sanitization".into());
                    format!("Ollama embedding failed with HTTP status {status}: {detail}")
                }
                None => format!("Ollama embedding failed with HTTP status {status}."),
            },
            Err(_) => format!("Ollama embedding failed with HTTP status {status}."),
        }
    }
}

impl EmbeddingGateway for OllamaEmbeddingGateway {
    fn embed<'a>(&'a self, model: &'a str, prompt: &'a str) -> EmbeddingFuture<'a> {
        Box::pin(async move {
            validate_embedding_model(model)?;
            let prompt = crate::secret_redactor::redact_context(prompt)
                .map_err(|_| {
                    EmbeddingError("Embedding content unavailable after sanitization.".into())
                })?
                .text;
            let response = self
                .client
                .post(format!("{}/api/embed", self.base_url))
                .json(&json!({"model": model, "input": prompt}))
                .send()
                .await
                .map_err(|error| EmbeddingError(format!("Failed to call Ollama: {error}")))?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return self.embed_legacy(model, &prompt).await;
            }
            if !response.status().is_success() {
                return Err(EmbeddingError(Self::describe_failure(response).await));
            }
            let parsed = response.json::<Value>().await.map_err(|error| {
                EmbeddingError(format!("Invalid response from Ollama: {error}"))
            })?;
            let embedding = parsed
                .get("embeddings")
                .and_then(Value::as_array)
                .and_then(|rows| rows.first())
                .and_then(Value::as_array)
                .ok_or_else(|| EmbeddingError("Ollama response without `embeddings`.".to_string()))?
                .iter()
                .filter_map(Value::as_f64)
                .collect::<Vec<_>>();
            if embedding.is_empty() {
                return Err(EmbeddingError(
                    "Ollama returned an empty embedding.".to_string(),
                ));
            }
            Ok(embedding)
        })
    }

    fn list_models<'a>(&'a self) -> ModelListFuture<'a> {
        Box::pin(async move {
            let response = self
                .client
                .get(format!("{}/api/tags", self.base_url))
                .send()
                .await
                .map_err(|error| EmbeddingError(format!("Failed to call Ollama: {error}")))?;
            if !response.status().is_success() {
                return Err(EmbeddingError(format!(
                    "Ollama answered with HTTP status {} when listing models.",
                    response.status().as_u16()
                )));
            }
            let parsed = response.json::<Value>().await.map_err(|error| {
                EmbeddingError(format!("Invalid response from Ollama: {error}"))
            })?;
            let models = parsed
                .get("models")
                .and_then(Value::as_array)
                .map(|models| {
                    models
                        .iter()
                        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Ok(models)
        })
    }

    fn pull_model<'a>(
        &'a self,
        model: &'a str,
        on_event: &'a (dyn Fn(ModelPullEvent) + Send + Sync),
    ) -> ModelPullFuture<'a> {
        Box::pin(async move {
            validate_embedding_model(model)?;
            on_event(ModelPullEvent::Started {
                model: model.to_string(),
            });
            let send_result = self
                .client
                .post(format!("{}/api/pull", self.base_url))
                .json(&json!({"model": model, "stream": true}))
                .send()
                .await;
            let response = match send_result {
                Ok(response) if response.status().is_success() => response,
                Ok(response) => {
                    let error = format!(
                        "Ollama answered with HTTP status {} when downloading the model.",
                        response.status().as_u16()
                    );
                    on_event(ModelPullEvent::Failed {
                        model: model.to_string(),
                        error: error.clone(),
                    });
                    return Err(EmbeddingError(error));
                }
                Err(error) => {
                    let error = format!("Failed to call Ollama: {error}");
                    on_event(ModelPullEvent::Failed {
                        model: model.to_string(),
                        error: error.clone(),
                    });
                    return Err(EmbeddingError(error));
                }
            };
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut completed_successfully = false;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let error = format!("Download interrompido: {error}");
                        on_event(ModelPullEvent::Failed {
                            model: model.to_string(),
                            error: error.clone(),
                        });
                        return Err(EmbeddingError(error));
                    }
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(newline) = buffer.find('\n') {
                    let line = buffer[..newline].trim().to_string();
                    buffer.drain(..=newline);
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(parsed) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if let Some(error) = parsed.get("error").and_then(Value::as_str) {
                        on_event(ModelPullEvent::Failed {
                            model: model.to_string(),
                            error: error.to_string(),
                        });
                        return Err(EmbeddingError(error.to_string()));
                    }
                    let status = parsed
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if status == "success" {
                        completed_successfully = true;
                    }
                    let completed = parsed.get("completed").and_then(Value::as_u64).unwrap_or(0);
                    let total = parsed.get("total").and_then(Value::as_u64).unwrap_or(0);
                    on_event(ModelPullEvent::Progress {
                        model: model.to_string(),
                        status,
                        completed,
                        total,
                    });
                }
            }
            if completed_successfully {
                on_event(ModelPullEvent::Completed {
                    model: model.to_string(),
                });
                Ok(())
            } else {
                let error = "Ollama ended the download without confirming success.".to_string();
                on_event(ModelPullEvent::Failed {
                    model: model.to_string(),
                    error: error.clone(),
                });
                Err(EmbeddingError(error))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::thread;

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn spawn_http_response(body: &str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake ollama should bind");
        let address = listener.local_addr().expect("fake ollama address");
        let body = body.to_string();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fake ollama should accept");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.flush().unwrap();
        });
        (format!("http://{address}"), handle)
    }

    fn spawn_streaming_response(lines: &[&str]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake ollama should bind");
        let address = listener.local_addr().expect("fake ollama address");
        let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fake ollama should accept");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            for line in lines {
                stream.write_all(line.as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
            }
            stream.flush().unwrap();
        });
        (format!("http://{address}"), handle)
    }

    /// Serves one (status, body) pair per accepted connection, in order — for asserting a
    /// specific sequence of requests (e.g. a 404 on the modern endpoint followed by a 200 on the
    /// legacy fallback).
    fn spawn_sequential_responses(
        responses: Vec<(u16, &'static str, &'static str)>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fake ollama should bind");
        let address = listener.local_addr().expect("fake ollama address");
        let handle = thread::spawn(move || {
            for (status, reason, body) in responses {
                let (mut stream, _) = listener.accept().expect("fake ollama should accept");
                let mut request = [0_u8; 8192];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn embedding_model_metadata_is_checked_before_modern_or_legacy_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let gateway = OllamaEmbeddingGateway::with_base_url(
            Client::builder().no_proxy().build().unwrap(),
            format!("http://{}", listener.local_addr().unwrap()),
        );
        let runtime = runtime();
        for model in [
            "sk-proj-syntheticabcdefghijklmnop",
            r#"{"token":"\u0073k-proj-syntheticabcdefghijklmnop"}"#,
        ] {
            for legacy in [false, true] {
                let error = if legacy {
                    runtime.block_on(gateway.embed_legacy(model, "safe input"))
                } else {
                    runtime.block_on(gateway.embed(model, "safe input"))
                }
                .unwrap_err();
                assert_eq!(error.0, "Embedding model unavailable after sanitization.");
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            }
            let events = Mutex::new(Vec::new());
            let on_event = |event| events.lock().unwrap().push(event);
            let error = runtime
                .block_on(gateway.pull_model(model, &on_event))
                .unwrap_err();
            assert_eq!(error.0, "Embedding model unavailable after sanitization.");
            assert!(events.lock().unwrap().is_empty());
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
        validate_embedding_model("nomic-embed-text:latest").unwrap();
    }

    #[test]
    fn embed_uses_the_modern_endpoint_and_reads_the_first_embedding_row() {
        let (url, handle) =
            spawn_http_response(r#"{"model":"nomic-embed-text","embeddings":[[0.1,0.2,0.3]]}"#);
        let gateway = OllamaEmbeddingGateway::with_base_url(Client::new(), url);
        let vector = runtime()
            .block_on(gateway.embed("nomic-embed-text", "hello"))
            .unwrap();
        assert_eq!(vector, vec![0.1, 0.2, 0.3]);
        handle.join().unwrap();
    }

    #[test]
    fn embedding_modern_and_legacy_wire_payloads_are_sanitized() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in [(404, "{}"), (200, r#"{"embedding":[0.4,0.5]}"#)] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let count = stream.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            seen.push(String::from_utf8(bytes[end + 4..].to_vec()).unwrap());
                            break;
                        }
                    }
                }
                write!(stream,"HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
            seen
        });
        let gateway = OllamaEmbeddingGateway::with_base_url(
            Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
            url,
        );
        runtime()
            .block_on(gateway.embed(
                "nomic-embed-text",
                "OPENAI_API_KEY=sk-proj-syntheticabcdefghijklmnop",
            ))
            .unwrap();
        let payloads = server.join().unwrap();
        assert_eq!(payloads.len(), 2);
        for body in payloads {
            assert!(!body.contains("sk-proj-syntheticabcdefghijklmnop"));
            assert!(body.contains("REDACTED_SECRET"));
        }
    }

    #[test]
    fn embed_falls_back_to_the_legacy_endpoint_when_the_modern_one_is_missing() {
        let (url, handle) = spawn_sequential_responses(vec![
            (404, "Not Found", r#"{"error":"404 page not found"}"#),
            (200, "OK", r#"{"embedding":[0.4,0.5]}"#),
        ]);
        let gateway = OllamaEmbeddingGateway::with_base_url(Client::new(), url);
        let vector = runtime()
            .block_on(gateway.embed("nomic-embed-text", "hello"))
            .unwrap();
        assert_eq!(vector, vec![0.4, 0.5]);
        handle.join().unwrap();
    }

    #[test]
    fn embed_surfaces_ollamas_own_error_detail_on_real_failure() {
        let (url, handle) = spawn_sequential_responses(vec![(
            500,
            "Internal Server Error",
            r#"{"error":"input length exceeds maximum context length"}"#,
        )]);
        let gateway = OllamaEmbeddingGateway::with_base_url(Client::new(), url);
        let error = runtime()
            .block_on(gateway.embed("nomic-embed-text", "a very long note"))
            .unwrap_err();
        assert!(
            error
                .0
                .contains("input length exceeds maximum context length")
        );
        handle.join().unwrap();
    }

    #[test]
    fn list_models_parses_tag_names() {
        let (url, handle) = spawn_http_response(
            r#"{"models":[{"name":"nomic-embed-text:latest"},{"name":"llama3:8b"}]}"#,
        );
        let gateway = OllamaEmbeddingGateway::with_base_url(Client::new(), url);
        let models = runtime().block_on(gateway.list_models()).unwrap();
        assert_eq!(models, vec!["nomic-embed-text:latest", "llama3:8b"]);
        handle.join().unwrap();
    }

    #[test]
    fn list_models_reports_connection_failure_without_panicking() {
        let gateway = OllamaEmbeddingGateway::with_base_url(Client::new(), "http://127.0.0.1:1");
        let error = runtime().block_on(gateway.list_models()).unwrap_err();
        assert!(!error.0.is_empty());
    }

    #[test]
    fn pull_model_streams_progress_then_completes() {
        let (url, handle) = spawn_streaming_response(&[
            r#"{"status":"pulling manifest"}"#,
            r#"{"status":"downloading","completed":50,"total":100}"#,
            r#"{"status":"success"}"#,
        ]);
        let gateway = OllamaEmbeddingGateway::with_base_url(Client::new(), url);
        let events = Mutex::new(Vec::new());
        let result = runtime().block_on(gateway.pull_model("nomic-embed-text", &|event| {
            events.lock().unwrap().push(event);
        }));
        assert!(result.is_ok());
        let events = events.into_inner().unwrap();
        assert!(matches!(events[0], ModelPullEvent::Started { .. }));
        assert!(events.iter().any(|event| matches!(
            event,
            ModelPullEvent::Progress {
                completed: 50,
                total: 100,
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(ModelPullEvent::Completed { .. })
        ));
        handle.join().unwrap();
    }

    #[test]
    fn model_names_match_ignores_tag_suffix() {
        assert!(model_names_match(
            "nomic-embed-text:latest",
            "nomic-embed-text"
        ));
        assert!(!model_names_match("llama3:8b", "nomic-embed-text"));
    }
}
