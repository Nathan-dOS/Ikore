use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use ikore_core::embedding_service::{
    EmbeddingError, EmbeddingFuture, EmbeddingGateway, ModelListFuture, ModelPullEvent,
    ModelPullFuture,
};
use ikore_core::llm::{Clock, CredentialStore, JitterSource, LlmError, LlmErrorKind};

#[derive(Clone)]
pub struct ManualClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(now)),
        }
    }

    pub fn advance(&self, duration: Duration) {
        let mut now = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += duration;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(f64);

impl FixedJitter {
    pub fn new(factor: f64) -> Self {
        Self(factor.clamp(0.8, 1.2))
    }
}

impl Default for FixedJitter {
    fn default() -> Self {
        Self(1.0)
    }
}

impl JitterSource for FixedJitter {
    fn factor(&self, _key: &str, _attempt: u32) -> f64 {
        self.0
    }
}

#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    values: Arc<Mutex<HashMap<String, String>>>,
    failure: Arc<Mutex<Option<String>>>,
    set_calls: Arc<AtomicUsize>,
}

impl MemoryCredentialStore {
    pub fn fail_with(&self, message: impl Into<String>) {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message.into());
    }

    pub fn clear_failure(&self) {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    pub fn contains(&self, credential_ref: &str) -> bool {
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(credential_ref)
    }

    pub fn set_call_count(&self) -> usize {
        self.set_calls.load(Ordering::SeqCst)
    }

    #[allow(clippy::result_large_err)]
    fn configured_failure(&self) -> Result<(), LlmError> {
        let failure = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match failure {
            Some(message) => Err(LlmError::new(LlmErrorKind::Persistence, message)),
            None => Ok(()),
        }
    }
}

impl fmt::Debug for MemoryCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let entry_count = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("MemoryCredentialStore")
            .field("entry_count", &entry_count)
            .finish_non_exhaustive()
    }
}

// A assinatura é definida pelo contrato de produção e mantém o DTO Serde por valor.
#[allow(clippy::result_large_err)]
impl CredentialStore for MemoryCredentialStore {
    fn get(&self, credential_ref: &str) -> Result<Option<String>, LlmError> {
        self.configured_failure()?;
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(credential_ref)
            .cloned())
    }

    fn set(&self, credential_ref: &str, secret: &str) -> Result<(), LlmError> {
        self.set_calls.fetch_add(1, Ordering::SeqCst);
        self.configured_failure()?;
        let normalized = secret.trim();
        if normalized.is_empty() {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "credencial de teste não pode ser vazia",
            ));
        }
        self.values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(credential_ref.to_string(), normalized.to_string());
        Ok(())
    }

    fn delete(&self, credential_ref: &str) -> Result<bool, LlmError> {
        self.configured_failure()?;
        Ok(self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(credential_ref)
            .is_some())
    }
}

#[derive(Clone)]
pub struct FixedEmbeddingGateway {
    result: Arc<Mutex<Result<Vec<f64>, String>>>,
    calls: Arc<Mutex<Vec<(String, String)>>>,
    models: Arc<Mutex<Vec<String>>>,
    list_models_error: Arc<Mutex<Option<String>>>,
    pull_error: Arc<Mutex<Option<String>>>,
}

impl FixedEmbeddingGateway {
    pub fn success(values: Vec<f64>) -> Self {
        Self {
            result: Arc::new(Mutex::new(Ok(values))),
            calls: Arc::new(Mutex::new(Vec::new())),
            models: Arc::new(Mutex::new(Vec::new())),
            list_models_error: Arc::new(Mutex::new(None)),
            pull_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn fail_with(message: impl Into<String>) -> Self {
        Self {
            result: Arc::new(Mutex::new(Err(message.into()))),
            calls: Arc::new(Mutex::new(Vec::new())),
            models: Arc::new(Mutex::new(Vec::new())),
            list_models_error: Arc::new(Mutex::new(None)),
            pull_error: Arc::new(Mutex::new(None)),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Marks `models` as already installed, for `list_models()`/readiness checks.
    pub fn set_installed_models(&self, models: Vec<String>) {
        *self
            .models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = models;
    }

    /// Makes `list_models()` fail, simulating Ollama being unreachable.
    pub fn fail_list_models_with(&self, message: impl Into<String>) {
        *self
            .list_models_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message.into());
    }

    /// Makes `pull_model()` fail after `Started`, instead of the default `Started`→`Completed`.
    pub fn fail_pull_with(&self, message: impl Into<String>) {
        *self
            .pull_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message.into());
    }
}

impl Default for FixedEmbeddingGateway {
    fn default() -> Self {
        Self::success(vec![0.25, 0.5, 0.75])
    }
}

impl EmbeddingGateway for FixedEmbeddingGateway {
    fn embed<'a>(&'a self, model: &'a str, prompt: &'a str) -> EmbeddingFuture<'a> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((model.to_string(), prompt.to_string()));
        let result = self
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move { result.map_err(EmbeddingError) })
    }

    fn list_models<'a>(&'a self) -> ModelListFuture<'a> {
        let error = self
            .list_models_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let models = self
            .models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            match error {
                Some(message) => Err(EmbeddingError(message)),
                None => Ok(models),
            }
        })
    }

    fn pull_model<'a>(
        &'a self,
        model: &'a str,
        on_event: &'a (dyn Fn(ModelPullEvent) + Send + Sync),
    ) -> ModelPullFuture<'a> {
        let error = self
            .pull_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Box::pin(async move {
            on_event(ModelPullEvent::Started {
                model: model.to_string(),
            });
            match error {
                Some(message) => {
                    on_event(ModelPullEvent::Failed {
                        model: model.to_string(),
                        error: message.clone(),
                    });
                    Err(EmbeddingError(message))
                }
                None => {
                    on_event(ModelPullEvent::Completed {
                        model: model.to_string(),
                    });
                    Ok(())
                }
            }
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemoryClipboard {
    text: Arc<Mutex<Option<String>>>,
}

impl MemoryClipboard {
    pub fn write(&self, text: impl Into<String>) {
        *self
            .text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(text.into());
    }

    pub fn read(&self) -> Option<String> {
        self.text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn clear(&self) {
        *self
            .text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScriptedPathPicker {
    responses: Arc<Mutex<VecDeque<Option<Vec<PathBuf>>>>>,
}

impl ScriptedPathPicker {
    pub fn push(&self, response: Option<Vec<PathBuf>>) {
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(response);
    }

    pub fn pick(&self) -> Option<Vec<PathBuf>> {
        self.responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_clipboard_and_picker_are_deterministic() {
        let clock = ManualClock::new("2026-08-23T12:00:00Z".parse().unwrap());
        clock.advance(Duration::seconds(3));
        assert_eq!(clock.now().to_rfc3339(), "2026-08-23T12:00:03+00:00");

        let clipboard = MemoryClipboard::default();
        clipboard.write("texto");
        assert_eq!(clipboard.read().as_deref(), Some("texto"));

        let picker = ScriptedPathPicker::default();
        picker.push(None);
        picker.push(Some(vec![PathBuf::from("C:/fixture")]));
        assert!(picker.pick().is_none());
        assert_eq!(picker.pick().unwrap(), vec![PathBuf::from("C:/fixture")]);
    }

    #[test]
    fn credential_debug_output_never_contains_secret() {
        let store = MemoryCredentialStore::default();
        store.set("provider-a", "super-secret").unwrap();

        let debug = format!("{store:?}");
        assert!(store.contains("provider-a"));
        assert!(!debug.contains("super-secret"));
        assert!(debug.contains("entry_count"));
    }
}
