use super::{
    ActionFrame, ActionStatus, CliAdapter, CliBackend, CliCapabilities, CliFailureKind,
    CliInvocationRequest, CliOutputFormat, CliProcessSpec, NormalizedCliFrame, TerminalError,
    TerminalEventKind, ThinkingLevel, Value, append_model, base_capabilities,
    classify_failure_text, event_type, normalize_json_frame_with, reported_status,
    sanitize_cli_frame_text, version_supports_contract,
};
use crate::terminals::actions::mcp_display_name;

pub(super) struct CodexAdapter;

impl CliAdapter for CodexAdapter {
    fn capabilities(&self, version: Option<&str>) -> CliCapabilities {
        CliCapabilities {
            // On Windows the read-only sandbox refuses every command before it
            // becomes an item (ADR-040, A2): the stream then has no actions.
            action_frames: true,
            ..base_capabilities(
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
        normalize_json_frame_with(
            CliBackend::Codex,
            stream,
            bytes,
            |value| {
                let kind = event_type(value);
                if kind.contains("failed") || kind.contains("error") {
                    TerminalEventKind::Error
                } else if kind == "thread.started" {
                    TerminalEventKind::System
                } else if kind == "turn.completed" {
                    TerminalEventKind::Completed
                } else if kind.starts_with("item.") {
                    match item_type(value) {
                        "command_execution" | "mcp_tool_call" | "file_change" | "web_search" => {
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
            },
            refine_frame,
        )
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        Some(&["login", "status"])
    }

    fn classify_failure(&self, _exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        classify_failure_text(stderr)
    }
}

fn item_type(value: &Value) -> &str {
    value
        .pointer("/item/type")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Codex names an action by its item type (specification, section 3.2) and
/// pairs the completion with the start by `item.id`; `item.updated` only moves
/// the phase. A reasoning item is never text.
fn refine_frame(value: &Value, frame: &mut NormalizedCliFrame) {
    let event = event_type(value);
    let Some(item) = value.get("item").filter(|_| event.starts_with("item.")) else {
        return;
    };
    let name = match item_type(value) {
        "reasoning" => {
            frame.is_reasoning = true;
            return;
        }
        "command_execution" => "shell".to_owned(),
        "file_change" => "edit".to_owned(),
        "web_search" => "web_search".to_owned(),
        "todo_list" => "todo_list".to_owned(),
        "mcp_tool_call" => mcp_display_name(
            item.get("server")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            item.get("tool").and_then(Value::as_str).unwrap_or_default(),
        )
        .unwrap_or_else(|| "other".to_owned()),
        _ => return,
    };
    // The plan's steps and a tool's input are never the agent's message.
    frame.is_text = false;
    let key = item.get("id").and_then(Value::as_str).map(str::to_owned);
    let mut action = match event {
        "item.started" => ActionFrame::call(CliBackend::Codex, key, &name, item),
        "item.completed" => {
            let exit_code = item
                .get("exit_code")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok());
            let status = match (
                reported_status(item.get("status").and_then(Value::as_str)),
                exit_code,
            ) {
                (_, Some(code)) if code != 0 => ActionStatus::Error,
                (Some(status), _) => status,
                (None, _) => ActionStatus::Ok,
            };
            // A command's output is its result text (specification, 3.4); an
            // MCP result keeps the text the event showed before.
            let output = match item_type(value) {
                "command_execution" => item
                    .get("aggregated_output")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                "mcp_tool_call" => super::extract_visible_text(item),
                _ => None,
            }
            .map(|text| sanitize_cli_frame_text(text.as_bytes()));
            ActionFrame::call_with_result(
                CliBackend::Codex,
                key,
                &name,
                item,
                status,
                exit_code,
                output,
            )
        }
        _ => return,
    };
    if let Some((text, path)) = file_change_target(item) {
        action.target_text = Some(text);
        action.target_path = Some(path);
    }
    frame.actions.push(action);
}

/// The first changed path, and how many more the change touches.
fn file_change_target(item: &Value) -> Option<(String, String)> {
    let changes = item.get("changes")?.as_array()?;
    let first = changes.first()?.get("path")?.as_str()?;
    let text = match changes.len() {
        1 => first.to_owned(),
        count => format!("{first} +{}", count - 1),
    };
    Some((text, first.to_owned()))
}
