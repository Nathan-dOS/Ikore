//! The only HTTP client that ever carries a CLI token (ADR-028 G4, G6, G8): compile-time
//! HTTPS destinations, certificate validation, no redirects, bounded bodies and typed parsing.
//! Neither the body nor transport error text ever leaves this module.

use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;

use super::model::UnavailableReason;

pub const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Every destination a token may be sent to.
pub const PINNED_URLS: [&str; 2] = [CLAUDE_USAGE_URL, CODEX_USAGE_URL];
/// Honest identification (G9): the app names itself, never another client.
pub const USER_AGENT: &str = concat!("hard-vibe-code/", env!("CARGO_PKG_VERSION"));

const MAX_BODY_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    Unavailable(UnavailableReason),
    /// `429` or `403`: not retried before `retry_after` (or the backoff cap).
    Throttled {
        reason: UnavailableReason,
        retry_after: Option<Duration>,
    },
}

impl EndpointError {
    pub fn reason(self) -> UnavailableReason {
        match self {
            Self::Unavailable(reason) | Self::Throttled { reason, .. } => reason,
        }
    }
}

pub struct QuotaHttp {
    client: Client,
    #[cfg(test)]
    origin_override: Option<String>,
}

impl Default for QuotaHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl QuotaHttp {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .redirect(Policy::none())
                .https_only(true)
                .timeout(REQUEST_TIMEOUT)
                .user_agent(USER_AGENT)
                .build()
                .unwrap_or_default(),
            #[cfg(test)]
            origin_override: None,
        }
    }

    /// Sends pinned requests to a local plain-HTTP test server instead of the provider.
    #[cfg(test)]
    pub fn for_test(origin: String) -> Self {
        Self {
            client: Client::builder()
                .redirect(Policy::none())
                .timeout(REQUEST_TIMEOUT)
                .user_agent(USER_AGENT)
                .build()
                .unwrap(),
            origin_override: Some(origin),
        }
    }

    /// GETs one of the pinned URLs. `url` must be one of `PINNED_URLS`; anything else is
    /// refused before a connection is made.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: &'static str,
        headers: HeaderMap,
    ) -> Result<T, EndpointError> {
        if !PINNED_URLS.contains(&url) {
            return Err(EndpointError::Unavailable(UnavailableReason::Disabled));
        }
        #[cfg(test)]
        let target = match &self.origin_override {
            Some(origin) => {
                let path = url.splitn(4, '/').nth(3).unwrap_or_default();
                format!("{origin}/{path}")
            }
            None => url.to_string(),
        };
        #[cfg(not(test))]
        let target = url.to_string();
        self.fetch(&target, headers).await
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        target: &str,
        headers: HeaderMap,
    ) -> Result<T, EndpointError> {
        let failed = EndpointError::Unavailable(UnavailableReason::SourceFailed);
        let mut response = self
            .client
            .get(target)
            .headers(headers)
            .send()
            .await
            .map_err(|_| failed)?;
        let status = response.status();
        if status != StatusCode::OK {
            let retry_after = retry_after(response.headers());
            return Err(match status {
                StatusCode::UNAUTHORIZED => {
                    EndpointError::Unavailable(UnavailableReason::SignInExpired)
                }
                StatusCode::TOO_MANY_REQUESTS => EndpointError::Throttled {
                    reason: UnavailableReason::RateLimited,
                    retry_after,
                },
                StatusCode::FORBIDDEN => EndpointError::Throttled {
                    reason: UnavailableReason::Refused,
                    retry_after,
                },
                status if status.is_redirection() => {
                    EndpointError::Unavailable(UnavailableReason::UnexpectedResponse)
                }
                _ => failed,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_BODY_BYTES as u64)
        {
            return Err(EndpointError::Unavailable(
                UnavailableReason::UnexpectedResponse,
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| failed)? {
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(EndpointError::Unavailable(
                    UnavailableReason::UnexpectedResponse,
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| EndpointError::Unavailable(UnavailableReason::UnexpectedResponse))
    }
}

/// Spike-only probe: status, content type and JSON body of a pinned URL on the real host.
#[cfg(test)]
pub(crate) async fn spike_probe(
    url: &'static str,
    headers: HeaderMap,
) -> (u16, Option<String>, Option<serde_json::Value>) {
    assert!(PINNED_URLS.contains(&url));
    let client = QuotaHttp::new().client;
    let Ok(response) = client.get(url).headers(headers).send().await else {
        return (0, None, None);
    };
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.bytes().await.ok();
    let json = body.and_then(|body| serde_json::from_slice(&body).ok());
    (status, content_type, json)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER))
}

#[cfg(test)]
pub(crate) mod test_server {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A minimal HTTP/1.1 responder that records each request's head and serves scripted
    /// replies in order.
    pub struct TestServer {
        pub origin: String,
        pub requests: Arc<Mutex<Vec<String>>>,
    }

    pub async fn serve(replies: Vec<String>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    let Ok(read) = socket.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        break;
                    }
                    head.extend_from_slice(&buffer[..read]);
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).into_owned());
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        TestServer { origin, requests }
    }

    pub fn reply(status: &str, headers: &[&str], body: &str) -> String {
        let mut text = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for header in headers {
            text.push_str(header);
            text.push_str("\r\n");
        }
        text.push_str("\r\n");
        text.push_str(body);
        text
    }
}

#[cfg(test)]
mod tests {
    use super::test_server::{reply, serve};
    use super::*;

    #[derive(serde::Deserialize, Debug)]
    struct Probe {
        #[allow(dead_code)]
        ok: bool,
    }

    #[tokio::test]
    async fn quota_collectors_send_only_to_pinned_hosts_and_refuse_redirects() {
        for url in PINNED_URLS {
            assert!(
                url.starts_with("https://api.anthropic.com/")
                    || url.starts_with("https://chatgpt.com/"),
                "{url}"
            );
        }
        // A redirect is returned as a failure, never followed to its target.
        let elsewhere = serve(vec![reply("200 OK", &[], r#"{"ok":true}"#)]).await;
        let redirect = serve(vec![reply(
            "302 Found",
            &[&format!("location: {}/steal", elsewhere.origin)],
            "",
        )])
        .await;
        let http = QuotaHttp::for_test(redirect.origin.clone());
        let result = http
            .get_json::<Probe>(CLAUDE_USAGE_URL, HeaderMap::new())
            .await;
        assert_eq!(
            result.unwrap_err(),
            EndpointError::Unavailable(UnavailableReason::UnexpectedResponse)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(redirect.requests.lock().unwrap().len(), 1);
        assert!(elsewhere.requests.lock().unwrap().is_empty());
        let head = redirect.requests.lock().unwrap()[0].clone();
        assert!(head.starts_with("GET /api/oauth/usage "), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains(&format!("user-agent: {USER_AGENT}"))
        );

        // A URL that is not pinned is refused before any connection.
        let unpinned: &'static str = Box::leak(format!("{}/x", elsewhere.origin).into_boxed_str());
        assert_eq!(
            http.get_json::<Probe>(unpinned, HeaderMap::new())
                .await
                .unwrap_err(),
            EndpointError::Unavailable(UnavailableReason::Disabled)
        );
        // The production client refuses plain HTTP outright.
        let plain = serve(vec![reply("200 OK", &[], r#"{"ok":true}"#)]).await;
        assert!(
            QuotaHttp::new()
                .fetch::<Probe>(
                    &format!("{}/api/oauth/usage", plain.origin),
                    HeaderMap::new()
                )
                .await
                .is_err()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(plain.requests.lock().unwrap().is_empty());
        assert!(elsewhere.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn statuses_map_to_states_and_bodies_are_bounded() {
        let oversized = format!(r#"{{"ok":true,"pad":"{}"}}"#, "x".repeat(MAX_BODY_BYTES));
        let server = serve(vec![
            reply("401 Unauthorized", &[], "{}"),
            reply("429 Too Many Requests", &["retry-after: 120"], "{}"),
            reply("403 Forbidden", &[], "<html>blocked</html>"),
            reply("500 Internal Server Error", &[], "{}"),
            reply("200 OK", &[], &oversized),
            reply("200 OK", &[], r#"{"unexpected":1}"#),
            reply("200 OK", &[], r#"{"ok":true,"extra":"ignored"}"#),
        ])
        .await;
        let http = QuotaHttp::for_test(server.origin.clone());
        let mut results = Vec::new();
        for _ in 0..7 {
            results.push(
                http.get_json::<Probe>(CODEX_USAGE_URL, HeaderMap::new())
                    .await
                    .map(|_| ()),
            );
        }
        assert_eq!(
            results,
            vec![
                Err(EndpointError::Unavailable(UnavailableReason::SignInExpired)),
                Err(EndpointError::Throttled {
                    reason: UnavailableReason::RateLimited,
                    retry_after: Some(Duration::from_secs(120)),
                }),
                Err(EndpointError::Throttled {
                    reason: UnavailableReason::Refused,
                    retry_after: None,
                }),
                Err(EndpointError::Unavailable(UnavailableReason::SourceFailed)),
                Err(EndpointError::Unavailable(
                    UnavailableReason::UnexpectedResponse
                )),
                Err(EndpointError::Unavailable(
                    UnavailableReason::UnexpectedResponse
                )),
                Ok(()),
            ]
        );
    }
}
