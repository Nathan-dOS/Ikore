use super::*;

pub(super) struct GeminiAdapter;

impl CliAdapter for GeminiAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        base_capabilities(
            CliOutputFormat::JsonLines,
            false,
            version_supports_contract(CliBackend::Gemini, version),
            &[],
            false,
            true,
            false,
        )
    }

    fn build_process_spec(
        &self,
        request: CliInvocationRequest,
    ) -> Result<CliProcessSpec, TerminalError> {
        if request.model_profile.is_some() || request.thinking_level != ThinkingLevel::Default {
            return Err(TerminalError::Validation(
                "Gemini CLI offers no stable thinking control".into(),
            ));
        }
        let mut args = ["--output-format", "stream-json", "--approval-mode", "plan"]
            .map(str::to_owned)
            .to_vec();
        if let Some(session_id) = request.resume_session_id.as_ref() {
            args.extend(["--resume".into(), session_id.clone()]);
        }
        append_model(&mut args, request.model.as_deref());
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
            CliBackend::Gemini,
            stream,
            bytes,
            |value| match event_type(value) {
                "init" => TerminalEventKind::System,
                "tool_use" | "tool_call" => TerminalEventKind::ToolUse,
                "tool_result" => TerminalEventKind::ToolResult,
                "error" => TerminalEventKind::Error,
                "result" | "stats" => TerminalEventKind::Completed,
                _ => TerminalEventKind::Message,
            },
        )
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        None
    }

    fn classify_failure(&self, exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        if exit_code == Some(41) {
            CliFailureKind::AuthenticationRequired
        } else {
            // Gemini CLI exits non-zero when 503 retries are exhausted; the
            // standard phrases may appear in stderr. Check here before the
            // generic text classifier so this is never mis-classified.
            let lower = stderr.to_ascii_lowercase();
            if lower.contains("503") || lower.contains("service unavailable") {
                CliFailureKind::RateLimited
            } else {
                classify_failure_text(stderr)
            }
        }
    }
}
