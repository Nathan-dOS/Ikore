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
            // Plan mode upgrades Haiku sessions to Sonnet for planning.
            // dontAsk keeps read tools available while denying prompts.
            "dontAsk",
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
        if let Some(mut schema) = request.schema {
            // Claude Code currently validates the supplied schema against a
            // registry that does not resolve draft/2020-12's meta URI. The
            // v4 Claude flavor already omits it; retaining this adapter guard
            // makes the argv contract safe for every caller.
            if let Some(object) = schema.as_object_mut() {
                object.remove("$schema");
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haiku_launch_does_not_enable_plan_mode_model_upgrade() {
        let spec = ClaudeAdapter
            .build_process_spec(CliInvocationRequest {
                executable: std::path::PathBuf::from(r"C:\Tools\claude.exe"),
                backend: CliBackend::Claude,
                model: Some("haiku".into()),
                model_profile: None,
                thinking_level: ThinkingLevel::Default,
                prompt: "Read the project".into(),
                schema_file: None,
                schema: None,
                custom_args: Vec::new(),
                resume_session_id: None,
            })
            .unwrap();
        assert!(spec.args.windows(2).any(|pair| pair == ["--model", "haiku"]));
        assert!(spec.args.windows(2).any(|pair| pair == ["--permission-mode", "dontAsk"]));
        assert!(spec.args.windows(2).any(|pair| pair == ["--tools", "Read,Glob,Grep"]));
        assert!(!spec.args.windows(2).any(|pair| pair == ["--permission-mode", "plan"]));
    }

    #[test]
    fn message_start_captures_generation_identity_and_invocation_lineage() {
        let frame = ClaudeAdapter.normalize_frame(
            "stdout",
            br#"{"type":"stream_event","parent_tool_use_id":"tool-parent","agent_id":"agent-child","event":{"type":"message_start","message":{"id":"msg-17","model":"claude-haiku-4-5-20251001"}}}"#,
        );
        assert!(frame.is_model_generation);
        assert_eq!(frame.observed_model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert_eq!(frame.message_id.as_deref(), Some("msg-17"));
        assert_eq!(frame.parent_invocation_id.as_deref(), Some("tool-parent"));
        assert_eq!(frame.agent_id.as_deref(), Some("agent-child"));
    }

    #[test]
    fn session_initialization_does_not_count_as_an_assistant_generation() {
        let frame = ClaudeAdapter.normalize_frame(
            "stdout",
            br#"{"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001"}"#,
        );
        assert!(!frame.is_model_generation);
        assert_eq!(frame.observed_model.as_deref(), Some("claude-haiku-4-5-20251001"));
    }
}
