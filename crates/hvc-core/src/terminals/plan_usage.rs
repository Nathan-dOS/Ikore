//! Read-only subscription usage for the global Connections page.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::Command;

use super::{
    CliAvailability, CliAvailabilityState, CliBackend, CliQuotaBucket, ProcessTreeJob,
    configure_hidden, copy_safe_environment, secure_probe_spec,
};

const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_LIMIT_GROUPS: usize = 16;
const USAGE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize)]
pub struct CliConnectionStatus {
    pub availability: CliAvailability,
    pub plan: Option<String>,
    pub quota_source: Option<&'static str>,
    pub quota_diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliConnectionsSnapshot {
    pub connections: Vec<CliConnectionStatus>,
    pub checked_at: String,
}

pub(super) async fn connections(discovery: Vec<CliAvailability>) -> CliConnectionsSnapshot {
    let mut connections = Vec::new();
    for availability in discovery {
        if availability.backend == CliBackend::Custom {
            // Custom executables belong to individual agent configurations, not a global login.
            continue;
        }
        let mut connection = CliConnectionStatus {
            availability,
            plan: None,
            quota_source: None,
            quota_diagnostic: None,
        };
        if connection.availability.state == CliAvailabilityState::Available {
            if connection.availability.backend == CliBackend::Codex {
                if let Some(executable) = &connection.availability.executable_path {
                    match codex_usage(executable).await {
                        Ok(usage) => {
                            connection.plan = usage.plan;
                            connection.availability.quota = usage.quota;
                            connection.quota_source = Some("CLI app-server");
                        }
                        Err(_) => {
                            // RPC errors can contain account details. Keep raw output out of UI/logs.
                            connection.quota_diagnostic = Some(
                                "Plan usage is unavailable. Reload to retry; API-key accounts may not expose plan limits."
                                    .into(),
                            );
                        }
                    }
                }
            } else if connection.availability.backend == CliBackend::Antigravity
                && !connection.availability.quota.is_empty()
            {
                connection.quota_source = Some("CLI /usage");
            }
            if connection.availability.quota.is_empty() && connection.quota_diagnostic.is_none() {
                connection.quota_diagnostic = Some(
                    "This CLI does not report plan usage through its connection check.".into(),
                );
            }
        }
        connections.push(connection);
    }
    CliConnectionsSnapshot {
        connections,
        checked_at: Utc::now().to_rfc3339(),
    }
}

async fn codex_usage(executable: &Path) -> std::io::Result<CodexUsage> {
    let spec = secure_probe_spec(CliBackend::Codex, executable, vec!["app-server".into()]).await?;
    let directory = tempfile::tempdir()?;
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.args)
        .current_dir(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env_clear();
    copy_safe_environment(&mut command);
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        command.env("CODEX_HOME", home);
    }
    for (name, value) in &spec.environment {
        command.env(name, value);
    }
    configure_hidden(&mut command);
    let mut child = command.spawn()?;
    let process_tree = ProcessTreeJob::assign(&child)?;
    let stdin = child.stdin.take().ok_or_else(invalid_response)?;
    let stdout = child.stdout.take().ok_or_else(invalid_response)?;
    let result = tokio::time::timeout(USAGE_TIMEOUT, read_codex_usage(stdin, stdout)).await;
    process_tree.terminate();
    let _ = child.kill().await;
    let _ = child.wait().await;
    result.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Plan usage timed out"))?
}

async fn read_codex_usage(
    mut stdin: impl AsyncWrite + Unpin,
    stdout: impl AsyncRead + Unpin,
) -> std::io::Result<CodexUsage> {
    // One budget across all notifications and replies, including an unterminated line.
    let mut reader = BufReader::new(stdout.take(MAX_RESPONSE_BYTES + 1));
    let mut budget = MAX_RESPONSE_BYTES as usize;
    send(
        &mut stdin,
        json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "hard_vibe_code", "version": env!("CARGO_PKG_VERSION")}
        }}),
    )
    .await?;
    reply(&mut reader, &mut budget, 1).await?;
    send(&mut stdin, json!({"method": "initialized"})).await?;
    send(
        &mut stdin,
        json!({"id": 2, "method": "account/rateLimits/read"}),
    )
    .await?;
    let result = reply(&mut reader, &mut budget, 2).await?;
    parse_codex_usage(result)
}

async fn send(writer: &mut (impl AsyncWrite + Unpin), value: Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await
}

async fn reply(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    budget: &mut usize,
    id: u64,
) -> std::io::Result<Value> {
    loop {
        let mut line = Vec::new();
        let read = reader.read_until(b'\n', &mut line).await?;
        if read == 0 || read > *budget {
            return Err(invalid_response());
        }
        *budget -= read;
        let value: Value = serde_json::from_slice(&line).map_err(|_| invalid_response())?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if value.get("error").is_some() {
                return Err(invalid_response());
            }
            return value.get("result").cloned().ok_or_else(invalid_response);
        }
    }
}

#[derive(Debug)]
struct CodexUsage {
    plan: Option<String>,
    quota: Vec<CliQuotaBucket>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse {
    rate_limits: RateLimits,
    rate_limits_by_limit_id: Option<BTreeMap<String, RateLimits>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimits {
    limit_name: Option<String>,
    plan_type: Option<String>,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateWindow {
    used_percent: u8,
    window_duration_mins: Option<u64>,
    resets_at: Option<i64>,
}

fn parse_codex_usage(value: Value) -> std::io::Result<CodexUsage> {
    let response: RateLimitsResponse =
        serde_json::from_value(value).map_err(|_| invalid_response())?;
    let groups = match response.rate_limits_by_limit_id {
        Some(groups) if !groups.is_empty() => groups,
        _ => BTreeMap::from([("Codex".to_string(), response.rate_limits)]),
    };
    let mut usage = CodexUsage {
        plan: None,
        quota: Vec::new(),
    };
    for (id, limits) in groups.into_iter().take(MAX_LIMIT_GROUPS) {
        if usage.plan.is_none() {
            usage.plan = limits.plan_type.map(|plan| label(&plan));
        }
        let group = label(limits.limit_name.as_deref().unwrap_or(&id));
        for (fallback, window) in [("Primary", limits.primary), ("Secondary", limits.secondary)] {
            let Some(window) = window else { continue };
            if window.used_percent > 100 || window.window_duration_mins == Some(0) {
                return Err(invalid_response());
            }
            let reset_time = window
                .resets_at
                .map(|epoch| {
                    DateTime::from_timestamp(epoch, 0)
                        .map(|time| time.to_rfc3339())
                        .ok_or_else(invalid_response)
                })
                .transpose()?;
            let window_label = match window.window_duration_mins {
                Some(10080) => "weekly".into(),
                Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
                Some(minutes) => format!("{minutes} min"),
                None => fallback.into(),
            };
            usage.quota.push(CliQuotaBucket {
                group: group.clone(),
                window: window_label,
                remaining_percent: 100 - window.used_percent,
                reset_time,
            });
        }
    }
    Ok(usage)
}

fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect()
}

fn invalid_response() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "Plan usage is unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_plan_usage_prefers_all_metered_groups_and_preserves_zero_and_exhaustion() {
        let usage = parse_codex_usage(json!({
            "rateLimits": {"primary": {"usedPercent": 45}},
            "rateLimitsByLimitId": {
                "codex": {"planType": "plus", "primary": {
                    "usedPercent": 0, "windowDurationMins": 300, "resetsAt": 2000000000
                }, "secondary": {"usedPercent": 100, "windowDurationMins": 10080}},
                "other": {"primary": {"usedPercent": 25, "windowDurationMins": 60}}
            }
        }))
        .unwrap();
        assert_eq!(usage.plan.as_deref(), Some("plus"));
        assert_eq!(usage.quota.len(), 3);
        assert_eq!(usage.quota[0].window, "5h");
        assert_eq!(usage.quota[0].remaining_percent, 100);
        assert!(usage.quota[0].reset_time.is_some());
        assert_eq!(usage.quota[1].window, "weekly");
        assert_eq!(usage.quota[1].remaining_percent, 0);
    }

    #[test]
    fn codex_plan_usage_handles_legacy_and_unavailable_windows_without_guessing() {
        for extra in [json!(null), json!({})] {
            let usage = parse_codex_usage(json!({
                "rateLimits": {"primary": {"usedPercent": 12}},
                "rateLimitsByLimitId": extra
            }))
            .unwrap();
            assert_eq!(usage.quota[0].window, "Primary");
            assert_eq!(usage.quota[0].remaining_percent, 88);
        }
        assert!(
            parse_codex_usage(json!({"rateLimits": {}}))
                .unwrap()
                .quota
                .is_empty()
        );
        for invalid in [json!(-1), json!(101), json!(12.5), json!("12")] {
            assert!(
                parse_codex_usage(json!({
                    "rateLimits": {"primary": {"usedPercent": invalid}}
                }))
                .is_err()
            );
        }
        assert!(
            parse_codex_usage(json!({"rateLimits": {"primary": {
                "usedPercent": 1, "resetsAt": i64::MAX
            }}}))
            .is_err()
        );
    }

    #[tokio::test]
    async fn codex_usage_initializes_then_reads_limits_without_creating_a_thread() {
        let (client, server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(client);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut reader = BufReader::new(read);
            for method in ["initialize", "initialized", "account/rateLimits/read"] {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], method);
                if method == "initialize" {
                    send(&mut write, json!({"id": 1, "result": {}}))
                        .await
                        .unwrap();
                } else if method == "account/rateLimits/read" {
                    send(
                        &mut write,
                        json!({"method": "account/rateLimits/updated", "params": {}}),
                    )
                    .await
                    .unwrap();
                    send(
                        &mut write,
                        json!({"id": 2, "result": {"rateLimits": {
                            "primary": {"usedPercent": 20, "windowDurationMins": 300}
                        }}}),
                    )
                    .await
                    .unwrap();
                }
            }
        });
        let usage = read_codex_usage(write, read).await.unwrap();
        assert_eq!(usage.quota[0].remaining_percent, 80);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn codex_usage_rejects_oversized_and_error_replies() {
        let oversized = vec![b'x'; MAX_RESPONSE_BYTES as usize + 10];
        assert!(
            read_codex_usage(tokio::io::sink(), oversized.as_slice())
                .await
                .is_err()
        );
        let error = b"{\"id\":1,\"error\":{\"message\":\"sensitive account details\"}}\n";
        let error = read_codex_usage(tokio::io::sink(), &error[..])
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("sensitive"));
    }

    #[tokio::test]
    #[ignore = "requires an installed Codex CLI and a signed-in subscription account"]
    async fn codex_plan_usage_live_read() {
        let cli = super::super::discover_clis()
            .into_iter()
            .find(|cli| cli.backend == CliBackend::Codex)
            .unwrap();
        let usage = codex_usage(cli.executable_path.as_deref().expect("Codex installed"))
            .await
            .unwrap();
        assert!(!usage.quota.is_empty());
        assert!(
            usage
                .quota
                .iter()
                .all(|window| window.remaining_percent <= 100)
        );
    }
}
