//! Provider usage endpoints called with the CLI's own access token (ADR-028 §3, feature
//! `quota-credential-collectors`).

use std::path::Path;

use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::Value;

use super::http::{CLAUDE_USAGE_URL, CODEX_USAGE_URL, EndpointError, QuotaHttp};
use super::model::{QuotaWindow, UnavailableReason};
use super::sources::{claude_window, epoch_seconds};
use super::{QuotaReading, SourceError, credentials};
use crate::terminals::CliBackend;

impl From<EndpointError> for SourceError {
    fn from(error: EndpointError) -> Self {
        match error {
            EndpointError::Unavailable(reason) => reason.into(),
            EndpointError::Throttled {
                reason,
                retry_after,
            } => Self {
                reason,
                retry_after,
                throttled: true,
            },
        }
    }
}

fn header(value: &str) -> Result<HeaderValue, SourceError> {
    let mut value =
        HeaderValue::from_str(value).map_err(|_| UnavailableReason::UnexpectedResponse)?;
    value.set_sensitive(true);
    Ok(value)
}

pub(crate) fn claude_headers(token: &credentials::AccessToken) -> Result<HeaderMap, SourceError> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, header(&token.bearer())?);
    headers.insert(
        HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static("oauth-2025-04-20"),
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    Ok(headers)
}

pub(crate) fn codex_headers(
    credential: &credentials::CodexCredential,
) -> Result<HeaderMap, SourceError> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, header(&credential.token.bearer())?);
    if let Some(account) = &credential.account_id {
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            header(account)?,
        );
    }
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    Ok(headers)
}

/// The Claude usage reply (S2, 2026-10-21). Only the named windows are read; every other
/// field, including account data, is ignored by the parser.
#[derive(Deserialize)]
struct ClaudeUsage {
    five_hour: Option<ClaudeWindow>,
    seven_day: Option<ClaudeWindow>,
    seven_day_opus: Option<ClaudeWindow>,
    seven_day_sonnet: Option<ClaudeWindow>,
}

#[derive(Deserialize)]
struct ClaudeWindow {
    /// Percent, 0–100.
    utilization: Option<f64>,
    resets_at: Option<String>,
}

pub(crate) fn parse_claude(body: Value) -> Result<QuotaReading, UnavailableReason> {
    let usage: ClaudeUsage =
        serde_json::from_value(body).map_err(|_| UnavailableReason::UnexpectedResponse)?;
    let mut windows = Vec::new();
    for (name, window) in [
        ("five_hour", usage.five_hour),
        ("seven_day", usage.seven_day),
        ("seven_day_opus", usage.seven_day_opus),
        ("seven_day_sonnet", usage.seven_day_sonnet),
    ] {
        // A window the plan does not have is null or carries no utilization.
        let Some(window) = window else { continue };
        let Some(utilization) = window.utilization else {
            continue;
        };
        let (scope, minutes) = claude_window(name).ok_or(UnavailableReason::UnexpectedResponse)?;
        let resets_at = window
            .resets_at
            .map(|reset| {
                DateTime::parse_from_rfc3339(&reset)
                    .map(|reset| reset.to_utc())
                    .map_err(|_| UnavailableReason::UnexpectedResponse)
            })
            .transpose()?;
        windows.push(
            QuotaWindow::new(scope, utilization, Some(minutes), resets_at)
                .map_err(|_| UnavailableReason::UnexpectedResponse)?,
        );
    }
    Ok(QuotaReading {
        plan: None,
        windows,
        observed_at: None,
    })
}

/// The Codex usage reply (S1, 2026-10-21). Only `plan_type` and `rate_limit` are read; the
/// account id, e-mail and user id in the same body are never deserialized.
#[derive(Deserialize)]
struct CodexUsage {
    plan_type: Option<String>,
    rate_limit: Option<CodexRateLimit>,
}

#[derive(Deserialize)]
struct CodexRateLimit {
    primary_window: Option<CodexWindow>,
    secondary_window: Option<CodexWindow>,
}

#[derive(Deserialize)]
struct CodexWindow {
    used_percent: f64,
    limit_window_seconds: Option<u64>,
    reset_at: Option<i64>,
}

pub(crate) fn parse_codex(body: Value) -> Result<QuotaReading, UnavailableReason> {
    let usage: CodexUsage =
        serde_json::from_value(body).map_err(|_| UnavailableReason::UnexpectedResponse)?;
    let mut windows = Vec::new();
    if let Some(limits) = usage.rate_limit {
        for window in [limits.primary_window, limits.secondary_window]
            .into_iter()
            .flatten()
        {
            let minutes = match window.limit_window_seconds {
                Some(seconds) if seconds >= 60 => Some(
                    u32::try_from(seconds / 60)
                        .map_err(|_| UnavailableReason::UnexpectedResponse)?,
                ),
                Some(_) => return Err(UnavailableReason::UnexpectedResponse),
                None => None,
            };
            let resets_at = window.reset_at.map(epoch_seconds).transpose()?;
            windows.push(
                QuotaWindow::new(None, window.used_percent, minutes, resets_at)
                    .map_err(|_| UnavailableReason::UnexpectedResponse)?,
            );
        }
    }
    Ok(QuotaReading {
        plan: usage.plan_type,
        windows,
        observed_at: None,
    })
}

/// Reads the CLI's token from `file` and queries its provider. The token lives only inside
/// this call: it moves into a sensitive header and the credential is dropped before sending.
pub(crate) async fn read(
    http: &QuotaHttp,
    backend: CliBackend,
    file: Option<&Path>,
    now: DateTime<Utc>,
) -> Result<QuotaReading, SourceError> {
    let file = file.ok_or(UnavailableReason::NoChannel)?;
    match backend {
        CliBackend::Claude => {
            let headers = claude_headers(&credentials::read_claude(file, now)?.token)?;
            let body: Value = http.get_json(CLAUDE_USAGE_URL, headers).await?;
            Ok(parse_claude(body)?)
        }
        CliBackend::Codex => {
            let headers = codex_headers(&credentials::read_codex(file)?)?;
            let body: Value = http.get_json(CODEX_USAGE_URL, headers).await?;
            Ok(parse_codex(body)?)
        }
        _ => Err(UnavailableReason::NoChannel.into()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture(name: &str) -> Value {
        serde_json::from_str(&crate::quota::fixture(name)).unwrap()
    }

    #[test]
    fn provider_replies_parse_into_validated_windows() {
        let claude = parse_claude(fixture("claude/usage-endpoint.json")).unwrap();
        assert_eq!(claude.windows.len(), 2);
        assert_eq!(claude.windows[0].label(), "5h");
        assert_eq!(claude.windows[0].used_percent, 30.0);
        assert_eq!(claude.windows[1].label(), "Weekly");
        assert_eq!(
            claude.windows[1].resets_at.unwrap().to_rfc3339(),
            "2026-10-28T09:00:00+00:00"
        );

        let codex = parse_codex(fixture("codex/usage-endpoint.json")).unwrap();
        assert_eq!(codex.plan.as_deref(), Some("plus"));
        assert_eq!(codex.windows.len(), 2);
        assert_eq!(codex.windows[0].window_minutes, Some(300));
        assert_eq!(codex.windows[1].window_minutes, Some(10_080));
        assert_eq!(codex.windows[0].used_percent, 42.0);
    }

    #[test]
    fn quota_response_shape_changes_fail_closed() {
        let reject = |result: Result<QuotaReading, UnavailableReason>| {
            assert_eq!(result.unwrap_err(), UnavailableReason::UnexpectedResponse);
        };
        // Out of range, wrong type, unparsable timestamp, wrong top-level type.
        reject(parse_claude(json!({"five_hour": {"utilization": 120.0}})));
        reject(parse_claude(json!({"five_hour": {"utilization": -1.0}})));
        reject(parse_claude(json!({"five_hour": {"utilization": "46"}})));
        reject(parse_claude(
            json!({"five_hour": {"utilization": 4.0, "resets_at": "soon"}}),
        ));
        reject(parse_claude(json!([1, 2])));
        reject(parse_codex(
            json!({"rate_limit": {"primary_window": {"used_percent": 101}}}),
        ));
        reject(parse_codex(json!({"rate_limit": {"primary_window": {
            "used_percent": 5, "limit_window_seconds": 30
        }}})));
        reject(parse_codex(json!({"rate_limit": {"primary_window": {
            "used_percent": 5, "reset_at": i64::MAX
        }}})));
        reject(parse_codex(json!({"rate_limit": "limited"})));
        // Unknown fields are ignored; a reply without windows yields no number at all.
        assert!(
            parse_claude(json!({"five_hour": null, "new_field": {"x": 1}}))
                .unwrap()
                .windows
                .is_empty()
        );
        assert!(
            parse_codex(json!({"plan_type": "plus", "rate_limit": null}))
                .unwrap()
                .windows
                .is_empty()
        );
    }

    #[tokio::test]
    async fn requests_carry_the_token_only_in_the_authorization_header() {
        use super::super::http::test_server::{reply, serve};
        let server = serve(vec![reply(
            "200 OK",
            &[],
            &crate::quota::fixture("claude/usage-endpoint.json"),
        )])
        .await;
        let http = QuotaHttp::for_test(server.origin.clone());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"token-fixture-value","refreshToken":"refresh-fixture-value"}}"#,
        )
        .unwrap();
        let headers =
            claude_headers(&credentials::read_claude(&path, Utc::now()).unwrap().token).unwrap();
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert!(!format!("{headers:?}").contains("token-fixture-value"));
        let body: Value = http.get_json(CLAUDE_USAGE_URL, headers).await.unwrap();
        assert_eq!(parse_claude(body).unwrap().windows.len(), 2);
        let head = server.requests.lock().unwrap()[0].to_ascii_lowercase();
        assert!(head.contains("authorization: bearer token-fixture-value"));
        assert!(head.contains("anthropic-beta: oauth-2025-04-20"));
        assert!(!head.contains("refresh-fixture-value"));
        assert!(!head.contains("originator"));
        assert_eq!(head.matches("token-fixture-value").count(), 1);
    }
}
/// Spikes S1/S2: print only the reply's shape (keys, numbers, timestamps, short enum-like
/// strings); any other string is replaced by its length, so no account data is printed.
#[cfg(test)]
mod spikes {
    use super::*;

    fn shape(value: &Value) -> Value {
        match value {
            Value::String(text) => {
                let timestamp = text.len() >= 19
                    && text.as_bytes()[4] == b'-'
                    && text.as_bytes()[10] == b'T'
                    && text[..4].bytes().all(|byte| byte.is_ascii_digit());
                let enum_like = text.len() <= 24
                    && text
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
                if timestamp || enum_like {
                    value.clone()
                } else {
                    Value::String(format!("<string len {}>", text.len()))
                }
            }
            Value::Array(items) => Value::Array(items.iter().map(shape).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), shape(value)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn sha(path: &std::path::Path) -> Vec<u8> {
        use sha2::Digest;
        sha2::Sha256::digest(std::fs::read(path).unwrap()).to_vec()
    }

    #[tokio::test]
    #[ignore = "spike S2: reads the signed-in Claude Code token and calls api.anthropic.com once"]
    async fn spike_claude_usage_endpoint_shape() {
        let path = credentials::claude_credentials_path().unwrap();
        let before = sha(&path);
        let credential = credentials::read_claude(&path, Utc::now()).unwrap();
        let headers = claude_headers(&credential.token).unwrap();
        drop(credential);
        let started = std::time::Instant::now();
        let (status, content_type, json) =
            super::super::http::spike_probe(CLAUDE_USAGE_URL, headers).await;
        println!(
            "S2 claude: status={status} content_type={content_type:?} elapsed_ms={}",
            started.elapsed().as_millis()
        );
        if let Some(json) = json {
            println!(
                "S2 claude shape: {}",
                serde_json::to_string_pretty(&shape(&json)).unwrap()
            );
        }
        assert_eq!(before, sha(&path), "the credential file must not change");
    }

    #[tokio::test]
    #[ignore = "spike S1: reads the signed-in Codex token and calls chatgpt.com once, honestly identified"]
    async fn spike_codex_usage_endpoint_shape() {
        let path = credentials::codex_auth_path().unwrap();
        let before = sha(&path);
        let credential = credentials::read_codex(&path).unwrap();
        println!(
            "S1 codex: account id present={}",
            credential.account_id.is_some()
        );
        let headers = codex_headers(&credential).unwrap();
        drop(credential);
        let started = std::time::Instant::now();
        let (status, content_type, json) =
            super::super::http::spike_probe(CODEX_USAGE_URL, headers).await;
        println!(
            "S1 codex: user_agent={} status={status} content_type={content_type:?} elapsed_ms={}",
            super::super::http::USER_AGENT,
            started.elapsed().as_millis()
        );
        if let Some(json) = json {
            println!(
                "S1 codex shape: {}",
                serde_json::to_string_pretty(&shape(&json)).unwrap()
            );
        }
        assert_eq!(before, sha(&path), "the credential file must not change");
    }
}
