use std::future::Future;
use std::pin::Pin;

use reqwest::Client;
use serde_json::{json, Value};

pub type EmbeddingFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<f64>, EmbeddingError>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct EmbeddingError(pub String);

impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EmbeddingError {}

pub trait EmbeddingGateway: Send + Sync {
    fn embed<'a>(&'a self, model: &'a str, prompt: &'a str) -> EmbeddingFuture<'a>;
}

pub struct OllamaEmbeddingGateway {
    client: Client,
}

impl OllamaEmbeddingGateway {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

impl EmbeddingGateway for OllamaEmbeddingGateway {
    fn embed<'a>(&'a self, model: &'a str, prompt: &'a str) -> EmbeddingFuture<'a> {
        Box::pin(async move {
            let response = self
                .client
                .post("http://127.0.0.1:11434/api/embeddings")
                .json(&json!({"model": model, "prompt": prompt}))
                .send()
                .await
                .map_err(|error| EmbeddingError(format!("Falha ao chamar Ollama: {error}")))?;
            if !response.status().is_success() {
                return Err(EmbeddingError(format!(
                    "Ollama embedding falhou com status HTTP {}.",
                    response.status().as_u16()
                )));
            }
            let parsed = response
                .json::<Value>()
                .await
                .map_err(|error| EmbeddingError(format!("Resposta inválida do Ollama: {error}")))?;
            let embedding = parsed
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| EmbeddingError("Resposta do Ollama sem `embedding`.".to_string()))?
                .iter()
                .filter_map(Value::as_f64)
                .collect::<Vec<_>>();
            if embedding.is_empty() {
                return Err(EmbeddingError(
                    "Embedding vazio retornado pelo Ollama.".to_string(),
                ));
            }
            Ok(embedding)
        })
    }
}
