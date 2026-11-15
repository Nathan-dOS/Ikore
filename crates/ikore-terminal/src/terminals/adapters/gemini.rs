use super::{
    ActionFrame, ActionStatus, CliAdapter, CliBackend, CliCapabilities, CliFailureKind,
    CliInvocationRequest, CliOutputFormat, CliProcessSpec, NormalizedCliFrame, TerminalError,
    TerminalEventKind, ThinkingLevel, Value, append_model, base_capabilities,
    classify_failure_text, event_type, extract_visible_text, normalize_json_frame_with,
    reported_status, sanitize_cli_frame_text, version_supports_contract,
};

pub(super) struct GeminiAdapter;

impl CliAdapter for GeminiAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        CliCapabilities {
            action_frames: true,
            ..base_capabilities(
                CliOutputFormat::JsonLines,
                false,
                version_supports_contract(CliBackend::Gemini, version),
                &[],
                false,
                true,
                false,
            )
        }
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
        normalize_json_frame_with(
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
            refine_frame,
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

/// `tool_use` names the tool, its `tool_id` and `parameters`; `tool_result`
/// answers that id with a `status` and its `output` or `error`. The 0.60.0
/// corpus has no tool frame (the account was refused), so the shapes follow
/// the CLI's documented stream and the hand-written fixtures.
fn refine_frame(value: &Value, frame: &mut NormalizedCliFrame) {
    let key = value
        .get("tool_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let name = value
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type(value) {
        "tool_use" | "tool_call" => {
            frame.actions.push(ActionFrame::call(
                CliBackend::Gemini,
                key,
                name,
                value.get("parameters").unwrap_or(&Value::Null),
            ));
            frame.is_text = false;
        }
        "tool_result" => {
            let status = reported_status(value.get("status").and_then(Value::as_str)).unwrap_or(
                if value.get("error").is_some_and(|error| !error.is_null()) {
                    ActionStatus::Error
                } else {
                    ActionStatus::Ok
                },
            );
            // The text the event carried before: the output, or the error.
            let output =
                extract_visible_text(value).map(|text| sanitize_cli_frame_text(text.as_bytes()));
            let mut action = ActionFrame::result(key, status, None, output);
            if !name.is_empty() {
                action = action.named(CliBackend::Gemini, name);
            }
            frame.actions.push(action);
            frame.is_text = false;
        }
        _ => {}
    }
}
