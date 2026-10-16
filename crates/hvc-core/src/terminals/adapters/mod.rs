use serde_json::Value;

use super::{
    CliBackend, CliCapabilities, CliInvocationRequest, CliOutputFormat, CliProcessSpec,
    CliQuotaBucket, TerminalError, TerminalEventKind, ThinkingLevel, extract_visible_text,
    sanitize_terminal_text,
};

mod antigravity;
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
    pub message_id: Option<String>,
    pub parent_invocation_id: Option<String>,
    pub agent_id: Option<String>,
    pub is_model_generation: bool,
    pub is_text: bool,
    /// True when `content` is an incremental fragment of the assistant's
    /// message (streamed token-by-token) rather than the complete text -
    /// callers must append it to prior fragments instead of replacing them.
    pub is_delta: bool,
    /// The provider's own stop reason for this turn, when it reported one.
    /// Discarding it is why an output cut off by the model's token budget
    /// surfaced as "produced no final content" and was retried identically.
    pub finish_reason: Option<String>,
    /// True when the frame says the model has started thinking. Nothing else
    /// arrives until it finishes, so the watchdog must widen its silence window
    /// rather than read the quiet as a stuck process.
    pub is_reasoning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CliFailureKind {
    AuthenticationRequired,
    VersionUnsupported,
    RateLimited,
    ProcessFailed,
}

/// Only the minimum is a gate. A version above the last one this adapter was validated against
/// is `UnknownNewer`: it runs with the full contract and merely carries an advisory diagnostic,
/// so a CLI auto-update never blocks a pulsation while the ceiling waits to be bumped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CliVersionCompatibility {
    Supported,
    Unsupported,
    UnknownNewer,
}

#[derive(Debug, Clone, Copy)]
struct CliCompatibilityRule {
    minimum: (u64, u64, u64),
    /// First version not yet validated; advisory, never blocking.
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
    /// Usage allowance the auth probe printed on its way, for CLIs billed by quota.
    fn quota_from_auth_probe(&self, _stdout: &[u8]) -> Vec<CliQuotaBucket> {
        Vec::new()
    }
    fn classify_failure(&self, exit_code: Option<i32>, stderr: &str) -> CliFailureKind;
}

static CODEX: codex::CodexAdapter = codex::CodexAdapter;
static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;
static GEMINI: gemini::GeminiAdapter = gemini::GeminiAdapter;
static OPENCODE: opencode::OpenCodeAdapter = opencode::OpenCodeAdapter;
static ANTIGRAVITY: antigravity::AntigravityAdapter = antigravity::AntigravityAdapter;
static CUSTOM: custom::CustomAdapter = custom::CustomAdapter;

fn adapter(backend: CliBackend) -> &'static dyn CliAdapter {
    match backend {
        CliBackend::Codex => &CODEX,
        CliBackend::Claude => &CLAUDE,
        CliBackend::Gemini => &GEMINI,
        CliBackend::Opencode => &OPENCODE,
        CliBackend::Antigravity => &ANTIGRAVITY,
        CliBackend::Custom => &CUSTOM,
    }
}

pub(super) fn capabilities(backend: CliBackend, version: Option<&str>) -> CliCapabilities {
    adapter(backend).capabilities(version)
}

pub(super) fn auth_probe_args(backend: CliBackend) -> Option<&'static [&'static str]> {
    adapter(backend).auth_probe_args()
}

pub(super) fn quota_from_auth_probe(backend: CliBackend, stdout: &[u8]) -> Vec<CliQuotaBucket> {
    adapter(backend).quota_from_auth_probe(stdout)
}

pub(super) fn build_process_spec(
    request: CliInvocationRequest,
) -> Result<CliProcessSpec, TerminalError> {
    if !request.executable.is_absolute() {
        return Err(TerminalError::Validation(
            "the CLI executable needs an absolute path".into(),
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
            "invalid CLI session identifier".into(),
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

/// Whether the adapter's full contract (structured output, native resume) applies. Newer
/// versions keep it: the CLIs have only added to their stream-json contracts so far, and losing
/// resume on every auto-update cost more than the rare regression it guarded against.
fn version_supports_contract(backend: CliBackend, version: Option<&str>) -> bool {
    matches!(
        version_compatibility(backend, version),
        CliVersionCompatibility::Supported | CliVersionCompatibility::UnknownNewer
    )
}

/// The validated `[minimum, maximum_exclusive)` version range for a backend, formatted as
/// `major.minor.patch` strings for display in diagnostics.
pub(super) fn compatibility_range(backend: CliBackend) -> (String, String) {
    let rule = compatibility_rule(backend);
    (
        format_version(rule.minimum),
        format_version(rule.maximum_exclusive),
    )
}

fn format_version(version: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", version.0, version.1, version.2)
}

fn compatibility_rule(backend: CliBackend) -> CliCompatibilityRule {
    match backend {
        CliBackend::Codex => CliCompatibilityRule {
            minimum: (0, 145, 0),
            maximum_exclusive: (0, 155, 0),
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
        CliBackend::Antigravity => CliCompatibilityRule {
            minimum: (1, 3, 0),
            maximum_exclusive: (1, 4, 0),
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
            message_id: None,
            parent_invocation_id: None,
            agent_id: None,
            is_model_generation: false,
            is_text: false,
            is_delta: false,
            finish_reason: None,
            is_reasoning: false,
        };
    }
    let Ok(value) = serde_json::from_str::<Value>(&sanitized) else {
        return NormalizedCliFrame {
            kind: TerminalEventKind::Stdout,
            content: sanitized,
            native_session_id: None,
            observed_model: None,
            message_id: None,
            parent_invocation_id: None,
            agent_id: None,
            is_model_generation: false,
            is_text: true,
            is_delta: false,
            finish_reason: None,
            is_reasoning: false,
        };
    };
    let kind = classify(&value);
    let is_model_generation = is_model_generation(backend, &value, kind);
    let visible = extract_visible_text(&value);
    let is_text = visible.is_some();
    let content = visible.unwrap_or_else(|| format!("{}: {sanitized}", backend.label()));
    // Covers both known incremental-streaming shapes: Gemini's explicit
    // `"delta":true` marker and Claude's `stream_event`/content-block-delta
    // frames (emitted because the adapter passes `--include-partial-messages`).
    let is_delta = value.get("delta").and_then(Value::as_bool) == Some(true)
        || event_type(&value) == "stream_event";
    NormalizedCliFrame {
        kind,
        content: sanitize_terminal_text(content.as_bytes()),
        native_session_id: find_session_id(&value),
        observed_model: find_model_id(&value),
        message_id: find_message_id(&value),
        parent_invocation_id: find_string_for_keys(
            &value,
            &["parent_tool_use_id", "parentInvocationId"],
        ),
        agent_id: find_string_for_keys(&value, &["agent_id", "agentId"]),
        is_model_generation,
        is_text,
        is_delta,
        finish_reason: find_finish_reason(&value),
        is_reasoning: is_reasoning_frame(&value),
    }
}

/// True when the frame announces a reasoning turn rather than carrying output.
/// OpenCode emits `step_start` and then goes silent for the whole thinking
/// phase; Claude and Gemini name theirs explicitly.
fn is_reasoning_frame(value: &Value) -> bool {
    let kind = event_type(value).to_ascii_lowercase();
    kind == "step_start"
        || kind.contains("thinking")
        || kind.contains("reasoning")
        || kind.contains("thought")
}

/// The provider's stop reason for a turn, under whichever key that backend
/// uses. OpenCode reports `part.reason`, Claude `stop_reason`, Gemini
/// `finishReason`, Codex `finish_reason`.
fn find_finish_reason(value: &Value) -> Option<String> {
    if let Value::Object(values) = value {
        for key in [
            "reason",
            "stop_reason",
            "stopReason",
            "finish_reason",
            "finishReason",
        ] {
            if let Some(reason) = values
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 64)
            {
                return Some(reason.to_string());
            }
        }
        return values.values().find_map(find_finish_reason);
    }
    if let Value::Array(values) = value {
        return values.iter().find_map(find_finish_reason);
    }
    None
}

/// True when a stop reason means the model ran out of output budget. The
/// distinction matters because the remedy is never a plain retry: the same
/// prompt against the same budget truncates again, identically.
pub(super) fn is_output_truncation(reason: &str) -> bool {
    matches!(
        reason.trim().to_ascii_lowercase().as_str(),
        "length" | "max_tokens" | "max_output_tokens" | "output_limit" | "token_limit"
    )
}

fn event_type(value: &Value) -> &str {
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn is_model_generation(backend: CliBackend, value: &Value, kind: TerminalEventKind) -> bool {
    match backend {
        CliBackend::Claude => {
            event_type(value) == "assistant"
                || value.pointer("/event/type").and_then(Value::as_str) == Some("message_start")
        }
        CliBackend::Codex => {
            let event = event_type(value);
            event.starts_with("item.")
                && value.pointer("/item/type").and_then(Value::as_str) == Some("agent_message")
                && (event.ends_with("added") || event.ends_with("completed"))
        }
        // `agy` names the model only in `init`: the one it resolved from
        // `--model`, failing the run when it cannot. No later frame repeats it.
        CliBackend::Antigravity => {
            value.get("event").and_then(Value::as_str) == Some("init")
                && value
                    .pointer("/init/model")
                    .and_then(Value::as_str)
                    .is_some()
        }
        CliBackend::Gemini | CliBackend::Opencode | CliBackend::Custom => {
            kind == TerminalEventKind::Message && find_model_id(value).is_some()
        }
    }
}

fn find_message_id(value: &Value) -> Option<String> {
    [
        "/event/message/id",
        "/message/id",
        "/item/id",
        "/message_id",
    ]
    .into_iter()
    .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .map(str::to_owned)
}

fn find_string_for_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(value) = object.get(*key).and_then(Value::as_str) {
                    return Some(value.to_owned());
                }
            }
            object
                .values()
                .find_map(|value| find_string_for_keys(value, keys))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_string_for_keys(value, keys)),
        _ => None,
    }
}

fn find_session_id(value: &Value) -> Option<String> {
    if let Value::Object(values) = value {
        for key in [
            "session_id",
            "sessionId",
            "sessionID",
            "thread_id",
            "threadId",
            "conversation_id",
            "conversationId",
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
    } else if [
        "rate limit",
        "too many requests",
        "resource exhausted",
        "503",
        "service unavailable",
        "overloaded",
        "model is overloaded",
    ]
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

    #[test]
    fn codex_public_item_event_needs_turn_context_model_evidence() {
        let frame = normalize_frame(
            CliBackend::Codex,
            "stdout",
            br#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"ok"}}"#,
        );
        assert!(frame.is_model_generation);
        assert_eq!(frame.message_id.as_deref(), Some("item_0"));
        assert_eq!(frame.observed_model, None);
    }

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
            (CliBackend::Antigravity, "antigravity"),
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
            (
                CliBackend::Antigravity,
                "antigravity",
                "gemini-3.8-flash-low",
            ),
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
            (CliBackend::Antigravity, "1.3.5"),
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
        assert_eq!(
            version_compatibility(CliBackend::Antigravity, Some("1.2.15")),
            CliVersionCompatibility::Unsupported
        );
        assert_eq!(
            version_compatibility(CliBackend::Antigravity, Some("1.4.0")),
            CliVersionCompatibility::UnknownNewer
        );
        assert_eq!(
            version_compatibility(CliBackend::Codex, Some("0.154.0")),
            CliVersionCompatibility::Supported,
            "0.154.0 is installed and working; the ceiling must cover it"
        );
        assert_eq!(
            version_compatibility(CliBackend::Codex, Some("0.155.0")),
            CliVersionCompatibility::UnknownNewer
        );
        // Above the validated ceiling the contract stays on; below the floor it is off.
        assert!(capabilities(CliBackend::Claude, Some("2.2.0")).native_resume);
        assert!(capabilities(CliBackend::Gemini, Some("0.60.0")).native_resume);
        assert!(!capabilities(CliBackend::Gemini, Some("0.54.9")).native_resume);
    }

    #[test]
    fn unknown_newer_diagnostic_reports_the_validated_range() {
        let (minimum, maximum_exclusive) = compatibility_range(CliBackend::Codex);
        assert_eq!(minimum, "0.145.0");
        assert_eq!(maximum_exclusive, "0.155.0");
    }

    #[test]
    fn versioned_tool_fixtures_map_use_and_result_for_every_backend() {
        for (backend, name) in [
            (CliBackend::Codex, "codex"),
            (CliBackend::Claude, "claude"),
            (CliBackend::Gemini, "gemini"),
            (CliBackend::Opencode, "opencode"),
            (CliBackend::Antigravity, "antigravity"),
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
            (CliBackend::Antigravity, "--conversation"),
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
            if backend == CliBackend::Antigravity {
                // `agy` reads its prompt as one NDJSON stream message.
                let message: Value = serde_json::from_slice(&spec.stdin_payload).unwrap();
                assert_eq!(message["message"]["content"], "follow-up");
            } else {
                assert_eq!(spec.stdin_payload, b"follow-up");
            }
            assert!(!spec.args.iter().any(|arg| arg == "--last"));
            assert!(!spec.args.iter().any(|arg| arg == "--continue"));
        }
    }

    #[test]
    fn claude_json_schema_omits_only_the_unsupported_meta_reference() {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "unit-response",
            "type": "object",
            "properties": { "outcome": { "type": "string" } }
        });
        let spec = build_process_spec(CliInvocationRequest {
            executable: PathBuf::from(r"C:\Tools\claude.exe"),
            backend: CliBackend::Claude,
            model: None,
            model_profile: None,
            thinking_level: ThinkingLevel::Default,
            prompt: "reply".into(),
            schema_file: None,
            schema: Some(schema),
            custom_args: Vec::new(),
            resume_session_id: None,
        })
        .unwrap();
        let index = spec
            .args
            .iter()
            .position(|arg| arg == "--json-schema")
            .unwrap();
        let delivered: Value = serde_json::from_str(&spec.args[index + 1]).unwrap();
        assert!(delivered.get("$schema").is_none());
        assert_eq!(delivered["title"], "unit-response");
        assert_eq!(delivered["properties"]["outcome"]["type"], "string");
    }

    #[test]
    fn unknown_and_hostile_fixtures_remain_observable_but_sanitized() {
        for (backend, name) in [
            (CliBackend::Codex, "codex"),
            (CliBackend::Claude, "claude"),
            (CliBackend::Gemini, "gemini"),
            (CliBackend::Opencode, "opencode"),
            (CliBackend::Antigravity, "antigravity"),
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

    #[test]
    fn a_step_finished_by_the_token_budget_reports_its_stop_reason() {
        // The Integrator lost three full generations to this frame. The host
        // read it as "completed", saw empty or unparseable text, and asked a
        // human whether to retry the CLI - never saying the model had been cut
        // off, and never learning that an identical retry cannot help.
        let frame = normalize_frame(
            CliBackend::Opencode,
            "stdout",
            br#"{"type":"step_finish","part":{"id":"prt_1","type":"step-finish","reason":"length","tokens":{"total":32269,"input":24269,"output":0,"reasoning":8877}}}"#,
        );
        assert_eq!(frame.finish_reason.as_deref(), Some("length"));
        assert!(is_output_truncation("length"));

        // A turn that simply ended is not a truncation.
        let finished = normalize_frame(
            CliBackend::Opencode,
            "stdout",
            br#"{"type":"step_finish","part":{"id":"prt_2","reason":"stop"}}"#,
        );
        assert_eq!(finished.finish_reason.as_deref(), Some("stop"));
        assert!(!is_output_truncation("stop"));

        // Each backend spells the same condition differently.
        for (backend, bytes, expected) in [
            (
                CliBackend::Claude,
                &br#"{"type":"result","stop_reason":"max_tokens"}"#[..],
                "max_tokens",
            ),
            (
                CliBackend::Gemini,
                &br#"{"type":"result","candidates":[{"finishReason":"MAX_TOKENS"}]}"#[..],
                "MAX_TOKENS",
            ),
        ] {
            let frame = normalize_frame(backend, "stdout", bytes);
            assert_eq!(
                frame.finish_reason.as_deref(),
                Some(expected),
                "{backend:?}"
            );
            assert!(is_output_truncation(expected), "{backend:?}");
        }
    }

    #[test]
    fn antigravity_fixtures_yield_the_answer_the_host_parses() {
        // The last text frame becomes the run's raw output (see the stream
        // writer): deltas append, a complete frame replaces them.
        let final_text = |name: &str| {
            fixture_lines("antigravity", name)
                .iter()
                .map(|line| normalize_frame(CliBackend::Antigravity, "stdout", line.as_bytes()))
                .rfind(|frame| frame.is_text)
                .map(|frame| frame.content)
                .unwrap()
        };
        assert_eq!(final_text("success.jsonl").trim(), "PONG");
        assert_eq!(final_text("resume.jsonl").trim(), "PONG");
        let structured: Value = serde_json::from_str(&final_text("structured.jsonl")).unwrap();
        assert_eq!(structured["answer"], "The capital of France is Paris.");

        // A headless turn refused a tool: the run still completes, empty.
        let denied = fixture_lines("antigravity", "denied.jsonl")
            .iter()
            .map(|line| normalize_frame(CliBackend::Antigravity, "stdout", line.as_bytes()))
            .collect::<Vec<_>>();
        assert!(
            denied
                .iter()
                .any(|frame| frame.kind == TerminalEventKind::Completed)
        );
        assert!(
            denied
                .iter()
                .any(|frame| frame.kind == TerminalEventKind::ToolUse)
        );
    }

    #[test]
    fn antigravity_error_fixtures_classify_as_failures() {
        for line in fixture_lines("antigravity", "errors.jsonl") {
            let frame = normalize_frame(CliBackend::Antigravity, "stdout", line.as_bytes());
            assert_eq!(frame.kind, TerminalEventKind::Error);
        }
        let stderr = fixture_lines("antigravity", "stderr-nonzero.txt").join("\n");
        assert_eq!(
            classify_failure(CliBackend::Antigravity, Some(1), &stderr),
            CliFailureKind::ProcessFailed
        );
    }

    #[test]
    fn stderr_and_unparseable_stdout_report_no_stop_reason() {
        let stderr = normalize_frame(CliBackend::Gemini, "stderr", b"boom");
        assert_eq!(stderr.finish_reason, None);
        let text = normalize_frame(CliBackend::Gemini, "stdout", b"not json");
        assert_eq!(text.finish_reason, None);
    }
}
