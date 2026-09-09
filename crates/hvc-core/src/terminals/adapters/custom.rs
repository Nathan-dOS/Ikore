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
                "Custom não oferece retomada nativa".into(),
            ));
        }
        if request.model_profile.is_some() || request.thinking_level != ThinkingLevel::Default {
            return Err(TerminalError::Validation(
                "Custom não declara capacidade de thinking".into(),
            ));
        }
        if request
            .executable
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "cmd" | "bat" | "ps1"))
        {
            return Err(TerminalError::Validation(
                "Custom não pode invocar um shell".into(),
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
            environment: Vec::new(),
            stdin_payload: request.prompt.into_bytes(),
            resume_session_id: None,
        })
    }

    fn normalize_frame(&self, stream: &str, bytes: &[u8]) -> NormalizedCliFrame {
        NormalizedCliFrame {
            kind: if stream == "stderr" {
                TerminalEventKind::Stderr
            } else {
                TerminalEventKind::Stdout
            },
            content: sanitize_terminal_text(bytes),
            native_session_id: None,
            observed_model: None,
            is_text: stream != "stderr",
        }
    }

    fn auth_probe_args(&self) -> Option<&'static [&'static str]> {
        None
    }

    fn classify_failure(&self, _exit_code: Option<i32>, stderr: &str) -> CliFailureKind {
        classify_failure_text(stderr)
    }
}
