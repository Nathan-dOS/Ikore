use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RedactionResult {
    pub text: String,
    pub count: u64,
    pub categories: Vec<String>,
}

#[derive(Debug)]
pub struct SecretRedactor {
    patterns: Vec<RedactionPattern>,
}

#[derive(Debug)]
struct RedactionPattern {
    category: &'static str,
    regex: Regex,
    replacement: &'static str,
}

impl SecretRedactor {
    pub fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            patterns: vec![
                RedactionPattern {
                    category: "private_key",
                    regex: Regex::new(
                        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                    )?,
                    replacement: "[REDACTED PRIVATE KEY]",
                },
                RedactionPattern {
                    category: "bearer_token",
                    regex: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}")?,
                    replacement: "Bearer [REDACTED]",
                },
                RedactionPattern {
                    category: "known_api_token",
                    regex: Regex::new(
                        r"(?i)\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{12,}|AIza[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9_]{12,}|xox[baprs]-[A-Za-z0-9-]{12,})\b",
                    )?,
                    replacement: "[REDACTED API TOKEN]",
                },
                RedactionPattern {
                    category: "sensitive_variable",
                    regex: Regex::new(
                        r#"(?i)\b((?:OPENAI|ANTHROPIC|GOOGLE|GEMINI|GITHUB|AWS|AZURE|SUPABASE|DATABASE)_[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIALS?)\s*[:=]\s*)["']?[^\s"',;]{4,}"#,
                    )?,
                    replacement: "$1[REDACTED]",
                },
                RedactionPattern {
                    category: "credential_url",
                    regex: Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@")?,
                    replacement: "$1[REDACTED]@",
                },
                RedactionPattern {
                    category: "credential_path",
                    regex: Regex::new(
                        r"(?i)(?:[A-Z]:\\|/)(?:[^\s\r\n]+[/\\])?(?:\.ssh[/\\]id_(?:rsa|ed25519)|\.aws[/\\]credentials|application_default_credentials\.json|credentials\.(?:json|ya?ml|toml))",
                    )?,
                    replacement: "[REDACTED CREDENTIAL PATH]",
                },
            ],
        })
    }

    pub fn redact(&self, input: &str) -> RedactionResult {
        let mut text = input.to_string();
        let mut count = 0u64;
        let mut categories = Vec::new();
        for pattern in &self.patterns {
            let matches = pattern.regex.find_iter(&text).count() as u64;
            if matches == 0 {
                continue;
            }
            count = count.saturating_add(matches);
            categories.push(pattern.category.to_string());
            text = pattern
                .regex
                .replace_all(&text, pattern.replacement)
                .into_owned();
        }
        RedactionResult {
            text,
            count,
            categories,
        }
    }
}

pub fn redact_secrets(input: &str) -> Result<RedactionResult, String> {
    static REDACTOR: OnceLock<Result<SecretRedactor, String>> = OnceLock::new();
    REDACTOR
        .get_or_init(|| SecretRedactor::new().map_err(|error| error.to_string()))
        .as_ref()
        .map(|redactor| redactor.redact(input))
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_supported_secret_categories_without_storing_values() {
        let input = "Authorization: Bearer abcdefghijklmnop\nOPENAI_API_KEY=fixture-secret-value\npostgres://user:pass@example.test/db\n-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----";
        let result = redact_secrets(input).unwrap();
        assert_eq!(result.count, 4);
        assert!(!result.text.contains("abcdefghijklmnop"));
        assert!(!result.text.contains("fixture-secret-value"));
        assert!(!result.text.contains("user:pass"));
        assert!(!result.text.contains("\nfixture\n"));
        assert_eq!(result.categories.len(), 4);
    }
}
