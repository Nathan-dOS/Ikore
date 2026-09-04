use super::*;

pub(super) struct CodexAdapter;

impl CliAdapter for CodexAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        base_capabilities(
            CliOutputFormat::JsonLines,
            true,
            version_supports_contract(CliBackend::Codex, version),
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
            "-a",
            "never",
            "exec",
            "--sandbox",
            "read-only",
            "--json",
            "--skip-git-repo-check",
        ]
        .map(str::to_owned)
        .to_vec();
        append_model(&mut args, request.model.as_deref());
        let model_profile = request.model_profile.as_deref().or_else(|| {
            (request.thinking_level != ThinkingLevel::Default)
                .then_some(request.thinking_level.as_str())
        });
        if let Some(model_profile) = model_profile {
            args.extend([
                "-c".into(),
                format!("model_reasoning_effort=\"{model_profile}\""),
            ]);
        }
        if let Some(schema) = request.schema_file {
            args.extend([
                "--output-schema".into(),
                schema.to_string_lossy().into_owned(),
            ]);
        }
        if let Some(session_id) = request.resume_session_id.as_ref() {
            args.extend(["resume".into(), session_id.clone()]);
        }
        args.push("-".into());
        Ok(CliProcessSpec {
            executable: request.executable,
            args,
            environment: Vec::new(),
            stdin_payload: request.prompt.into_bytes(),
            resume_session_id: request.resume_session_id,
        })
    }

    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame {
        normalize_json_frame(CliBackend::Codex, stream, bytes, |value| {
            let kind = event_type(value);
            if kind.contains("failed") || kind.contains("error") {
                TerminalEventKind::Error
            } else if kind == "thread.started" {
                TerminalEventKind::System
            } else if kind == "turn.completed" {
                TerminalEventKind::Completed
            } else if kind.starts_with("item.") {
                match value
                    .get("item")
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "command_execution" | "mcp_tool_call" | "file_change" => {
                        if kind.ends_with("completed") {
                            TerminalEventKind::ToolResult
                        } else {
                            TerminalEventKind::ToolUse
                        }
                    }
                    _ => TerminalEventKind::Message,
                }
            } else {
                TerminalEventKind::Message
            }
        })
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        Some(&["login", "status"])
    }

    fn classify_failure(&self, _exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        classify_failure_text(stderr)
    }
}
