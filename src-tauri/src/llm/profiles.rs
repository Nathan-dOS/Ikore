use serde::{Deserialize, Serialize};

use super::domain::ProviderProtocol;
use super::error::{LlmError, LlmErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStyle {
    Bearer,
    Anthropic,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub json_object: bool,
    pub json_schema: bool,
    pub usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfile {
    pub id: String,
    pub display_name: String,
    pub protocol: ProviderProtocol,
    pub endpoint: String,
    pub endpoint_configurable: bool,
    pub auth_style: AuthStyle,
    pub capabilities: ProviderCapabilities,
    pub forbidden_parameters: Vec<String>,
    pub default_model: String,
    pub local: bool,
}

impl ProviderProfile {
    pub fn classify_http_error(
        &self,
        status: u16,
        provider_code: Option<&str>,
        message: &str,
        retry_after_ms: Option<u64>,
    ) -> LlmError {
        let normalized_code = provider_code.unwrap_or("").to_ascii_lowercase();
        let normalized_message = message.to_ascii_lowercase();
        let model_scoped = normalized_code.contains("model")
            || normalized_message.contains("model not found")
            || normalized_message.contains("model_not_found")
            || (self.id == "gemini" && normalized_message.contains("model"));

        let kind = match status {
            400 => LlmErrorKind::InvalidRequest,
            401 => LlmErrorKind::Authentication,
            402 => LlmErrorKind::Billing,
            403 | 404 if model_scoped => LlmErrorKind::ModelNotFound,
            403 => LlmErrorKind::Permission,
            404 => LlmErrorKind::InvalidRequest,
            408 => LlmErrorKind::Timeout,
            429 => LlmErrorKind::RateLimited,
            500..=599 => LlmErrorKind::ProviderUnavailable,
            _ => LlmErrorKind::InvalidResponse,
        };

        let mut error = LlmError::new(kind, sanitize_provider_message(message));
        error.http_status = Some(status);
        error.provider_code = provider_code.map(ToOwned::to_owned);
        error.retry_after_ms = retry_after_ms;
        error
    }

    pub fn resolved_endpoint(&self, configured: Option<&str>) -> Result<String, LlmError> {
        if !self.endpoint_configurable {
            return Ok(self.endpoint.clone());
        }

        let candidate = configured.unwrap_or(self.endpoint.as_str()).trim();
        let parsed = reqwest::Url::parse(candidate).map_err(|_| {
            LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Endpoint do Ollama deve ser uma URL válida.",
            )
        })?;
        let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
        let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
        if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
            return Err(LlmError::new(
                LlmErrorKind::InvalidRequest,
                "Endpoint Ollama aceita HTTP somente em loopback; use HTTPS fora da máquina local.",
            ));
        }

        let mut endpoint = candidate.trim_end_matches('/').to_string();
        if !endpoint.ends_with("/v1/chat/completions") {
            endpoint.push_str("/v1/chat/completions");
        }
        Ok(endpoint)
    }
}

pub fn profiles() -> Vec<ProviderProfile> {
    vec![
        profile(
            "openai",
            "OpenAI",
            ProviderProtocol::OpenaiResponses,
            "https://api.openai.com/v1/responses",
            AuthStyle::Bearer,
            "gpt-4.1-mini",
            false,
            true,
            true,
            vec![],
        ),
        profile(
            "openai_legacy",
            "OpenAI legado",
            ProviderProtocol::OpenaiChat,
            "https://api.openai.com/v1/chat/completions",
            AuthStyle::Bearer,
            "gpt-4.1-mini",
            false,
            false,
            true,
            vec![],
        ),
        profile(
            "anthropic",
            "Anthropic",
            ProviderProtocol::AnthropicMessages,
            "https://api.anthropic.com/v1/messages",
            AuthStyle::Anthropic,
            "claude-sonnet-4-20250514",
            false,
            false,
            false,
            vec![],
        ),
        profile(
            "gemini",
            "Gemini",
            ProviderProtocol::OpenaiChat,
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
            AuthStyle::Bearer,
            "gemini-2.5-flash",
            false,
            true,
            true,
            vec![],
        ),
        profile(
            "deepseek",
            "DeepSeek",
            ProviderProtocol::OpenaiChat,
            "https://api.deepseek.com/chat/completions",
            AuthStyle::Bearer,
            "deepseek-chat",
            false,
            true,
            false,
            vec![],
        ),
        profile(
            "groq",
            "Groq",
            ProviderProtocol::OpenaiChat,
            "https://api.groq.com/openai/v1/chat/completions",
            AuthStyle::Bearer,
            "llama-3.3-70b-versatile",
            false,
            true,
            false,
            vec!["logprobs", "logit_bias", "top_logprobs"],
        ),
        profile(
            "mistral",
            "Mistral",
            ProviderProtocol::OpenaiChat,
            "https://api.mistral.ai/v1/chat/completions",
            AuthStyle::Bearer,
            "mistral-small-latest",
            false,
            true,
            true,
            vec![],
        ),
        ProviderProfile {
            id: "ollama".to_string(),
            display_name: "Ollama".to_string(),
            protocol: ProviderProtocol::OpenaiChat,
            endpoint: "http://127.0.0.1:11434/v1/chat/completions".to_string(),
            endpoint_configurable: true,
            auth_style: AuthStyle::None,
            capabilities: ProviderCapabilities {
                streaming: true,
                json_object: true,
                json_schema: false,
                usage: true,
            },
            forbidden_parameters: vec![],
            default_model: "qwen2.5-coder:7b".to_string(),
            local: true,
        },
    ]
}

fn profile(
    id: &str,
    display_name: &str,
    protocol: ProviderProtocol,
    endpoint: &str,
    auth_style: AuthStyle,
    default_model: &str,
    local: bool,
    json_object: bool,
    json_schema: bool,
    forbidden_parameters: Vec<&str>,
) -> ProviderProfile {
    ProviderProfile {
        id: id.to_string(),
        display_name: display_name.to_string(),
        protocol,
        endpoint: endpoint.to_string(),
        endpoint_configurable: false,
        auth_style,
        capabilities: ProviderCapabilities {
            streaming: true,
            json_object,
            json_schema,
            usage: true,
        },
        forbidden_parameters: forbidden_parameters
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        default_model: default_model.to_string(),
        local,
    }
}

pub fn profile_by_id(id: &str) -> Option<ProviderProfile> {
    profiles().into_iter().find(|profile| profile.id == id)
}

fn sanitize_provider_message(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return "Provider retornou um erro sem mensagem.".to_string();
    }
    let without_bearer = regex::Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]+")
        .expect("regex bearer válida")
        .replace_all(trimmed, "Bearer [REDACTED]");
    let without_keys = regex::Regex::new(r"(?i)(api[_ -]?key[=: ]+)\S+")
        .expect("regex api key válida")
        .replace_all(&without_bearer, "$1[REDACTED]");
    without_keys.chars().take(500).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_seven_providers_and_legacy_openai_profile() {
        let profiles = profiles();
        assert_eq!(profiles.len(), 8);
        for expected in [
            "openai",
            "openai_legacy",
            "anthropic",
            "gemini",
            "deepseek",
            "groq",
            "mistral",
            "ollama",
        ] {
            assert!(profiles.iter().any(|profile| profile.id == expected));
        }
    }

    #[test]
    fn ollama_rejects_non_loopback_http() {
        let ollama = profile_by_id("ollama").unwrap();
        assert!(ollama
            .resolved_endpoint(Some("http://example.com:11434"))
            .is_err());
        assert!(ollama
            .resolved_endpoint(Some("http://127.0.0.1:11434"))
            .is_ok());
        assert!(ollama
            .resolved_endpoint(Some("https://ollama.example.com"))
            .is_ok());
    }

    #[test]
    fn model_lockout_requires_model_scoped_evidence() {
        let profile = profile_by_id("openai").unwrap();
        assert_eq!(
            profile
                .classify_http_error(404, None, "not found", None)
                .kind,
            LlmErrorKind::InvalidRequest
        );
        assert_eq!(
            profile
                .classify_http_error(404, Some("model_not_found"), "model not found", None)
                .kind,
            LlmErrorKind::ModelNotFound
        );
    }
}
