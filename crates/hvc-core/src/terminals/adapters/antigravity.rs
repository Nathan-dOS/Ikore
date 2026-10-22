use super::*;
use crate::terminals::CliQuotaBucket;

pub(super) struct AntigravityAdapter;

/// How many quota windows the availability snapshot keeps. `agy` 1.3.5 reports four
/// (two model groups, each with a five-hour and a weekly window).
const MAX_QUOTA_BUCKETS: usize = 8;
const MAX_QUOTA_LABEL_CHARS: usize = 48;

impl CliAdapter for AntigravityAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        base_capabilities(
            CliOutputFormat::JsonLines,
            true,
            version_supports_contract(CliBackend::Antigravity, version),
            &[
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
            ],
            true,
            true,
            true,
        )
    }

    fn build_process_spec(
        &self,
        request: CliInvocationRequest,
    ) -> Result<CliProcessSpec, TerminalError> {
        // `-p` only takes the prompt as an argument. Stream input reads it from
        // stdin instead, as one NDJSON message, and still runs a single turn
        // once stdin closes.
        let mut args = [
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(conversation_id) = request.resume_session_id.as_ref() {
            args.extend(["--conversation".into(), conversation_id.clone()]);
        }
        append_model(&mut args, request.model.as_deref());
        let effort = request.model_profile.as_deref().or_else(|| {
            (request.thinking_level != ThinkingLevel::Default)
                .then_some(request.thinking_level.as_str())
        });
        if let Some(effort) = effort {
            args.extend(["--effort".into(), effort.into()]);
        }
        if let Some(schema) = request.schema {
            // The structured object arrives as `result.structured_output`; 1.3.5
            // accepts draft 2020-12's `$schema` key, unlike Claude Code, but
            // exits 1 unless the root is `type: object`, and its `finish` tool
            // adds `toolAction`/`toolSummary` to that root (see
            // `SchemaFlavor::Antigravity`).
            args.extend(["--json-schema".into(), serde_json::to_string(&schema)?]);
        }
        let mut stdin_payload = serde_json::to_vec(&serde_json::json!({
            "event": "user",
            "message": { "role": "user", "content": request.prompt },
        }))?;
        stdin_payload.push(b'\n');
        Ok(CliProcessSpec {
            executable: request.executable,
            args,
            environment: Vec::new(),
            stdin_payload,
            resume_session_id: request.resume_session_id,
        })
    }

    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame {
        let mut frame =
            normalize_json_frame(
                CliBackend::Antigravity,
                stream,
                bytes,
                |value| match frame_event(value) {
                    "init" => TerminalEventKind::System,
                    "result" => {
                        if value.pointer("/result/status").and_then(Value::as_str)
                            == Some("SUCCESS")
                        {
                            TerminalEventKind::Completed
                        } else {
                            TerminalEventKind::Error
                        }
                    }
                    "error" => TerminalEventKind::Error,
                    "step_update" => match step_type(value) {
                        "agent_response" => TerminalEventKind::Message,
                        "tool" if step_state(value) == "DONE" => TerminalEventKind::ToolResult,
                        "tool" => TerminalEventKind::ToolUse,
                        _ => TerminalEventKind::System,
                    },
                    _ => TerminalEventKind::Message,
                },
            );
        if stream == "stderr" {
            return frame;
        }
        let Ok(value) = serde_json::from_str::<Value>(&sanitize_cli_frame_text(bytes)) else {
            return frame;
        };
        match frame_event(&value) {
            "step_update" => {
                let step = &value["step_update"];
                let kind = step_type(&value);
                frame.is_reasoning = ["think", "reason", "thought", "planner"]
                    .iter()
                    .any(|marker| kind.contains(marker));
                if kind == "agent_response" {
                    let delta = step
                        .get("text_delta")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    frame.content = sanitize_cli_frame_text(delta.as_bytes());
                    frame.is_text = !frame.content.is_empty();
                    frame.is_delta = true;
                } else if kind == "tool" {
                    let name = step
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    frame.content = sanitize_cli_frame_text(
                        if frame.kind == TerminalEventKind::ToolUse {
                            let parameters = step
                                .pointer("/tool_info/parameters")
                                .map(Value::to_string)
                                .unwrap_or_default();
                            format!("{name} {parameters}")
                        } else {
                            format!("{name} finished")
                        }
                        .trim()
                        .as_bytes(),
                    );
                    frame.is_text = false;
                    frame.is_delta = false;
                } else {
                    frame.is_text = false;
                    frame.is_delta = false;
                }
            }
            "result" => {
                let result = &value["result"];
                // The schema-checked object wins over the free text, which also
                // carries the prose the model wrote before it.
                let content = match result.get("structured_output") {
                    Some(object) if !object.is_null() => object.to_string(),
                    _ => result
                        .get("response")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                };
                frame.content = sanitize_cli_frame_text(content.as_bytes());
                frame.is_text = !frame.content.trim().is_empty();
                frame.is_delta = false;
            }
            "init" => {
                frame.is_text = false;
                frame.is_delta = false;
            }
            _ => {}
        }
        frame
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        // A read-only slash command: answered without a model turn, quota or a
        // conversation left behind, and it fails when nobody is signed in. Only
        // in the developer's own config directory - an empty `--gemini_dir` sent
        // `/usage` to the model as plain text on 1.3.5.
        Some(&["-p", "/usage", "--output-format", "json"])
    }

    fn quota_from_auth_probe(&self, stdout: &[u8]) -> Vec<CliQuotaBucket> {
        parse_usage_quota(&sanitize_terminal_text(stdout))
    }

    fn classify_failure(&self, exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        let lower = stderr.to_ascii_lowercase();
        if let Some(status) = agy_error_status(stderr) {
            match status.as_str() {
                "UNAUTHENTICATED" | "PERMISSION_DENIED" => {
                    return CliFailureKind::AuthenticationRequired;
                }
                "RESOURCE_EXHAUSTED" | "UNAVAILABLE" | "DEADLINE_EXCEEDED" => {
                    return CliFailureKind::RateLimited;
                }
                _ => {}
            }
        }
        if [
            "not logged into antigravity",
            "authentication expired",
            "please log back in",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            CliFailureKind::AuthenticationRequired
        } else if [
            "credits balance is too low",
            "out of credits",
            "quota",
            "resource_exhausted",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            CliFailureKind::RateLimited
        } else if exit_code == Some(2) || lower.contains("provided but not defined") {
            // Exit 2 is `agy`'s print-mode usage error: a flag this build lacks.
            CliFailureKind::VersionUnsupported
        } else {
            classify_failure_text(stderr)
        }
    }
}

/// `agy` names its frame type `event`, not `type`.
fn frame_event(value: &Value) -> &str {
    value
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn step_type(value: &Value) -> &str {
    value
        .pointer("/step_update/step_type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn step_state(value: &Value) -> &str {
    value
        .pointer("/step_update/state")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The canonical status of the `AGY_ERROR: {...}` line a failed headless turn
/// prints on stderr (exit code 3), under whichever key it uses.
fn agy_error_status(stderr: &str) -> Option<String> {
    stderr.lines().find_map(|line| {
        let payload = line.trim().strip_prefix("AGY_ERROR:")?.trim();
        let value = serde_json::from_str::<Value>(payload).ok()?;
        ["status", "canonical_status", "canonicalStatus", "code"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(str::to_ascii_uppercase)
    })
}

/// Reads `command.data.groups[].buckets[]` from `agy -p /usage --output-format json`.
fn parse_usage_quota(stdout: &str) -> Vec<CliQuotaBucket> {
    let Some(value) = stdout
        .lines()
        .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
    else {
        return Vec::new();
    };
    let Some(groups) = value
        .pointer("/command/data/groups")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut buckets = Vec::new();
    for group in groups {
        let group_name = group
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Models");
        for bucket in group
            .get("buckets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(fraction) = bucket.get("remaining_fraction").and_then(Value::as_f64) else {
                continue;
            };
            buckets.push(CliQuotaBucket {
                group: bounded_label(group_name),
                window: bounded_label(
                    bucket
                        .get("window")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                remaining_percent: (fraction.clamp(0.0, 1.0) * 100.0).floor() as u8,
                reset_time: bucket
                    .get("reset_time")
                    .and_then(Value::as_str)
                    .map(bounded_label),
            });
        }
    }
    buckets.sort_by_key(|bucket| bucket.remaining_percent);
    buckets.truncate(MAX_QUOTA_BUCKETS);
    buckets
}

fn bounded_label(value: &str) -> String {
    sanitize_terminal_text(value.as_bytes())
        .chars()
        .take(MAX_QUOTA_LABEL_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(prompt: &str) -> CliInvocationRequest {
        CliInvocationRequest {
            executable: std::path::PathBuf::from(r"C:\Tools\agy.exe"),
            backend: CliBackend::Antigravity,
            model: Some("gemini-3.8-flash".into()),
            model_profile: None,
            thinking_level: ThinkingLevel::Low,
            prompt: prompt.into(),
            schema_file: None,
            schema: None,
            custom_args: Vec::new(),
            resume_session_id: None,
        }
    }

    #[test]
    fn the_prompt_travels_on_stdin_as_one_stream_message_never_in_argv() {
        let prompt = "Read the project\nand \"quote\" it";
        let spec = AntigravityAdapter
            .build_process_spec(request(prompt))
            .unwrap();
        assert!(!spec.args.iter().any(|arg| arg.contains("Read the project")));
        assert!(!spec.args.iter().any(|arg| arg == "-p" || arg == "--print"));
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--input-format", "stream-json"])
        );
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--model", "gemini-3.8-flash"])
        );
        assert!(spec.args.windows(2).any(|pair| pair == ["--effort", "low"]));
        assert_eq!(spec.stdin_payload.last(), Some(&b'\n'));
        let line: Value = serde_json::from_slice(&spec.stdin_payload).unwrap();
        assert_eq!(line["event"], "user");
        assert_eq!(line["message"]["content"], prompt);
    }

    #[test]
    fn a_catalog_profile_wins_over_the_role_thinking_level() {
        let mut with_profile = request("go");
        with_profile.model_profile = Some("high".into());
        let spec = AntigravityAdapter.build_process_spec(with_profile).unwrap();
        assert!(
            spec.args
                .windows(2)
                .any(|pair| pair == ["--effort", "high"])
        );

        let mut default = request("go");
        default.thinking_level = ThinkingLevel::Default;
        let spec = AntigravityAdapter.build_process_spec(default).unwrap();
        assert!(!spec.args.iter().any(|arg| arg == "--effort"));
    }

    #[test]
    fn the_resolved_model_in_init_is_the_generation_identity() {
        let frame = AntigravityAdapter.normalize_frame(
            "stdout",
            br#"{"event":"init","conversation_id":"c-1","init":{"model":"gemini-3.8-flash","cwd":"C:\\Workspace","tools":["view_file"],"permission_mode":"request-review"}}"#,
        );
        assert!(frame.is_model_generation);
        assert_eq!(frame.observed_model.as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(frame.native_session_id.as_deref(), Some("c-1"));
        assert!(!frame.is_text);
    }

    #[test]
    fn response_steps_stream_as_deltas_and_the_result_carries_the_structured_object() {
        let delta = AntigravityAdapter.normalize_frame(
            "stdout",
            br#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"The capital"}}"#,
        );
        assert_eq!(delta.kind, TerminalEventKind::Message);
        assert!(delta.is_text && delta.is_delta);
        assert_eq!(delta.content, "The capital");

        let result = AntigravityAdapter.normalize_frame(
            "stdout",
            br#"{"event":"result","result":{"conversation_id":"c-1","status":"SUCCESS","response":"Paris.\n{\"answer\":\"Paris\"}","structured_output":{"answer":"Paris"},"json_schema":{"type":"object"}}}"#,
        );
        assert_eq!(result.kind, TerminalEventKind::Completed);
        assert!(result.is_text && !result.is_delta);
        assert_eq!(result.content, r#"{"answer":"Paris"}"#);

        let plain = AntigravityAdapter.normalize_frame(
            "stdout",
            br#"{"event":"result","result":{"conversation_id":"c-1","status":"SUCCESS","response":"PONG\n"}}"#,
        );
        assert_eq!(plain.content.trim(), "PONG");
    }

    #[test]
    fn a_failed_result_is_an_error_frame() {
        let frame = AntigravityAdapter.normalize_frame(
            "stdout",
            br#"{"event":"result","result":{"conversation_id":"c-1","status":"ERROR","response":""}}"#,
        );
        assert_eq!(frame.kind, TerminalEventKind::Error);
    }

    #[test]
    fn failures_are_classified_from_agy_error_and_known_phrases() {
        let adapter = AntigravityAdapter;
        assert_eq!(
            adapter.classify_failure(
                Some(3),
                r#"AGY_ERROR: {"status":"RESOURCE_EXHAUSTED","code":429,"retryable":true}"#
            ),
            CliFailureKind::RateLimited
        );
        assert_eq!(
            adapter.classify_failure(Some(3), r#"AGY_ERROR: {"status":"UNAUTHENTICATED"}"#),
            CliFailureKind::AuthenticationRequired
        );
        assert_eq!(
            adapter.classify_failure(Some(1), "Authentication expired. Please log back in."),
            CliFailureKind::AuthenticationRequired
        );
        assert_eq!(
            adapter.classify_failure(Some(1), "Your AI credits balance is too low to continue."),
            CliFailureKind::RateLimited
        );
        assert_eq!(
            adapter.classify_failure(Some(2), "flags provided but not defined: -foo"),
            CliFailureKind::VersionUnsupported
        );
        assert_eq!(
            adapter.classify_failure(Some(1), "error: invalid model selection"),
            CliFailureKind::ProcessFailed
        );
    }

    #[test]
    fn usage_quota_is_read_lowest_first_and_bounded() {
        let stdout = r#"{"conversation_id":"","status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","window":"weekly","remaining_fraction":0.959,"reset_time":"2026-10-16T14:01:26Z"},{"id":"gemini-5h","window":"5h","remaining_fraction":0.922,"reset_time":"2026-10-14T17:11:54Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-5h","window":"5h","remaining_fraction":0.0172,"reset_time":"2026-10-14T18:11:40Z"},{"id":"broken","window":"5h"}]}]}}}"#;
        let quota = AntigravityAdapter.quota_from_auth_probe(stdout.as_bytes());
        assert_eq!(quota.len(), 3);
        assert_eq!(quota[0].group, "Claude and GPT models");
        assert_eq!(quota[0].remaining_percent, 1);
        assert_eq!(quota[0].window, "5h");
        assert_eq!(quota[2].remaining_percent, 95);
        assert!(
            AntigravityAdapter
                .quota_from_auth_probe(b"not json")
                .is_empty()
        );
    }
}
