use super::*;

pub(super) struct ClaudeAdapter;

impl CliAdapter for ClaudeAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        base_capabilities(
            CliOutputFormat::JsonLines,
            true,
            version_supports_contract(CliBackend::Claude, version),
            &[
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
            ],
            true,
            true,
            false,
        )
    }

    fn build_process_spec(
        &self,
        request: CliInvocationRequest,
    ) -> Result<CliProcessSpec, TerminalError> {
        let mut args = [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-mode",
            "plan",
            "--tools",
            "Read,Glob,Grep",
            "--disallowedTools",
            "mcp__*",
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(session_id) = request.resume_session_id.as_ref() {
            args.extend(["--resume".into(), session_id.clone()]);
        }
        append_model(&mut args, request.model.as_deref());
        let model_profile = request.model_profile.as_deref().or_else(|| {
            (request.thinking_level != ThinkingLevel::Default)
                .then_some(request.thinking_level.as_str())
        });
        if let Some(model_profile) = model_profile {
            args.extend(["--effort".into(), model_profile.into()]);
        }
        if let Some(schema) = request.schema {
            args.extend(["--json-schema".into(), serde_json::to_string(&schema)?]);
        }
        Ok(CliProcessSpec {
            executable: request.executable,
            args,
            environment: Vec::new(),
            stdin_payload: request.prompt.into_bytes(),
            resume_session_id: request.resume_session_id,
        })
    }

    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame {
        normalize_json_frame(
            CliBackend::Claude,
            stream,
            bytes,
            |value| match event_type(value) {
                "system" => TerminalEventKind::System,
                "result" => TerminalEventKind::Completed,
                "error" => TerminalEventKind::Error,
                "user" => TerminalEventKind::ToolResult,
                "stream_event" => {
                    let delta_type = value
                        .pointer("/event/delta/type")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if delta_type.contains("tool") {
                        TerminalEventKind::ToolUse
                    } else {
                        TerminalEventKind::Message
                    }
                }
                "assistant" => {
                    let has_tool = value
                        .pointer("/message/content")
                        .and_then(Value::as_array)
                        .is_some_and(|blocks| {
                            blocks.iter().any(|block| {
                                block.get("type").and_then(Value::as_str) == Some("tool_use")
                            })
                        });
                    if has_tool {
                        TerminalEventKind::ToolUse
                    } else {
                        TerminalEventKind::Message
                    }
                }
                _ => TerminalEventKind::Message,
            },
        )
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        Some(&["auth", "status"])
    }

    fn classify_failure(&self, _exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        classify_failure_text(stderr)
    }
}
