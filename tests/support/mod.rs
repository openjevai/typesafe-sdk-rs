//! An in-process HTTP/1.1 mock server shared by the integration tests.
//!
//! Each accepted connection serves exactly one request and then closes, so retries always open a new
//! connection. A handler sees the request exactly as it arrived — headers lowercased — and decides how
//! to answer it; per-attempt behaviour is usually driven by the `x-typesafe-retry-count` header.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::Value;

/// A response the mock server can send.
#[derive(Clone, Debug)]
pub enum Reply {
    /// A JSON response with `content-type: application/json`.
    Json(u16, Value),
    /// A response with no body.
    Status(u16),
    /// A raw response body.
    Bytes(u16, Vec<u8>),
    /// A raw response body with additional headers.
    WithHeaders(u16, Vec<(String, String)>, Vec<u8>),
}

impl Reply {
    /// A JSON reply.
    pub fn json(status: u16, body: Value) -> Self {
        Self::Json(status, body)
    }

    /// A body-less reply.
    pub fn status(status: u16) -> Self {
        Self::Status(status)
    }

    /// A raw reply.
    pub fn bytes(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self::Bytes(status, body.into())
    }

    /// A raw reply with additional headers.
    pub fn with_headers(
        status: u16,
        headers: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        Self::WithHeaders(
            status,
            headers
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
            body.into(),
        )
    }

    /// The status code this reply sends.
    fn status_code(&self) -> u16 {
        match self {
            Self::Json(status, _)
            | Self::Status(status)
            | Self::Bytes(status, _)
            | Self::WithHeaders(status, _, _) => *status,
        }
    }

    /// The headers this reply sends, on top of the defaults.
    fn extra_headers(&self) -> &[(String, String)] {
        match self {
            Self::WithHeaders(_, headers, _) => headers,
            _ => &[],
        }
    }

    /// The body this reply sends.
    fn body(&self) -> Vec<u8> {
        match self {
            Self::Json(_, body) => serde_json::to_vec(body).unwrap(),
            Self::Status(_) => Vec::new(),
            Self::Bytes(_, body) | Self::WithHeaders(_, _, body) => body.clone(),
        }
    }

    /// Whether the reply declares a JSON content type.
    fn is_json(&self) -> bool {
        matches!(self, Self::Json(_, _))
    }
}

/// How the server answers one request.
#[derive(Clone, Debug)]
pub enum Action {
    /// Answer immediately.
    Reply(Reply),
    /// Close the connection without answering, which the client sees as a transport failure.
    Close,
    /// Wait, then answer.
    Delay(Duration, Reply),
}

impl Action {
    /// Answers with JSON.
    pub fn json(status: u16, body: Value) -> Self {
        Self::Reply(Reply::Json(status, body))
    }

    /// Closes the connection without answering.
    pub fn close() -> Self {
        Self::Close
    }

    /// Waits, then answers.
    pub fn delay(delay: Duration, reply: Reply) -> Self {
        Self::Delay(delay, reply)
    }
}

/// A request the server received.
#[derive(Clone, Debug)]
pub struct CapturedRequest {
    /// HTTP method, as sent.
    pub method: String,
    /// Request target, including any query string.
    pub path: String,
    /// Request headers, with lowercased names.
    pub headers: BTreeMap<String, String>,
    /// Request body.
    pub body: Vec<u8>,
}

impl CapturedRequest {
    /// Parses the request body as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("the request body should be JSON")
    }

    /// Returns a header value, looked up case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Returns the retry count this attempt reported, `None` for the first attempt.
    pub fn retry_count(&self) -> Option<&str> {
        self.header("x-typesafe-retry-count")
    }
}

/// A mock server bound to a loopback port.
pub struct MockServer {
    /// Address the server listens on.
    address: SocketAddr,
    /// Requests received so far, in arrival order.
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl MockServer {
    /// Starts a server that answers each request with `handler`.
    pub fn start(handler: impl Fn(&CapturedRequest) -> Action + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port is available");
        let address = listener
            .local_addr()
            .expect("the listener has a local address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let collected = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let handler = Arc::clone(&handler);
                let collected = Arc::clone(&collected);
                thread::spawn(move || serve(stream, handler.as_ref(), &collected));
            }
        });
        Self { address, requests }
    }

    /// Returns the base URL to configure a client with.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Returns every request received so far.
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.requests
            .lock()
            .expect("the capture lock is not poisoned")
            .clone()
    }

    /// Returns the number of requests received so far.
    pub fn count(&self) -> usize {
        self.requests
            .lock()
            .expect("the capture lock is not poisoned")
            .len()
    }

    /// Returns the request at `index`, panicking when it was never received.
    pub fn request(&self, index: usize) -> CapturedRequest {
        self.requests().get(index).cloned().unwrap_or_else(|| {
            panic!(
                "expected at least {} requests, saw {}",
                index + 1,
                self.count()
            )
        })
    }
}

/// Serves one request on one connection, then closes it.
fn serve(
    mut stream: TcpStream,
    handler: &(dyn Fn(&CapturedRequest) -> Action + Send + Sync),
    collected: &Mutex<Vec<CapturedRequest>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("the stream accepts a read timeout");
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    collected
        .lock()
        .expect("the capture lock is not poisoned")
        .push(request.clone());
    match handler(&request) {
        Action::Close => {}
        Action::Reply(reply) => write_reply(&mut stream, &reply),
        Action::Delay(delay, reply) => {
            thread::sleep(delay);
            write_reply(&mut stream, &reply);
        }
    }
}

/// Reads one HTTP/1.1 request from `stream`.
fn read_request(stream: &mut TcpStream) -> Option<CapturedRequest> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        match headers.get_mut(&name) {
            Some(existing) => {
                existing.push_str(", ");
                existing.push_str(&value);
            }
            None => {
                headers.insert(name, value);
            }
        }
    }

    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(CapturedRequest {
        method,
        path,
        headers,
        body,
    })
}

/// Writes one response, closing the connection afterwards.
fn write_reply(stream: &mut TcpStream, reply: &Reply) {
    let body = reply.body();
    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        reply.status_code(),
        reason(reply.status_code())
    );
    if reply.is_json() {
        head.push_str("content-type: application/json\r\n");
    }
    for (name, value) in reply.extra_headers() {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    ));
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

/// Returns the reason phrase for a status code.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// The reference System One response body, mirroring the Python SDK's test fixture.
pub fn system_one_body() -> Value {
    serde_json::json!({
        "model": "jev-latest",
        "usage": {"input_tokens": 12, "output_tokens": 3},
        "answers": {
            "spam": {"type": "noul", "noul": 0.98},
            "tone": {"type": "choice", "choice": "friendly", "confidence": 0.9, "probabilities": {"friendly": 0.9, "hostile": 0.1}},
            "quality": {
                "type": "score",
                "score": 1.7,
                "confidence": 0.8,
                "legend": {"0": "bad", "1": "ok", "2": "great"},
                "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8},
            },
        },
    })
}

/// The reference model card, mirroring the Python SDK's test fixture.
pub fn model_card() -> Value {
    serde_json::json!({"name": "jev-latest", "description": "Fast model", "release_date": "2026-08-01"})
}

/// Builds an async client bound to `server` with a deterministic, retry-free policy.
pub fn client(server: &MockServer) -> typesafe_sdk::Result<typesafe_sdk::TypeSafeClient> {
    typesafe_sdk::TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .build()
}
