//! Read-only access to a subscription CLI's own access token (ADR-028 G1–G3).
//!
//! Only the CLI's default credential file is read, never an OS keyring entry. Only the access
//! token (plus Claude's expiry and Codex's account id, which the request needs) is
//! deserialized; refresh tokens, ID tokens and API keys are skipped by the parser and the raw
//! file bytes are zeroized after parsing. Nothing is ever written back and no token is refreshed.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Deserializer};
use zeroize::Zeroizing;

use super::model::UnavailableReason;

/// A credential file larger than this is not a CLI credential file.
const MAX_CREDENTIAL_FILE_BYTES: u64 = 64 * 1024;
/// A token this close to its expiry is treated as expired: the request would race it.
const EXPIRY_MARGIN: Duration = Duration::seconds(60);

/// A CLI access token. Zeroized on drop; `Debug` never prints it and there is no `Display`.
pub struct AccessToken(Zeroizing<String>);

impl AccessToken {
    /// The `Authorization` header value, itself zeroized when dropped.
    pub(crate) fn bearer(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("Bearer {}", self.0.as_str()))
    }

    #[cfg(test)]
    pub(crate) fn expose_for_test(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessToken(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for AccessToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Zeroizing::new(String::deserialize(deserializer)?);
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(serde::de::Error::custom("empty or malformed access token"));
        }
        Ok(Self(value))
    }
}

pub struct ClaudeCredential {
    pub token: AccessToken,
    pub expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for ClaudeCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeCredential")
            .field("token", &self.token)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub struct CodexCredential {
    pub token: AccessToken,
    /// Sent as a request header only; never stored, logged or shown.
    pub account_id: Option<Zeroizing<String>>,
}

impl fmt::Debug for CodexCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredential")
            .field("token", &self.token)
            .field(
                "account_id",
                &self.account_id.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Deserialize)]
struct ClaudeFile {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<ClaudeOauth>,
}

#[derive(Deserialize)]
struct ClaudeOauth {
    #[serde(rename = "accessToken")]
    access_token: AccessToken,
    /// Epoch milliseconds.
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
struct CodexFile {
    tokens: Option<CodexTokens>,
}

#[derive(Deserialize)]
struct CodexTokens {
    access_token: AccessToken,
    account_id: Option<String>,
}

type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn non_empty(env: EnvLookup<'_>, name: &str) -> Option<PathBuf> {
    env(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir(env: EnvLookup<'_>) -> Option<PathBuf> {
    non_empty(env, "USERPROFILE").or_else(|| non_empty(env, "HOME"))
}

/// `<CLAUDE_CONFIG_DIR or ~/.claude>/.credentials.json`.
pub fn claude_credentials_path() -> Option<PathBuf> {
    claude_credentials_path_in(&|name| std::env::var_os(name))
}

fn claude_credentials_path_in(env: EnvLookup<'_>) -> Option<PathBuf> {
    let directory = non_empty(env, "CLAUDE_CONFIG_DIR")
        .or_else(|| home_dir(env).map(|home| home.join(".claude")))?;
    Some(directory.join(".credentials.json"))
}

/// `<CODEX_HOME or ~/.codex>/auth.json`.
pub fn codex_auth_path() -> Option<PathBuf> {
    codex_auth_path_in(&|name| std::env::var_os(name))
}

fn codex_auth_path_in(env: EnvLookup<'_>) -> Option<PathBuf> {
    let directory =
        non_empty(env, "CODEX_HOME").or_else(|| home_dir(env).map(|home| home.join(".codex")))?;
    Some(directory.join("auth.json"))
}

/// Reads the whole file into a buffer that is zeroized on drop. A missing file means the CLI
/// keeps its sign-in elsewhere (an OS credential store), which is never read.
fn read_bounded(path: &Path) -> Result<Zeroizing<Vec<u8>>, UnavailableReason> {
    let file = std::fs::File::open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => UnavailableReason::CredentialStoreNotFile,
        _ => UnavailableReason::SourceFailed,
    })?;
    let metadata = file
        .metadata()
        .map_err(|_| UnavailableReason::SourceFailed)?;
    if !metadata.is_file() || metadata.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(UnavailableReason::UnexpectedResponse);
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(metadata.len() as usize));
    file.take(MAX_CREDENTIAL_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UnavailableReason::SourceFailed)?;
    if bytes.len() as u64 > MAX_CREDENTIAL_FILE_BYTES {
        return Err(UnavailableReason::UnexpectedResponse);
    }
    Ok(bytes)
}

pub fn read_claude(path: &Path, now: DateTime<Utc>) -> Result<ClaudeCredential, UnavailableReason> {
    let bytes = read_bounded(path)?;
    let file: ClaudeFile =
        serde_json::from_slice(&bytes).map_err(|_| UnavailableReason::UnexpectedResponse)?;
    // No OAuth entry: the CLI is signed in with an API key, which has no plan windows.
    let oauth = file.oauth.ok_or(UnavailableReason::NotSignedIn)?;
    let expires_at = match oauth.expires_at {
        Some(millis) => Some(
            DateTime::from_timestamp_millis(millis).ok_or(UnavailableReason::UnexpectedResponse)?,
        ),
        None => None,
    };
    if expires_at.is_some_and(|expires_at| expires_at <= now + EXPIRY_MARGIN) {
        return Err(UnavailableReason::SignInExpired);
    }
    Ok(ClaudeCredential {
        token: oauth.access_token,
        expires_at,
    })
}

pub fn read_codex(path: &Path) -> Result<CodexCredential, UnavailableReason> {
    let bytes = read_bounded(path)?;
    let file: CodexFile =
        serde_json::from_slice(&bytes).map_err(|_| UnavailableReason::UnexpectedResponse)?;
    // No ChatGPT tokens: the CLI is signed in with an API key, which has no plan windows.
    let tokens = file.tokens.ok_or(UnavailableReason::NotSignedIn)?;
    Ok(CodexCredential {
        token: tokens.access_token,
        account_id: tokens
            .account_id
            .filter(|id| !id.trim().is_empty() && !id.chars().any(char::is_control))
            .map(Zeroizing::new),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn sha(path: &Path) -> Vec<u8> {
        use sha2::Digest;
        sha2::Sha256::digest(std::fs::read(path).unwrap()).to_vec()
    }

    #[test]
    fn quota_ignores_refresh_and_id_tokens_and_api_keys() {
        let directory = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let expires = (now + Duration::hours(2)).timestamp_millis();
        let claude = write(
            directory.path(),
            ".credentials.json",
            &format!(
                r#"{{"claudeAiOauth":{{"accessToken":"claude-access-fixture","refreshToken":"claude-refresh-fixture","expiresAt":{expires},"scopes":["user:inference"]}},"primaryApiKey":"sk-ant-fixture-api-key"}}"#
            ),
        );
        let credential = read_claude(&claude, now).unwrap();
        assert_eq!(credential.token.expose_for_test(), "claude-access-fixture");
        let debug = format!("{credential:?}");
        for secret in [
            "claude-access-fixture",
            "claude-refresh-fixture",
            "sk-ant-fixture",
        ] {
            assert!(!debug.contains(secret), "{debug}");
        }

        let codex = write(
            directory.path(),
            "auth.json",
            r#"{"OPENAI_API_KEY":"sk-fixture-api-key","tokens":{"id_token":"codex-id-fixture","access_token":"codex-access-fixture","refresh_token":"codex-refresh-fixture","account_id":"acct-fixture"},"last_refresh":"2026-10-21T00:00:00Z"}"#,
        );
        let credential = read_codex(&codex).unwrap();
        assert_eq!(credential.token.expose_for_test(), "codex-access-fixture");
        let debug = format!("{credential:?}");
        for secret in [
            "codex-access-fixture",
            "codex-refresh-fixture",
            "codex-id-fixture",
            "acct-fixture",
            "sk-fixture",
        ] {
            assert!(!debug.contains(secret), "{debug}");
        }

        // API-key logins carry no plan windows.
        let api_only = write(directory.path(), "api.json", r#"{"OPENAI_API_KEY":"sk-x"}"#);
        assert_eq!(
            read_codex(&api_only).unwrap_err(),
            UnavailableReason::NotSignedIn
        );
        let claude_api = write(
            directory.path(),
            "claude-api.json",
            r#"{"primaryApiKey":"sk-ant-x"}"#,
        );
        assert_eq!(
            read_claude(&claude_api, now).unwrap_err(),
            UnavailableReason::NotSignedIn
        );
    }

    #[test]
    fn quota_expired_token_reports_sign_in_expired_without_a_request() {
        let directory = tempfile::tempdir().unwrap();
        let now = Utc::now();
        for expires in [now - Duration::minutes(5), now + Duration::seconds(30)] {
            let path = write(
                directory.path(),
                ".credentials.json",
                &format!(
                    r#"{{"claudeAiOauth":{{"accessToken":"a","expiresAt":{}}}}}"#,
                    expires.timestamp_millis()
                ),
            );
            assert_eq!(
                read_claude(&path, now).unwrap_err(),
                UnavailableReason::SignInExpired
            );
        }
    }

    #[test]
    fn quota_credentials_are_read_from_the_cli_directory_only() {
        let directory = tempfile::tempdir().unwrap();
        // A missing file means a keyring-backed store, which is never read.
        assert_eq!(
            read_claude(&directory.path().join(".credentials.json"), Utc::now()).unwrap_err(),
            UnavailableReason::CredentialStoreNotFile
        );
        assert_eq!(
            read_codex(&directory.path().join("auth.json")).unwrap_err(),
            UnavailableReason::CredentialStoreNotFile
        );
        // Oversized, malformed and directory "files" fail closed.
        let huge = write(
            directory.path(),
            "huge.json",
            &"x".repeat(MAX_CREDENTIAL_FILE_BYTES as usize + 1),
        );
        assert_eq!(
            read_codex(&huge).unwrap_err(),
            UnavailableReason::UnexpectedResponse
        );
        let malformed = write(directory.path(), "bad.json", "{not json");
        assert_eq!(
            read_codex(&malformed).unwrap_err(),
            UnavailableReason::UnexpectedResponse
        );
        assert_ne!(
            read_codex(directory.path()).unwrap_err(),
            UnavailableReason::NotSignedIn
        );

        // The locations follow the CLIs' own directory variables, then the home directory.
        let overrides = |name: &str| match name {
            "CLAUDE_CONFIG_DIR" => Some(OsString::from("D:/claude-config")),
            "CODEX_HOME" => Some(OsString::from("D:/codex-home")),
            "USERPROFILE" => Some(OsString::from("D:/home")),
            _ => None,
        };
        assert_eq!(
            claude_credentials_path_in(&overrides).unwrap(),
            Path::new("D:/claude-config").join(".credentials.json")
        );
        assert_eq!(
            codex_auth_path_in(&overrides).unwrap(),
            Path::new("D:/codex-home").join("auth.json")
        );
        let defaults = |name: &str| match name {
            "CLAUDE_CONFIG_DIR" | "CODEX_HOME" => Some(OsString::new()),
            "HOME" => Some(OsString::from("/home/dev")),
            _ => None,
        };
        assert_eq!(
            claude_credentials_path_in(&defaults).unwrap(),
            Path::new("/home/dev")
                .join(".claude")
                .join(".credentials.json")
        );
        assert_eq!(
            codex_auth_path_in(&defaults).unwrap(),
            Path::new("/home/dev").join(".codex").join("auth.json")
        );
    }

    #[test]
    fn quota_never_refreshes_or_writes_cli_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let claude = write(
            directory.path(),
            ".credentials.json",
            &format!(
                r#"{{"claudeAiOauth":{{"accessToken":"a","refreshToken":"r","expiresAt":{}}}}}"#,
                (now - Duration::hours(1)).timestamp_millis()
            ),
        );
        let codex = write(
            directory.path(),
            "auth.json",
            r#"{"tokens":{"access_token":"a","refresh_token":"r"}}"#,
        );
        let before = (
            sha(&claude),
            sha(&codex),
            std::fs::metadata(&claude).unwrap().modified().unwrap(),
        );
        for _ in 0..3 {
            let _ = read_claude(&claude, now);
            let _ = read_codex(&codex);
        }
        assert_eq!(
            before,
            (
                sha(&claude),
                sha(&codex),
                std::fs::metadata(&claude).unwrap().modified().unwrap()
            )
        );
        // No refresh endpoint is part of the pinned destinations.
        for url in super::super::http::PINNED_URLS {
            assert!(!url.contains("token") && !url.contains("refresh"), "{url}");
        }
    }
}
