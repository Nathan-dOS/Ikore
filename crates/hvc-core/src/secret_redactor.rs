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
                    // OAuth access tokens read for the plan-quota meter (ADR-028) are JWTs and
                    // can surface without a `Bearer` prefix, e.g. in a JSON body or an error.
                    category: "jwt_token",
                    regex: Regex::new(
                        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*",
                    )?,
                    replacement: "[REDACTED JWT]",
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

    #[test]
    fn jwt_shaped_tokens_are_redacted() {
        // Synthetic: base64url of {"alg":"none"} . {"sub":"fixture"} . "synthetic-signature".
        let jwt = [
            "eyJhbGciOiJub25lIn0",
            "eyJzdWIiOiJmaXh0dXJlIn0",
            "c3ludGhldGljLXNpZ25hdHVyZQ",
        ]
        .join(".");
        let unsigned = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIn0.";
        for input in [
            format!(r#"{{"access_token":"{jwt}","expires_in":3600}}"#),
            format!("request failed for token {jwt}: 401"),
            format!("token={unsigned} trailing"),
        ] {
            let result = redact_secrets(&input).unwrap();
            assert_eq!(result.categories, ["jwt_token"], "{input}");
            assert!(!result.text.contains("eyJzdWIi"), "{}", result.text);
            assert!(result.text.contains("[REDACTED JWT]"));
        }

        let bearer = redact_secrets(&format!("Authorization: Bearer {jwt}")).unwrap();
        assert_eq!(bearer.categories, ["bearer_token"]);
        assert!(!bearer.text.contains("eyJzdWIi"));

        let harmless = "eyJ is how base64 JSON starts; v1.2.3 and a.b.c stay";
        assert_eq!(redact_secrets(harmless).unwrap().text, harmless);
    }
}
