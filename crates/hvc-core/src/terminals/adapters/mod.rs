use serde_json::Value;

use super::{
    CliBackend, CliCapabilities, CliInvocationRequest, CliOutputFormat, CliProcessSpec,
    TerminalError, TerminalEventKind, ThinkingLevel, extract_visible_text, sanitize_terminal_text,
};

mod claude;
mod codex;
mod custom;
mod gemini;
mod opencode;

pub(super) struct NormalizedCliFrame {
    pub kind: TerminalEventKind,
    pub content: String,
    pub native_session_id: Option<String>,
    pub observed_model: Option<String>,
    pub is_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CliFailureKind {
    AuthenticationRequired,
    VersionUnsupported,
    RateLimited,
    ProcessFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CliVersionCompatibility {
    Supported,
    Unsupported,
    UnknownNewer,
}

#[derive(Debug, Clone, Copy)]
struct CliCompatibilityRule {
    minimum: (u64, u64, u64),
    maximum_exclusive: (u64, u64, u64),
}

impl CliFailureKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "cli_auth_required",
            Self::VersionUnsupported => "cli_version_unsupported",
            Self::RateLimited => "cli_rate_limited",
            Self::ProcessFailed => "non_zero_exit",
        }
    }
}

pub(super) trait CliAdapter: Sync {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities;
    fn build_process_spec(
        &self,
        request: CliInvocationRequest,
    ) -> Result<CliProcessSpec, TerminalError>;
    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame;
    fn auth_probe_args(&self) -> Option<&'static [&'static str]>;
    fn classify_failure(&self, exit_code: Option<i32>, stderr: &str) -> CliFailureKind;
}

static CODEX: codex::CodexAdapter = codex::CodexAdapter;
static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;
static GEMINI: gemini::GeminiAdapter = gemini::GeminiAdapter;
static OPENCODE: opencode::OpenCodeAdapter = opencode::OpenCodeAdapter;
static CUSTOM: custom::CustomAdapter = custom::CustomAdapter;

fn adapter(backend: CliBackend) -> &'static dyn CliAdapter {
    match backend {
        CliBackend::Codex => &CODEX,
        CliBackend::Claude => &CLAUDE,
        CliBackend::Gemini => &GEMINI,
        CliBackend::Opencode => &OPENCODE,
        CliBackend::Custom => &CUSTOM,
    }
}

pub(super) fn capabilities(backend: CliBackend, version: Option<&str>) -> CliCapabilities {
    adapter(backend).capabilities(version)
}

pub(super) fn auth_probe_args(backend: CliBackend) -> Option<&'static [&'static str]> {
    adapter(backend).auth_probe_args()
}

pub(super) fn build_process_spec(
    request: CliInvocationRequest,
) -> Result<CliProcessSpec, TerminalError> {
    if !request.executable.is_absolute() {
        return Err(TerminalError::Validation(
            "o executável CLI deve ter caminho absoluto".into(),
        ));
    }
    validate_resume_session(request.resume_session_id.as_deref())?;
    adapter(request.backend).build_process_spec(request)
}

pub(super) fn normalize_frame(
    backend: CliBackend,
    stream: &str,
    bytes: &[u8],
) -> NormalizedCliFrame {
    adapter(backend).normalize_frame(stream, bytes)
}

pub(super) fn classify_failure(
    backend: CliBackend,
    exit_code: Option<i32>,
    stderr: &str,
) -> CliFailureKind {
    adapter(backend).classify_failure(exit_code, stderr)
}

fn validate_resume_session(session_id: Option<&str>) -> Result<(), TerminalError> {
    let Some(session_id) = session_id else {
        return Ok(());
    };
    if session_id.is_empty()
        || session_id.len() > 256
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return Err(TerminalError::Validation(
            "identificador de sessão CLI inválido".into(),
        ));
    }
    Ok(())
}

fn base_capabilities(
    output_format: CliOutputFormat,
    structured_output: bool,
    native_resume: bool,
    thinking_levels: &[ThinkingLevel],
    auth_probe: bool,
    read_only: bool,
    model_catalog: bool,
) -> CliCapabilities {
    CliCapabilities {
        output_format,
        structured_output,
        native_resume,
        thinking_levels: thinking_levels.to_vec(),
        auth_probe,
        read_only,
        model_catalog,
    }
}

pub(super) fn version_compatibility(
    backend: CliBackend,
    version: Option<&str>,
) -> CliVersionCompatibility {
    let Some(version) = version.and_then(parse_semantic_version) else {
        return CliVersionCompatibility::Unsupported;
    };
    let rule = compatibility_rule(backend);
    if version < rule.minimum {
        CliVersionCompatibility::Unsupported
    } else if version >= rule.maximum_exclusive {
        CliVersionCompatibility::UnknownNewer
    } else {
        CliVersionCompatibility::Supported
    }
}

fn version_supports_contract(backend: CliBackend, version: Option<&str>) -> bool {
    version_compatibility(backend, version) == CliVersionCompatibility::Supported
}

fn compatibility_rule(backend: CliBackend) -> CliCompatibilityRule {
    match backend {
        CliBackend::Codex => CliCompatibilityRule {
            minimum: (0, 145, 0),
            maximum_exclusive: (0, 154, 0),
        },
        CliBackend::Claude => CliCompatibilityRule {
            minimum: (2, 1, 0),
            maximum_exclusive: (2, 2, 0),
        },
        CliBackend::Gemini => CliCompatibilityRule {
            minimum: (0, 55, 0),
            maximum_exclusive: (0, 60, 0),
        },
        CliBackend::Opencode => CliCompatibilityRule {
            minimum: (1, 18, 0),
            maximum_exclusive: (1, 19, 0),
        },
        CliBackend::Custom => CliCompatibilityRule {
            minimum: (0, 0, 0),
            maximum_exclusive: (u64::MAX, u64::MAX, u64::MAX),
        },
    }
}

fn parse_semantic_version(value: &str) -> Option<(u64, u64, u64)> {
    let bytes = value.as_bytes();
    for start in 0..bytes.len() {
        if !bytes[start].is_ascii_digit()
            || start
                .checked_sub(1)
                .is_some_and(|previous| bytes[previous].is_ascii_digit())
        {
            continue;
        }
        let core = value[start..]
            .split(|character: char| !(character.is_ascii_digit() || character == '.'))
            .next()
            .unwrap_or_default();
        let mut parts = core.split('.');
        let Some(major) = parts.next().and_then(|part| part.parse().ok()) else {
            continue;
        };
        let Some(minor) = parts.next().and_then(|part| part.parse().ok()) else {
            continue;
        };
        let Some(patch) = parts.next().and_then(|part| part.parse().ok()) else {
            continue;
        };
        if parts.next().is_none() {
            return Some((major, minor, patch));
        }
    }
    None
}

fn append_model(args: &mut Vec<String>, model: Option<&str>) {
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        args.extend(["--model".into(), model.trim().into()]);
    }
}

fn normalize_json_frame(
    backend: CliBackend,
    stream: &str,
    bytes: &[u8],
    classify: impl FnOnce(&Value) -> TerminalEventKind,
) -> NormalizedCliFrame {
    let sanitized = sanitize_terminal_text(bytes);
    if stream == "stderr" {
        return NormalizedCliFrame {
            kind: TerminalEventKind::Stderr,
            content: sanitized,
            native_session_id: None,
            observed_model: None,
            is_text: false,
        };
    }
    let Ok(value) = serde_json::from_str::<Value>(&sanitized) else {
        return NormalizedCliFrame {
            kind: TerminalEventKind::Stdout,
            content: sanitized,
            native_session_id: None,
            observed_model: None,
            is_text: true,
        };
    };
    let kind = classify(&value);
    let visible = extract_visible_text(&value);
    let is_text = visible.is_some();
    let content = visible.unwrap_or_else(|| format!("{}: {sanitized}", backend.label()));
    NormalizedCliFrame {
        kind,
        content: sanitize_terminal_text(content.as_bytes()),
        native_session_id: find_session_id(&value),
        observed_model: find_model_id(&value),
        is_text,
    }
}

fn event_type(value: &Value) -> &str {
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn find_session_id(value: &Value) -> Option<String> {
    if let Value::Object(values) = value {
        for key in [
            "session_id",
            "sessionId",
            "sessionID",
            "thread_id",
            "threadId",
        ] {
            if let Some(session_id) = values
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 256)
            {
                return Some(session_id.to_string());
            }
        }
        return values.values().find_map(find_session_id);
    }
    if let Value::Array(values) = value {
        return values.iter().find_map(find_session_id);
    }
    None
}

fn find_model_id(value: &Value) -> Option<String> {
    if let Value::Object(values) = value {
        for key in ["model", "model_id", "modelId", "modelID"] {
            if let Some(model) = values
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= super::MAX_MODEL_CHARS)
            {
                return Some(model.to_string());
            }
        }
        return values.values().find_map(find_model_id);
    }
    if let Value::Array(values) = value {
        return values.iter().find_map(find_model_id);
    }
    None
}

fn classify_failure_text(stderr: &str) -> CliFailureKind {
    let stderr = stderr.to_ascii_lowercase();
    if [
        "not logged in",
        "login required",
        "authentication required",
        "unauthorized",
        "invalid api key",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
    {
        CliFailureKind::AuthenticationRequired
    } else if [
        "unknown option",
        "unknown argument",
        "unrecognized option",
        "unsupported flag",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
    {
        CliFailureKind::VersionUnsupported
    } else if ["rate limit", "too many requests", "resource exhausted"]
        .iter()
        .any(|needle| stderr.contains(needle))
    {
        CliFailureKind::RateLimited
    } else {
        CliFailureKind::ProcessFailed
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../hvc-test-support/fixtures/cli"
    );

    fn fixture_lines(backend: &str, name: &str) -> Vec<String> {
        std::fs::read_to_string(Path::new(FIXTURES).join(backend).join("current").join(name))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn versioned_success_fixtures_expose_session_and_completion() {
        for (backend, name) in [
            (CliBackend::Codex, "codex"),
            (CliBackend::Claude, "claude"),
            (CliBackend::Gemini, "gemini"),
            (CliBackend::Opencode, "opencode"),
        ] {
            let lines = fixture_lines(name, "success.jsonl");
            let normalized = lines
                .iter()
                .map(|line| normalize_frame(backend, "stdout", line.as_bytes()))
                .collect::<Vec<_>>();
            assert!(
                normalized
                    .iter()
                    .any(|event| event.native_session_id.is_some()),
                "{name} must expose a native session id"
            );
            assert!(
                normalized
                    .iter()
                    .any(|event| event.kind == TerminalEventKind::Completed),
                "{name} must expose a completion event"
            );
        }
    }

    #[test]
    fn current_fixtures_capture_the_effective_model_separately_from_the_request() {
        for (backend, name, expected_model) in [
            (CliBackend::Codex, "codex", "gpt-5.4"),
            (CliBackend::Claude, "claude", "claude-sonnet-4-6"),
            (CliBackend::Gemini, "gemini", "gemini-3-pro"),
            (CliBackend::Opencode, "opencode", "openai/gpt-5.4"),
        ] {
            let observed = fixture_lines(name, "success.jsonl")
                .iter()
                .find_map(|line| {
                    normalize_frame(backend, "stdout", line.as_bytes()).observed_model
                });
            assert_eq!(observed.as_deref(), Some(expected_model), "{name}");
        }
    }

    #[test]
    fn semantic_compatibility_is_backend_specific_and_fails_closed() {
        for (backend, version) in [
            (CliBackend::Codex, "codex-cli 0.153.0-alpha.5"),
            (CliBackend::Claude, "2.1.81 (Claude Code)"),
            (CliBackend::Gemini, "v0.55.1"),
            (CliBackend::Gemini, "0.59.5"),
            (CliBackend::Opencode, "opencode 1.18.17"),
        ] {
            assert_eq!(
                version_compatibility(backend, Some(version)),
                CliVersionCompatibility::Supported,
                "expected {backend:?} {version} to be supported"
            );
        }
        assert_eq!(
            version_compatibility(CliBackend::Codex, Some("not a version")),
            CliVersionCompatibility::Unsupported
        );
        assert_eq!(
            version_compatibility(CliBackend::Gemini, Some("0.54.9")),
            CliVersionCompatibility::Unsupported
        );
        assert_eq!(
            version_compatibility(CliBackend::Gemini, Some("0.60.0")),
            CliVersionCompatibility::UnknownNewer
        );
        assert_eq!(
            version_compatibility(CliBackend::Opencode, Some("1.19.0")),
            CliVersionCompatibility::UnknownNewer
        );
        assert!(!capabilities(CliBackend::Claude, Some("2.2.0")).native_resume);
    }

    #[test]
    fn versioned_tool_fixtures_map_use_and_result_for_every_backend() {
        for (backend, name) in [
            (CliBackend::Codex, "codex"),
            (CliBackend::Claude, "claude"),
            (CliBackend::Gemini, "gemini"),
            (CliBackend::Opencode, "opencode"),
        ] {
            let kinds = fixture_lines(name, "tools.jsonl")
                .iter()
                .map(|line| normalize_frame(backend, "stdout", line.as_bytes()).kind)
                .collect::<Vec<_>>();
            assert_eq!(
                kinds,
                vec![TerminalEventKind::ToolUse, TerminalEventKind::ToolResult],
                "unexpected tool mapping for {name}"
            );
        }
    }

    #[test]
    fn resume_builders_use_explicit_session_ids_and_stdin() {
        for (backend, resume_flag) in [
            (CliBackend::Codex, "resume"),
            (CliBackend::Claude, "--resume"),
            (CliBackend::Gemini, "--resume"),
            (CliBackend::Opencode, "--session"),
        ] {
            let spec = build_process_spec(CliInvocationRequest {
                executable: PathBuf::from(r"C:\Tools\agent.exe"),
                backend,
                model: None,
                model_profile: None,
                thinking_level: ThinkingLevel::Default,
                prompt: "follow-up".into(),
                schema_file: None,
                schema: None,
                custom_args: Vec::new(),
                resume_session_id: Some("session-fixture-1".into()),
            })
            .unwrap();
            assert!(spec.args.iter().any(|arg| arg == resume_flag));
            assert!(spec.args.iter().any(|arg| arg == "session-fixture-1"));
            assert_eq!(spec.stdin_payload, b"follow-up");
            assert!(!spec.args.iter().any(|arg| arg == "--last"));
            assert!(!spec.args.iter().any(|arg| arg == "--continue"));
        }
    }

    #[test]
    fn unknown_and_hostile_fixtures_remain_observable_but_sanitized() {
        for (backend, name) in [
            (CliBackend::Codex, "codex"),
            (CliBackend::Claude, "claude"),
            (CliBackend::Gemini, "gemini"),
            (CliBackend::Opencode, "opencode"),
        ] {
            let unknown = fixture_lines(name, "unknown.jsonl").remove(0);
            let event = normalize_frame(backend, "stdout", unknown.as_bytes());
            assert!(event.content.contains("preserve this unknown event"));

            let hostile = std::fs::read(
                Path::new(FIXTURES)
                    .join(name)
                    .join("current")
                    .join("hostile.txt"),
            )
            .unwrap();
            let sanitized = sanitize_terminal_text(&hostile);
            assert!(!sanitized.contains('\u{1b}'));
            assert!(!sanitized.contains('\0'));
        }
    }

    #[test]
    fn fixture_corpus_contains_no_real_credentials_or_user_paths() {
        for entry in walkdir::WalkDir::new(FIXTURES) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let bytes = std::fs::read(entry.path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains("sk-proj-"),
                "unsafe fixture: {:?}",
                entry.path()
            );
            assert!(
                !text.contains(r"C:\Users\"),
                "unsafe fixture: {:?}",
                entry.path()
            );
            assert!(!text.contains("-----BEGIN PRIVATE KEY-----"));
        }
    }
}
