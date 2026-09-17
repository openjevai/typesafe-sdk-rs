//! Log output: captured levels, credential redaction, and the retry/transport/warning records.

mod support;

use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::json;
use support::{Action, CapturedRequest, MockServer, Reply, model_card, system_one_body};
use tokio::sync::Mutex as AsyncMutex;
use typesafe_sdk::{Answer, Error, Noul, RetryPolicy, TypeSafeClient};

/// Log target every record emitted by the SDK carries.
const TARGET: &str = "typesafe_sdk";

/// A key whose value must never reach a log record.
const API_KEY: &str = "auth-credential";

/// Header names whose values must be hidden from log output.
const SECRET_HEADERS: [&str; 9] = [
    "Authorization",
    "Proxy-Authorization",
    "X-API-Key",
    "API-Key",
    "Cookie",
    "Set-Cookie",
    "X-Access-Token",
    "X-Client-Secret",
    "x-MiXeD-ToKeN",
];

/// Credential sent as a request header by the redaction test.
const REQUEST_CREDENTIAL: &str = "request-credential";

/// Credential echoed back as a response header by the redaction test.
const RESPONSE_CREDENTIAL: &str = "response-credential";

/// Non-secret request header value that must survive redaction, proving values are logged verbatim.
const REQUEST_VISIBLE: &str = "request-visible";

/// The kind names the SDK uses when it logs a transport failure.
const TRANSPORT_KINDS: [&str; 7] = [
    "Timeout", "Connect", "Body", "Decode", "Redirect", "Request", "Error",
];

/// Every log record the SDK emitted, in emission order.
#[derive(Debug, Default)]
struct Capture {
    /// Rendered messages.
    lines: Mutex<Vec<String>>,
    /// Levels of the messages in [`Capture::lines`], position for position.
    levels: Mutex<Vec<log::Level>>,
}

impl Capture {
    /// Forgets every captured record.
    fn clear(&self) {
        let mut lines = self.lines.lock().expect("the capture lock is not poisoned");
        let mut levels = self
            .levels
            .lock()
            .expect("the capture lock is not poisoned");
        lines.clear();
        levels.clear();
    }

    /// Appends one record.
    fn push(&self, level: log::Level, message: String) {
        let mut lines = self.lines.lock().expect("the capture lock is not poisoned");
        let mut levels = self
            .levels
            .lock()
            .expect("the capture lock is not poisoned");
        lines.push(message);
        levels.push(level);
    }

    /// Returns every captured message.
    fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .expect("the capture lock is not poisoned")
            .clone()
    }

    /// Returns every captured record as a `(level, message)` pair.
    fn records(&self) -> Vec<(log::Level, String)> {
        let lines = self.lines.lock().expect("the capture lock is not poisoned");
        let levels = self
            .levels
            .lock()
            .expect("the capture lock is not poisoned");
        levels.iter().copied().zip(lines.iter().cloned()).collect()
    }
}

/// The record buffer shared by the logger and the tests.
///
/// `log` holds one logger per process and only exposes `set_boxed_logger` with its `alloc` feature
/// enabled, which this dependency graph does not turn on, so the static logger is registered with
/// `log::set_logger` instead. A logger installed by something else is tolerated.
static CAPTURE: LazyLock<Arc<Capture>> = LazyLock::new(|| {
    let capture = Arc::new(Capture::default());
    let _ = log::set_logger(&LOGGER);
    capture
});

/// A `log` logger that appends SDK records to the buffer.
#[derive(Debug)]
struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == TARGET && metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            CAPTURE.push(record.level(), record.args().to_string());
        }
    }

    fn flush(&self) {}
}

/// The logger `log` keeps for the rest of the process.
static LOGGER: CaptureLogger = CaptureLogger;

/// Returns the shared record buffer, registering the logger on first use.
fn capture() -> Arc<Capture> {
    Arc::clone(&CAPTURE)
}

/// Serializes the tests in this file, which share one process-wide logger and one buffer.
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

/// Builds a client with an explicit key, a fast retry policy, and optional default headers.
fn logging_client(
    server: &MockServer,
    api_key: &str,
    retry: RetryPolicy,
    headers: &[(&str, &str)],
) -> TypeSafeClient {
    let mut builder = TypeSafeClient::builder()
        .api_key(api_key)
        .base_url(server.base_url())
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .retry(retry);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.build().expect("the client builds")
}

/// A policy whose retries happen immediately, so the tests stay fast.
fn fast_retry() -> RetryPolicy {
    RetryPolicy::new()
        .with_backoff_initial(Duration::from_millis(1))
        .with_backoff_max(Duration::from_millis(1))
}

/// A server that echoes `name` back as a secret response header and answers with `status`.
fn redacting_server(
    name: &'static str,
    status: u16,
) -> impl Fn(&CapturedRequest) -> Action + Send + Sync + 'static {
    let body = match status {
        200 => serde_json::to_vec(&system_one_body()),
        _ => serde_json::to_vec(&json!({"message": "rejected"})),
    }
    .expect("the reply body serializes");
    let reply = Action::Reply(Reply::with_headers(
        status,
        [
            ("content-type", "application/json"),
            (name, RESPONSE_CREDENTIAL),
            ("x-visible", "response-visible"),
            ("x-typesafe-request-id", "req-log"),
        ],
        body,
    ));
    move |_| reply.clone()
}

/// Asserts that visible headers are logged verbatim while every credential stays hidden.
fn assert_credentials_redacted(lines: &[String], name: &str) {
    let output = lines.join("\n");
    let lowered = name.to_ascii_lowercase();
    for visible in [REQUEST_VISIBLE, "response-visible"] {
        assert!(
            output.contains(visible),
            "{name}: the visible header value {visible} should be logged: {output}"
        );
    }
    assert!(
        output.contains("***"),
        "{name}: redaction should be visible in the output: {output}"
    );
    assert!(
        output.contains(&format!("{lowered}: ***")),
        "{name}: the secret header should be logged, redacted: {output}"
    );
    for secret in [REQUEST_CREDENTIAL, RESPONSE_CREDENTIAL, API_KEY] {
        assert!(
            !output.contains(secret),
            "{name}: {secret} leaked into the log output: {output}"
        );
    }
}

/// Returns whether `line` is the info record naming a transport failure.
fn is_transport_failure(line: &str) -> bool {
    line.contains(" <- ") && TRANSPORT_KINDS.iter().any(|kind| line.ends_with(kind))
}

#[tokio::test]
async fn credentials_are_redacted_on_success_and_error() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    log::set_max_level(log::LevelFilter::Debug);

    for name in SECRET_HEADERS {
        for status in [200, 400] {
            capture.clear();
            let server = MockServer::start(redacting_server(name, status));
            let client = logging_client(
                &server,
                API_KEY,
                fast_retry(),
                &[
                    (name, REQUEST_CREDENTIAL),
                    ("X-Visible-Request", REQUEST_VISIBLE),
                ],
            );
            let outcome = client
                .system_one()
                .state("I was charged twice.")
                .question("spam", Noul::new())
                .send()
                .await;

            if status == 200 {
                assert_eq!(outcome.expect("the 200 reply decodes").model, "jev-latest");
            } else {
                let error = outcome.expect_err("a 400 reply is an error");
                assert!(matches!(error, Error::BadRequest(_)), "{name}: {error:?}");
            }
            assert_credentials_redacted(&capture.lines(), name);
        }
    }
}

#[tokio::test]
async fn levels_filter_what_is_captured() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    let reply = Action::Reply(Reply::with_headers(
        200,
        [
            ("content-type", "application/json"),
            ("x-typesafe-request-id", "req-log"),
            ("x-visible", "response-visible"),
        ],
        serde_json::to_vec(&json!({"models": [model_card()]})).expect("the reply body serializes"),
    ));
    let server = MockServer::start(move |_| reply.clone());
    let client = logging_client(&server, API_KEY, fast_retry(), &[]);

    capture.clear();
    log::set_max_level(log::LevelFilter::Info);
    client
        .models()
        .list()
        .send()
        .await
        .expect("the models reply decodes");

    let lines = capture.lines();
    let summaries = lines
        .iter()
        .filter(|line| line.contains("GET") && line.contains("<- 200"))
        .collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        1,
        "one call should produce one summary line: {lines:?}"
    );
    assert!(
        summaries[0].contains("(request req-log)"),
        "{}",
        summaries[0]
    );
    assert!(
        lines.iter().all(|line| !line.contains("headers=")),
        "info level should not capture the debug header lines: {lines:?}"
    );
    assert!(
        capture
            .records()
            .iter()
            .all(|(level, _)| *level == log::Level::Info),
        "info level should capture no record below info"
    );

    capture.clear();
    log::set_max_level(log::LevelFilter::Debug);
    client
        .models()
        .list()
        .send()
        .await
        .expect("the models reply decodes");

    let lines = capture.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("-> headers="))
            .count(),
        1,
        "the outgoing request should be logged with its headers: {lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("<- headers="))
            .count(),
        1,
        "the incoming reply should be logged with its headers: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("x-visible") && line.contains("response-visible")),
        "visible response headers should be logged: {lines:?}"
    );
    assert!(
        capture
            .records()
            .iter()
            .any(|(level, _)| *level == log::Level::Debug),
        "debug level should capture debug records"
    );
}

#[tokio::test]
async fn retries_are_logged_with_their_progress_header() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    capture.clear();
    log::set_max_level(log::LevelFilter::Debug);

    let busy = Action::Reply(Reply::with_headers(
        429,
        [
            ("content-type", "application/json"),
            ("retry-after-ms", "0"),
        ],
        serde_json::to_vec(&json!({"message": "slow down"})).expect("the reply body serializes"),
    ));
    let ok = Action::json(200, system_one_body());
    let server = MockServer::start(move |request| match request.retry_count() {
        Some("2") => ok.clone(),
        _ => busy.clone(),
    });
    let client = logging_client(&server, API_KEY, fast_retry().with_max_retries(2), &[]);

    client
        .system_one()
        .state("I was charged twice.")
        .question("spam", Noul::new())
        .send()
        .await
        .expect("the third attempt succeeds");

    assert_eq!(server.count(), 3);
    assert_eq!(server.request(0).retry_count(), None);
    assert_eq!(server.request(1).retry_count(), Some("1"));
    assert_eq!(server.request(2).retry_count(), Some("2"));

    let lines = capture.lines();
    assert!(
        lines.iter().any(|line| line.contains("retry 1")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("retry 2")),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("retry 3")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn unknown_answers_are_warned_about() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    capture.clear();
    log::set_max_level(log::LevelFilter::Debug);

    let server = MockServer::start(|_| {
        Action::json(
            200,
            json!({
                "model": "jev-latest",
                "usage": {},
                "answers": {"mystery": {"type": "aurora"}},
            }),
        )
    });
    let client = logging_client(&server, API_KEY, fast_retry(), &[]);
    let response = client
        .system_one()
        .state("I was charged twice.")
        .question("mystery", Noul::new())
        .send()
        .await
        .expect("the reply decodes");

    assert!(
        matches!(response.answer("mystery"), Some(Answer::Unknown(unknown)) if unknown.kind == "aurora")
    );

    let warnings = capture
        .records()
        .into_iter()
        .filter(|(level, _)| *level == log::Level::Warn)
        .map(|(_, line)| line)
        .collect::<Vec<_>>();
    assert_eq!(
        warnings.len(),
        1,
        "one unknown answer should warn once: {warnings:?}"
    );
    assert!(warnings[0].contains("aurora"), "{}", warnings[0]);
    assert!(warnings[0].contains("mystery"), "{}", warnings[0]);
}

#[tokio::test]
async fn transport_failures_are_logged_without_credentials() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    capture.clear();
    log::set_max_level(log::LevelFilter::Debug);

    let ok = Action::json(200, system_one_body());
    let server = MockServer::start(move |request| match request.retry_count() {
        None => Action::close(),
        Some(_) => ok.clone(),
    });
    let client = logging_client(&server, API_KEY, fast_retry().with_max_retries(1), &[]);

    let response = client
        .system_one()
        .state("I was charged twice.")
        .question("spam", Noul::new())
        .send()
        .await
        .expect("the second attempt succeeds");

    assert_eq!(response.model, "jev-latest");
    assert_eq!(server.count(), 2);
    assert_eq!(server.request(1).retry_count(), Some("1"));

    let lines = capture.lines();
    assert!(
        lines.iter().any(|line| line.contains("<- 200")),
        "the recovered attempt should be summarised: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| is_transport_failure(line)),
        "the dropped connection should be logged with its kind: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains(API_KEY)),
        "a transport failure must not log the key: {lines:?}"
    );
}

#[tokio::test]
async fn warning_level_silences_successful_calls_but_keeps_warnings() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    capture.clear();
    log::set_max_level(log::LevelFilter::Warn);

    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let client = logging_client(&server, API_KEY, fast_retry(), &[]);
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .expect("the call succeeds");
    assert!(
        capture.records().is_empty(),
        "a successful call logs nothing at WARNING: {:?}",
        capture.lines()
    );

    let server = MockServer::start(|_| {
        Action::json(
            200,
            json!({"model": "m", "usage": {}, "answers": {"mystery": {"type": "aurora"}}}),
        )
    });
    let client = logging_client(&server, API_KEY, fast_retry(), &[]);
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .expect("the call succeeds");
    let records = capture.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].0, log::Level::Warn);

    log::set_max_level(log::LevelFilter::Debug);
}

#[tokio::test]
async fn request_and_response_bodies_are_logged_at_debug() {
    let _serial = SERIAL.lock().await;
    let capture = capture();
    capture.clear();
    log::set_max_level(log::LevelFilter::Debug);

    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let client = logging_client(&server, API_KEY, fast_retry(), &[]);
    client
        .system_one()
        .state("hello 🌍")
        .question("q", Noul::new())
        .send()
        .await
        .expect("the call succeeds");

    let output = capture.lines().join("\n");
    assert!(
        output.contains("hello 🌍"),
        "the request body is logged: {output}"
    );
    assert!(output.contains("-> headers="), "{output}");
    assert!(output.contains("<- headers="), "{output}");
    assert!(
        output.contains("jev-latest"),
        "the response body is logged: {output}"
    );
}
