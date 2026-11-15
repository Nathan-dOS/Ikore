use super::{
    ActionFrame, ActionStatus, CliAdapter, CliBackend, CliCapabilities, CliFailureKind,
    CliInvocationRequest, CliOutputFormat, CliProcessSpec, NormalizedCliFrame, TerminalError,
    TerminalEventKind, ThinkingLevel, Value, append_model, base_capabilities,
    classify_failure_text, event_type, normalize_json_frame_with, sanitize_cli_frame_text,
    version_supports_contract,
};

pub(super) struct OpenCodeAdapter;

impl CliAdapter for OpenCodeAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        CliCapabilities {
            action_frames: true,
            ..base_capabilities(
                CliOutputFormat::JsonLines,
                false,
                version_supports_contract(CliBackend::Opencode, version),
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
    }

    fn build_process_spec(
        &self,
        request: CliInvocationRequest,
    ) -> Result<CliProcessSpec, TerminalError> {
        let mut args = ["--pure", "run", "--format", "json"]
            .map(str::to_owned)
            .to_vec();
        if let Some(session_id) = request.resume_session_id.as_ref() {
            args.extend(["--session".into(), session_id.clone()]);
        }
        append_model(&mut args, request.model.as_deref());
        let model_profile = request.model_profile.as_deref().or_else(|| {
            (request.thinking_level != ThinkingLevel::Default)
                .then_some(request.thinking_level.as_str())
        });
        if let Some(model_profile) = model_profile {
            args.extend(["--variant".into(), model_profile.into()]);
        }
        Ok(CliProcessSpec {
            executable: request.executable,
            args,
            environment: vec![
                (
                    "OPENCODE_PERMISSION".into(),
                    r#"{"*":"deny","read":"allow","glob":"allow","grep":"allow","list":"allow","lsp":"allow"}"#.into(),
                ),
                ("OPENCODE_DISABLE_LSP_DOWNLOAD".into(), "true".into()),
            ],
            stdin_payload: request.prompt.into_bytes(),
            resume_session_id: request.resume_session_id,
        })
    }

    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame {
        normalize_json_frame_with(
            CliBackend::Opencode,
            stream,
            bytes,
            |value| {
                let kind = event_type(value);
                if kind.contains("error") {
                    TerminalEventKind::Error
                } else if kind.contains("tool") && kind.contains("result") {
                    TerminalEventKind::ToolResult
                } else if kind.contains("tool") {
                    TerminalEventKind::ToolUse
                } else if matches!(kind, "step_finish" | "result" | "session.idle") {
                    TerminalEventKind::Completed
                } else if matches!(kind, "step_start" | "session.created") {
                    TerminalEventKind::System
                } else {
                    TerminalEventKind::Message
                }
            },
            refine_frame,
        )
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        Some(&["auth", "list"])
    }

    fn classify_failure(&self, _exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        classify_failure_text(stderr)
    }
}

/// A tool part (`part.tool`, `part.callID`, `part.state`) usually arrives
/// once, finished: `completed` with its `output`, or `error` (ADR-040, A5). A
/// part still `pending` or `running` opens the call; the finished one closes
/// it under the same `callID`.
fn refine_frame(value: &Value, frame: &mut NormalizedCliFrame) {
    let Some(part) = value.get("part") else {
        return;
    };
    let Some(tool) = part.get("tool").and_then(Value::as_str) else {
        return;
    };
    let key = part
        .get("callID")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let state = part.get("state").unwrap_or(&Value::Null);
    let input = state.get("input").unwrap_or(&Value::Null);
    // The output the event carried before; an error carried none.
    let output = || {
        state
            .get("output")
            .and_then(Value::as_str)
            .map(|text| sanitize_cli_frame_text(text.as_bytes()))
    };
    let action = match state.get("status").and_then(Value::as_str) {
        Some("completed") => ActionFrame::call_with_result(
            CliBackend::Opencode,
            key,
            tool,
            input,
            ActionStatus::Ok,
            None,
            output(),
        ),
        Some("error") => ActionFrame::call_with_result(
            CliBackend::Opencode,
            key,
            tool,
            input,
            ActionStatus::Error,
            None,
            None,
        ),
        _ if event_type(value).contains("result") => {
            ActionFrame::result(key, ActionStatus::Ok, None, output())
                .named(CliBackend::Opencode, tool)
        }
        _ => ActionFrame::call(CliBackend::Opencode, key, tool, input),
    };
    frame.actions.push(action);
    frame.is_text = false;
}
