use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ScriptedResponse {
    status: u16,
    content_type: String,
    chunks: Vec<Vec<u8>>,
}

impl ScriptedResponse {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "application/json".into(),
            chunks: vec![body.into().into_bytes()],
        }
    }

    pub fn sse(events: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream".into(),
            chunks: events
                .into_iter()
                .map(|event| event.into().into_bytes())
                .collect(),
        }
    }
}

pub struct ScriptedHttpServer {
    address: SocketAddr,
    requests: Arc<(Mutex<Vec<RecordedRequest>>, Condvar)>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ScriptedHttpServer {
    pub fn start(responses: impl IntoIterator<Item = ScriptedResponse>) -> Self {
        let mut responses = responses.into_iter().collect::<VecDeque<_>>();
        Self::respond_with(move |_| {
            responses
                .pop_front()
                .unwrap_or_else(|| ScriptedResponse::json(500, r#"{"error":"fixture exhausted"}"#))
        })
    }

    /// Loopback-only deterministic provider. The handler must be bounded; Drop joins it.
    pub fn respond_with(
        mut handler: impl FnMut(&RecordedRequest) -> ScriptedResponse + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("scripted HTTP fixture should bind to loopback");
        let address = listener
            .local_addr()
            .expect("scripted HTTP fixture address should resolve");
        let requests = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
        let worker_requests = Arc::clone(&requests);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("ikore-http-fixture".into())
            .spawn(move || {
                while let Ok((mut stream, _)) = listener.accept() {
                    if worker_shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let request = read_request(&mut stream);
                    let (lock, changed) = &*worker_requests;
                    lock.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(request.clone());
                    changed.notify_all();
                    let response = handler(&request);
                    let _ = write_response(&mut stream, response);
                }
            })
            .expect("scripted HTTP fixture thread should start");
        Self {
            address,
            requests,
            shutdown,
            worker: Some(worker),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, normalize_path(path))
    }

    pub fn wait_for_requests(&self, expected: usize, timeout: Duration) -> Vec<RecordedRequest> {
        let (lock, changed) = &*self.requests;
        let requests = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (requests, _) = changed
            .wait_timeout_while(requests, timeout, |requests| requests.len() < expected)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        requests.clone()
    }
}

impl Drop for ScriptedHttpServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

fn read_request(stream: &mut TcpStream) -> RecordedRequest {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                bytes.extend_from_slice(&buffer[..count]);
                if request_is_complete(&bytes) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    parse_request(&bytes)
}

fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = find_header_end(bytes) else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or_default();
    bytes.len() >= header_end + 4 + content_length
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_request(bytes: &[u8]) -> RecordedRequest {
    let header_end = find_header_end(bytes).unwrap_or(bytes.len());
    let header_text = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = header_text.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let value = if is_sensitive_header(name) {
                "[REDACTED]".to_string()
            } else {
                value.trim().to_string()
            };
            Some((name.trim().to_ascii_lowercase(), value))
        })
        .collect();
    let body_start = header_end.saturating_add(4).min(bytes.len());
    let body = String::from_utf8_lossy(&bytes[body_start..]).into_owned();
    RecordedRequest {
        method,
        path,
        headers,
        body,
    }
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie" | "x-api-key"
    )
}

fn write_response(stream: &mut TcpStream, response: ScriptedResponse) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        response.status,
        reason_phrase(response.status),
        response.content_type
    )?;
    for chunk in response.chunks {
        write!(stream, "{:X}\r\n", chunk.len())?;
        stream.write_all(&chunk)?;
        stream.write_all(b"\r\n")?;
        stream.flush()?;
    }
    stream.write_all(b"0\r\n\r\n")?;
    stream.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Fixture Response",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_chunked_fixture_and_redacts_sensitive_headers() {
        let server = ScriptedHttpServer::start([ScriptedResponse::json(200, r#"{"ok":true}"#)]);
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream
            .write_all(
                b"POST /v1/test HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture-secret\r\nContent-Length: 2\r\n\r\n{}",
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        let requests = server.wait_for_requests(1, Duration::from_secs(1));
        assert!(response.contains("200 OK"));
        assert_eq!(requests[0].method, "POST");
        assert_eq!(requests[0].path, "/v1/test");
        assert_eq!(requests[0].body, "{}");
        let rendered = format!("{:?}", requests[0]);
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains("fixture-secret"));
    }
}
