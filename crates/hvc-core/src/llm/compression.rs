use std::time::{Duration, Instant};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_COMPRESSION_INPUT_BYTES: usize = 8 * 1024 * 1024;
pub const COMPRESSION_BUDGET: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    ToolOutput,
    VaultProse,
    SystemPrompt,
    Json,
    Yaml,
    Code,
    Diff,
    Adr,
    Contract,
    Requirement,
    Specification,
    Unknown,
}

impl ContentKind {
    pub fn compressible(self, engine: CompressionEngine) -> bool {
        match engine {
            CompressionEngine::Rtk => self == Self::ToolOutput,
            CompressionEngine::Caveman => self == Self::VaultProse,
            CompressionEngine::None => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionEngine {
    None,
    Rtk,
    Caveman,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionPreviewRequest {
    pub content: String,
    pub content_kind: ContentKind,
    pub engine: CompressionEngine,
    #[serde(default)]
    pub caveman_opt_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionPreview {
    pub engine: String,
    pub engine_version: String,
    pub filter_name: Option<String>,
    pub content_kind: ContentKind,
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub saved_bytes: usize,
    pub duration_ms: u64,
    pub applied: bool,
    pub result: String,
    pub output: String,
    pub original_sha256: String,
}

pub fn preview_compression(request: CompressionPreviewRequest) -> CompressionPreview {
    let started = Instant::now();
    let original = request.content;
    let original_bytes = original.len();
    let hash = sha256(original.as_bytes());

    let (candidate, filter_name, result) = if original_bytes > MAX_COMPRESSION_INPUT_BYTES {
        (original.clone(), None, "input_too_large".to_string())
    } else if !request.content_kind.compressible(request.engine) {
        (
            original.clone(),
            None,
            "ineligible_content_kind".to_string(),
        )
    } else if request.engine == CompressionEngine::Caveman && !request.caveman_opt_in {
        (original.clone(), None, "opt_in_required".to_string())
    } else {
        match request.engine {
            CompressionEngine::Rtk => {
                let (output, filter) = compress_tool_output(&redact(&original));
                (output, Some(filter), "compressed".to_string())
            }
            CompressionEngine::Caveman => (
                compress_vault_prose(&redact(&original)),
                Some("vault_prose".to_string()),
                "compressed".to_string(),
            ),
            CompressionEngine::None => (original.clone(), None, "disabled".to_string()),
        }
    };

    let elapsed = started.elapsed();
    let within_budget = elapsed <= COMPRESSION_BUDGET;
    let applied = within_budget && candidate.len() < original.len();
    let output = if applied { candidate } else { original.clone() };
    let final_result = if !within_budget {
        "timeout_fail_open".to_string()
    } else if result == "compressed" && !applied {
        "inflation_guard".to_string()
    } else {
        result
    };

    CompressionPreview {
        engine: match request.engine {
            CompressionEngine::None => "none",
            CompressionEngine::Rtk => "rtk",
            CompressionEngine::Caveman => "caveman",
        }
        .to_string(),
        engine_version: "hvc-1".to_string(),
        filter_name,
        content_kind: request.content_kind,
        original_bytes,
        compressed_bytes: output.len(),
        saved_bytes: original_bytes.saturating_sub(output.len()),
        duration_ms: elapsed.as_millis() as u64,
        applied,
        result: final_result,
        output,
        original_sha256: hash,
    }
}

fn compress_tool_output(input: &str) -> (String, String) {
    let lower = input.to_ascii_lowercase();
    let filter = if lower.contains("clippy") {
        "clippy"
    } else if lower.contains("cargo") || lower.contains("rustc") {
        "cargo"
    } else if lower.contains("vitest") || lower.contains("test files") {
        "vitest"
    } else if lower.contains("typescript") || lower.contains("tsc") {
        "typescript"
    } else if lower.contains("npm") {
        "npm"
    } else if lower.contains("git ") || lower.contains("diff --git") {
        "git"
    } else {
        "ansi_generic"
    };

    let ansi = Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("regex ANSI válida");
    let without_ansi = ansi.replace_all(input, "");
    let progress =
        Regex::new(r"(?m)^\s*(Compiling|Checking|Downloading|Downloaded|Fresh)\s+[^\n]+\n?")
            .expect("regex progresso válida");
    let without_progress = progress.replace_all(&without_ansi, "");

    let mut output = Vec::new();
    let mut previous = None::<String>;
    let mut repeated = 0usize;
    for line in without_progress.lines() {
        let normalized = line.trim_end().to_string();
        if previous.as_deref() == Some(normalized.as_str()) {
            repeated += 1;
            continue;
        }
        if repeated > 0 {
            output.push(format!("[linha anterior repetida {repeated}x]"));
            repeated = 0;
        }
        output.push(normalized.clone());
        previous = Some(normalized);
    }
    if repeated > 0 {
        output.push(format!("[linha anterior repetida {repeated}x]"));
    }
    (output.join("\n"), filter.to_string())
}

fn compress_vault_prose(input: &str) -> String {
    let whitespace = Regex::new(r"[ \t]{2,}").expect("regex whitespace válida");
    let blank_lines = Regex::new(r"\n{3,}").expect("regex linhas válida");
    let compact = whitespace.replace_all(input, " ");
    blank_lines.replace_all(&compact, "\n\n").trim().to_string()
}

pub fn redact(input: &str) -> String {
    let bearer = Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]+").expect("regex bearer válida");
    let keys = Regex::new(r"(?i)(api[_ -]?key[=: ]+)\S+").expect("regex api key válida");
    let redacted = bearer.replace_all(input, "Bearer [REDACTED]");
    keys.replace_all(&redacted, "$1[REDACTED]").to_string()
}

fn sha256(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_compresses_normative_content() {
        let preview = preview_compression(CompressionPreviewRequest {
            content: "MUST preserve every clause".repeat(20),
            content_kind: ContentKind::Contract,
            engine: CompressionEngine::Rtk,
            caveman_opt_in: false,
        });
        assert!(!preview.applied);
        assert_eq!(preview.result, "ineligible_content_kind");
    }

    #[test]
    fn rtk_is_idempotent_and_fail_open_on_inflation() {
        let input = "\x1b[31merror\x1b[0m\nerror\nerror\nerror\n";
        let first = preview_compression(CompressionPreviewRequest {
            content: input.to_string(),
            content_kind: ContentKind::ToolOutput,
            engine: CompressionEngine::Rtk,
            caveman_opt_in: false,
        });
        let second = preview_compression(CompressionPreviewRequest {
            content: first.output.clone(),
            content_kind: ContentKind::ToolOutput,
            engine: CompressionEngine::Rtk,
            caveman_opt_in: false,
        });
        assert!(first.output.contains("error"));
        assert_eq!(second.output, first.output);
    }

    #[test]
    fn redacts_credentials() {
        assert!(!redact("Authorization: Bearer secret-123").contains("secret-123"));
    }

    #[test]
    fn rtk_preserves_error_paths_and_counts() {
        let input = "cargo test\nCompiling demo v0.1.0\nerror[E0308]: mismatched types\n --> src/main.rs:12:4\n2 errors emitted\n";
        let preview = preview_compression(CompressionPreviewRequest {
            content: input.to_string(),
            content_kind: ContentKind::ToolOutput,
            engine: CompressionEngine::Rtk,
            caveman_opt_in: false,
        });
        assert!(preview.output.contains("error[E0308]"));
        assert!(preview.output.contains("src/main.rs:12:4"));
        assert!(preview.output.contains("2 errors emitted"));
    }
}
