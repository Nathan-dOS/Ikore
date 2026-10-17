use super::*;

pub(super) struct CustomAdapter;

impl CliAdapter for CustomAdapter {
    fn capabilities(&self, _version: Option<&str>) -> CliCapabilities {
        base_capabilities(
            CliOutputFormat::Text,
            false,
            false,
            &[],
            false,
            false,
            false,
        )
    }

    fn build_process_spec(
        &self,
        request: CliInvocationRequest,
    ) -> Result<CliProcessSpec, TerminalError> {
        if request.resume_session_id.is_some() {
            return Err(TerminalError::Validation(
                "Custom offers no native resume".into(),
            ));
        }
        if request.model_profile.is_some() || request.thinking_level != ThinkingLevel::Default {
            return Err(TerminalError::Validation(
                "Custom declares no thinking capability".into(),
            ));
        }
        if request
            .executable
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "cmd" | "bat" | "ps1"))
        {
            return Err(TerminalError::Validation(
                "Custom cannot invoke a shell".into(),
            ));
        }
        if request.custom_args.len() > super::super::MAX_CUSTOM_ARGS {
            return Err(TerminalError::Validation(
                "argumentos Custom excedem o limite".into(),
            ));
        }
        Ok(CliProcessSpec {
            executable: request.executable,
            args: request.custom_args,
            environment: request
                .model
                .map(|model| vec![("HVC_SELECTED_MODEL".into(), model)])
                .unwrap_or_default(),
            stdin_payload: request.prompt.into_bytes(),
            resume_session_id: None,
        })
    }

    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame {
        if stream == "stdout"
            && let Ok(value) = serde_json::from_slice::<Value>(bytes)
            && value.get("type").and_then(Value::as_str) == Some("hvc_model_attestation_v1")
        {
            let model = value
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            return NormalizedCliFrame {
                kind: TerminalEventKind::System,
                content: String::new(),
                native_session_id: None,
                observed_model: model,
                message_id: value
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                parent_invocation_id: value
                    .get("parentInvocationId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                agent_id: value
                    .get("agentId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                is_model_generation: true,
                is_text: false,
                is_delta: false,
                finish_reason: None,
                is_reasoning: false,
            };
        }
        NormalizedCliFrame {
            kind: if stream == "stderr" {
                TerminalEventKind::Stderr
            } else {
                TerminalEventKind::Stdout
            },
            content: sanitize_terminal_text(bytes),
            native_session_id: None,
            observed_model: None,
            message_id: None,
            parent_invocation_id: None,
            agent_id: None,
            is_model_generation: false,
            is_text: stream != "stderr",
            is_delta: false,
            finish_reason: None,
            is_reasoning: false,
        }
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        None
    }

    fn classify_failure(&self, _exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        classify_failure_text(stderr)
    }
}
