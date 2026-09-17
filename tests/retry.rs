//! Retry behaviour: status policies, attempt counts, delays, budgets, and per-call overrides.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use support::{Action, MockServer, Reply, system_one_body};
use typesafe_sdk::{Error, Noul, RetryPolicy, StatusCode, SystemOneResponse, TypeSafeClient};

/// Builds a client with the given policy and fast backoff.
fn client_with(server: &MockServer, retry: RetryPolicy) -> TypeSafeClient {
    TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .retry(retry)
        .build()
        .unwrap()
}

/// A policy with no server delay and instant backoff, so retries are observable and fast.
fn instant(max_retries: u32) -> RetryPolicy {
    RetryPolicy::new()
        .with_max_retries(max_retries)
        .with_backoff_initial(Duration::from_millis(1))
        .with_backoff_max(Duration::from_millis(1))
}

/// Calls System One, discarding the response.
async fn call(client: &TypeSafeClient) -> Result<SystemOneResponse, Error> {
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
}

/// Returns the retry-count header each request carried.
fn retry_counts(server: &MockServer) -> Vec<Option<String>> {
    server
        .requests()
        .iter()
        .map(|request| request.retry_count().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn default_policies_retry_the_expected_statuses() {
    let cases = [
        (408, 3),
        (429, 3),
        (500, 3),
        (503, 3),
        (599, 3),
        (400, 1),
        (401, 1),
        (403, 1),
        (404, 1),
        (409, 1),
        (422, 1),
        (302, 1),
    ];
    for (status, expected) in cases {
        let server = MockServer::start(move |_| {
            Action::Reply(Reply::with_headers(
                status,
                [("retry-after-ms", "0")],
                br#"{"message":"failed"}"#.to_vec(),
            ))
        });
        let error = call(&client_with(&server, RetryPolicy::new()))
            .await
            .expect_err("every attempt fails");
        assert_eq!(
            error.status(),
            Some(StatusCode::from_u16(status).unwrap()),
            "{status}"
        );
        assert_eq!(server.count(), expected, "{status}");
        assert_eq!(
            retry_counts(&server),
            [None, Some("1".to_owned()), Some("2".to_owned())][..expected].to_vec(),
            "{status}"
        );
    }
}

#[tokio::test]
async fn max_retries_caps_the_attempt_count() {
    for (max_retries, expected) in [(0, 1), (1, 2), (4, 5)] {
        let server = MockServer::start(|_| {
            Action::Reply(Reply::with_headers(
                429,
                [("retry-after-ms", "0")],
                Vec::new(),
            ))
        });
        call(&client_with(&server, instant(max_retries)))
            .await
            .expect_err("every attempt fails");
        assert_eq!(server.count(), expected, "max_retries {max_retries}");
    }
}

#[tokio::test]
async fn a_recovered_call_returns_the_successful_response() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start(move |_| {
        if attempts.fetch_add(1, Ordering::SeqCst) < 2 {
            Action::Reply(Reply::with_headers(
                503,
                [("retry-after-ms", "0")],
                br#"{"message":"unavailable"}"#.to_vec(),
            ))
        } else {
            Action::json(200, system_one_body())
        }
    });
    let response = call(&client_with(&server, instant(2))).await.unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(server.count(), 3);
    assert_eq!(
        retry_counts(&server),
        vec![None, Some("1".to_owned()), Some("2".to_owned())]
    );
}

#[tokio::test]
async fn custom_statuses_and_predicates_opt_extra_failures_in() {
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            409,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    call(&client_with(
        &server,
        instant(2).with_retry_statuses([StatusCode::CONFLICT]),
    ))
    .await
    .expect_err("every attempt fails");
    assert_eq!(server.count(), 3, "409 is retried when configured");

    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            500,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    call(&client_with(
        &server,
        instant(2).with_retry_statuses([StatusCode::CONFLICT]),
    ))
    .await
    .expect_err("every attempt fails");
    assert_eq!(
        server.count(),
        1,
        "500 is not retried when it is not configured"
    );

    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            404,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    let policy = instant(1).retry_if(|error| error.status() == Some(StatusCode::NOT_FOUND));
    call(&client_with(&server, policy))
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count(), 2, "the predicate opts a 404 in");

    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            404,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    call(&client_with(&server, instant(1)))
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count(), 1, "a 404 is not retried by default");
}

#[tokio::test]
async fn connection_failures_are_retried() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server = MockServer::start(move |_| {
        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            Action::close()
        } else {
            Action::json(200, system_one_body())
        }
    });
    let response = call(&client_with(&server, instant(2))).await.unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(server.count(), 2);

    let server = MockServer::start(|_| Action::close());
    let error = call(&client_with(&server, instant(1)))
        .await
        .expect_err("every attempt fails");
    assert!(matches!(error, Error::Connection(_)), "{error:?}");
    assert_eq!(server.count(), 2);

    let server = MockServer::start(|_| Action::close());
    let policy = instant(1).with_retry_connection_errors(false);
    let error = call(&client_with(&server, policy))
        .await
        .expect_err("every attempt fails");
    assert!(matches!(error, Error::Connection(_)), "{error:?}");
    assert_eq!(server.count(), 1, "connection retries can be disabled");
}

#[tokio::test]
async fn server_requested_delays_are_honoured() {
    for header in ["retry-after-ms", "retry-after"] {
        let value = if header == "retry-after-ms" {
            "50"
        } else {
            "0.05"
        };
        let server = MockServer::start(move |request| match request.retry_count() {
            None => Action::Reply(Reply::with_headers(429, [(header, value)], Vec::new())),
            _ => Action::json(200, system_one_body()),
        });
        let started = Instant::now();
        call(&client_with(&server, RetryPolicy::new().with_budget(None)))
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(45),
            "{header}: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(300),
            "{header}: {elapsed:?} — the server delay must win over the 500 ms default backoff"
        );
    }

    let server = MockServer::start(|request| match request.retry_count() {
        None => Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "0")],
            Vec::new(),
        )),
        _ => Action::json(200, system_one_body()),
    });
    // With a five second backoff configured, finishing within a second proves the header won.
    let policy = RetryPolicy::new()
        .with_budget(None)
        .with_backoff_initial(Duration::from_secs(5));
    let started = Instant::now();
    call(&client_with(&server, policy)).await.unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "an explicit zero delay is honoured instead of the backoff: {elapsed:?}"
    );

    let server = MockServer::start(|request| match request.retry_count() {
        None => Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "0")],
            Vec::new(),
        )),
        _ => Action::json(200, system_one_body()),
    });
    let policy = RetryPolicy::new()
        .with_budget(None)
        .with_backoff_initial(Duration::from_millis(1))
        .with_respect_retry_after(false);
    call(&client_with(&server, policy)).await.unwrap();
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn the_budget_stops_retries_before_the_delay() {
    let policy = RetryPolicy::new()
        .with_budget(Duration::from_millis(100))
        .with_backoff_initial(Duration::from_millis(50))
        .with_backoff_max(Duration::from_millis(50))
        .with_backoff_jitter(0.0)
        .with_respect_retry_after(false);
    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(40),
            Reply::bytes(429, br#"{"message":"slow"}"#.to_vec()),
        )
    });
    let started = Instant::now();
    let error = call(&client_with(&server, policy))
        .await
        .expect_err("every attempt fails");
    assert!(matches!(error, Error::RateLimit(_)), "{error:?}");
    assert_eq!(
        server.count(),
        2,
        "the third attempt would exceed the 100 ms budget"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn each_call_gets_a_fresh_budget() {
    let policy = RetryPolicy::new()
        .with_budget(Duration::from_millis(100))
        .with_backoff_initial(Duration::from_millis(50))
        .with_backoff_max(Duration::from_millis(50))
        .with_backoff_jitter(0.0)
        .with_respect_retry_after(false);
    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(40),
            Reply::bytes(429, br#"{"message":"slow"}"#.to_vec()),
        )
    });
    let client = client_with(&server, policy);
    for _ in 0..2 {
        let before = server.count();
        call(&client).await.expect_err("every attempt fails");
        assert_eq!(
            server.count() - before,
            2,
            "each call retries on its own budget"
        );
    }
}

#[tokio::test]
async fn per_call_policies_override_the_client_policy() {
    // The client retries, the call does not.
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    let client = client_with(&server, instant(2));
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .retry(instant(0))
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count(), 1);

    // The client does not retry, the call does.
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    let client = client_with(&server, instant(0));
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .retry(instant(2))
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count(), 3);

    // Passing None falls back to the client policy.
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    let client = client_with(&server, instant(1));
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .retry(None)
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count(), 2);

    // The models endpoint honours per-call policies too.
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            409,
            [("retry-after-ms", "0")],
            Vec::new(),
        ))
    });
    let client = client_with(&server, instant(0));
    client
        .models()
        .list()
        .retry(instant(2).with_retry_statuses([StatusCode::CONFLICT]))
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_keep_their_own_retry_state() {
    let server = MockServer::start(|request| {
        let call = request.header("x-call").unwrap_or("unknown").to_owned();
        match request.retry_count() {
            Some("1") => Action::json(
                200,
                json!({"model": format!("model-{call}"), "usage": {}, "answers": {}}),
            ),
            _ => Action::Reply(Reply::with_headers(
                429,
                [("retry-after-ms", "0"), ("x-call-seen", call.as_str())],
                br#"{"message":"retry"}"#.to_vec(),
            )),
        }
    });
    let client = client_with(&server, instant(0));

    /// Sends one call with its own policy, model, and headers.
    fn spawn_call(
        client: &TypeSafeClient,
        name: &'static str,
        max_retries: u32,
    ) -> tokio::task::JoinHandle<Result<SystemOneResponse, Error>> {
        let request = client
            .system_one()
            .state(name)
            .question("q", Noul::new())
            .model(name)
            .header("x-call", name)
            .retry(instant(max_retries))
            .timeout(Duration::from_secs(5));
        tokio::spawn(request.send())
    }

    let (one, two, three) = tokio::join!(
        spawn_call(&client, "one", 1),
        spawn_call(&client, "two", 2),
        spawn_call(&client, "three", 3)
    );
    for (name, response) in [("one", one), ("two", two), ("three", three)] {
        let response = response
            .expect("the task does not panic")
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            response.model,
            format!("model-{name}"),
            "{name} keeps its own reply"
        );
    }

    for name in ["one", "two", "three"] {
        let requests = server
            .requests()
            .into_iter()
            .filter(|request| request.header("x-call") == Some(name))
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 2, "{name}");
        assert_eq!(requests[0].retry_count(), None, "{name}");
        assert_eq!(requests[1].retry_count(), Some("1"), "{name}");
        for request in requests {
            assert_eq!(request.json()["state"], json!(name), "{name}");
            assert_eq!(request.json()["model"], json!(name), "{name}");
            assert_eq!(
                request.header("authorization"),
                Some("Bearer test-key"),
                "{name}"
            );
        }
    }
}

#[tokio::test]
async fn exhausted_timeout_retries_report_the_configured_timeout() {
    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(80),
            Reply::json(200, system_one_body()),
        )
    });
    let client = client_with(&server, instant(2));
    let started = Instant::now();
    let error = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .timeout(Duration::from_millis(20))
        .send()
        .await
        .expect_err("every attempt exceeds the per-call timeout");
    let Error::Timeout(timeout) = &error else {
        panic!("expected a timeout, got {error:?}");
    };
    assert_eq!(timeout.timeout(), Some(Duration::from_millis(20)));
    assert_eq!(
        server.count(),
        3,
        "timeouts are retried like other transport failures"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );

    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(80),
            Reply::json(200, system_one_body()),
        )
    });
    let client = client_with(&server, instant(2).with_retry_timeout_errors(false));
    let error = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .timeout(Duration::from_millis(20))
        .send()
        .await
        .expect_err("the attempt exceeds the per-call timeout");
    assert!(matches!(error, Error::Timeout(_)), "{error:?}");
    assert_eq!(server.count(), 1, "timeout retries can be disabled");
}

#[tokio::test]
async fn per_call_overrides_do_not_leak_into_the_next_call() {
    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let client = client_with(&server, instant(2));
    client
        .system_one()
        .state("first")
        .question("q", Noul::new())
        .model("call-model")
        .header("x-call", "one")
        .send()
        .await
        .unwrap();
    client
        .system_one()
        .state("second")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();

    let first = server.request(0);
    assert_eq!(first.json()["model"], json!("call-model"));
    assert_eq!(first.json()["state"], json!("first"));
    assert_eq!(first.header("x-call"), Some("one"));

    let second = server.request(1);
    assert_eq!(
        second.json()["model"],
        json!("test-model"),
        "the client model is back"
    );
    assert_eq!(second.json()["state"], json!("second"));
    assert_eq!(second.header("x-call"), None, "the per-call header is gone");
    assert_eq!(second.header("x-typesafe-retry-count"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_keep_their_own_timeouts() {
    let server = MockServer::start(|request| {
        let call = request.header("x-call").unwrap_or("unknown").to_owned();
        Action::delay(
            Duration::from_millis(80),
            Reply::json(
                200,
                json!({"model": format!("model-{call}"), "usage": {}, "answers": {}}),
            ),
        )
    });
    let client = client_with(&server, instant(0));

    /// Sends one call with its own timeout, so a short deadline cannot bleed into its siblings.
    fn spawn_call(
        client: &TypeSafeClient,
        name: &'static str,
        timeout: Duration,
    ) -> tokio::task::JoinHandle<Result<SystemOneResponse, Error>> {
        let request = client
            .system_one()
            .state(name)
            .question("q", Noul::new())
            .model(name)
            .header("x-call", name)
            .timeout(timeout);
        tokio::spawn(request.send())
    }

    let (short, first, second) = tokio::join!(
        spawn_call(&client, "short", Duration::from_millis(30)),
        spawn_call(&client, "first", Duration::from_secs(2)),
        spawn_call(&client, "second", Duration::from_secs(2)),
    );
    let error = short
        .expect("the task does not panic")
        .expect_err("the short deadline fires");
    assert!(matches!(error, Error::Timeout(_)), "{error:?}");
    assert_eq!(
        first.expect("the task does not panic").unwrap().model,
        "model-first"
    );
    assert_eq!(
        second.expect("the task does not panic").unwrap().model,
        "model-second"
    );
    for name in ["short", "first", "second"] {
        let attempts = server
            .requests()
            .into_iter()
            .filter(|request| request.header("x-call") == Some(name))
            .count();
        assert_eq!(attempts, 1, "{name}");
    }
}

#[tokio::test]
async fn aborting_a_parked_retry_stops_the_attempts() {
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "5000")],
            br#"{"message":"slow"}"#.to_vec(),
        ))
    });
    let client = client_with(
        &server,
        RetryPolicy::new().with_max_retries(3).with_budget(None),
    );
    let request = client.system_one().state("x").question("q", Noul::new());
    let task = tokio::spawn(request.send());

    // Wait until the attempt has been sent; the loop is then parked in its five second retry sleep.
    for _ in 0..200 {
        if server.count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        server.count(),
        1,
        "the first attempt is sent before the retry sleep"
    );
    task.abort();
    assert!(task.await.is_err(), "the task is cancelled");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        server.count(),
        1,
        "a cancelled call makes no further attempts"
    );
}

#[tokio::test]
async fn zero_backoff_bounds_still_retry() {
    let server = MockServer::start(|request| match request.retry_count() {
        Some("1") => Action::json(200, system_one_body()),
        _ => Action::json(503, json!({"message": "unavailable"})),
    });
    let policy = RetryPolicy::new()
        .with_max_retries(1)
        .with_backoff_initial(Duration::ZERO)
        .with_backoff_max(Duration::ZERO);
    let response = call(&client_with(&server, policy)).await.unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(retry_counts(&server), vec![None, Some("1".to_owned())]);
}

#[tokio::test]
async fn server_delays_apply_to_any_retryable_status() {
    // A 503 carrying `Retry-After` is honoured like a 429: zero means retry immediately, not after the
    // 500 ms backoff.
    let server = MockServer::start(|request| match request.retry_count() {
        None => Action::Reply(Reply::with_headers(
            503,
            [("Retry-After", "0")],
            br#"{"message":"down"}"#.to_vec(),
        )),
        _ => Action::json(200, system_one_body()),
    });
    let started = Instant::now();
    call(&client_with(&server, RetryPolicy::new().with_budget(None)))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );

    // An HTTP date in the past clamps to zero, so the retry is immediate instead of after the backoff.
    let past = "Sun, 06 Nov 1994 08:49:37 GMT";
    let server = MockServer::start(move |request| match request.retry_count() {
        None => Action::Reply(Reply::with_headers(
            429,
            [("Retry-After", past)],
            Vec::new(),
        )),
        _ => Action::json(200, system_one_body()),
    });
    let started = Instant::now();
    call(&client_with(&server, RetryPolicy::new().with_budget(None)))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn milliseconds_beat_seconds_when_both_headers_are_valid() {
    // The seconds header is a full second so the assertion has room: the call must finish long before
    // that delay could have elapsed, which proves `retry-after-ms` won rather than the backoff.
    let server = MockServer::start(|request| match request.retry_count() {
        None => Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "0"), ("Retry-After", "1")],
            Vec::new(),
        )),
        _ => Action::json(200, system_one_body()),
    });
    let started = Instant::now();
    call(&client_with(&server, RetryPolicy::new().with_budget(None)))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "the millisecond header wins over the one second delay: {elapsed:?}"
    );
}

#[tokio::test]
async fn the_budget_can_differ_per_call() {
    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(40),
            Reply::bytes(429, br#"{"message":"slow"}"#.to_vec()),
        )
    });
    let client = client_with(
        &server,
        RetryPolicy::new()
            .with_budget(Duration::from_secs(30))
            .with_backoff_jitter(0.0),
    );

    let per_call = |budget: Option<Duration>| {
        client
            .system_one()
            .state("x")
            .question("q", Noul::new())
            .retry(
                RetryPolicy::new()
                    .with_max_retries(5)
                    .with_budget(budget)
                    .with_respect_retry_after(false)
                    .with_backoff_jitter(0.0)
                    .with_backoff_initial(Duration::from_millis(50))
                    .with_backoff_max(Duration::from_millis(50)),
            )
    };

    // A 100 ms budget stops after the second attempt (40 ms + 50 ms + 40 ms > 100 ms).
    let before = server.count();
    per_call(Some(Duration::from_millis(100)))
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(server.count() - before, 2, "the per-call budget binds");

    // The client's 30 s budget allows every attempt the call policy permits.
    let before = server.count();
    per_call(None)
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(
        server.count() - before,
        6,
        "without a per-call budget the client budget applies"
    );
}

#[tokio::test]
async fn exhaustion_returns_the_last_attempts_error() {
    let server = MockServer::start(|request| {
        let attempt = request
            .retry_count()
            .map_or(1, |count| count.parse::<u32>().expect("a number") + 1);
        Action::Reply(Reply::with_headers(
            [429, 500, 503][(attempt - 1) as usize],
            [
                ("x-typesafe-request-id", format!("request-{attempt}")),
                ("retry-after-ms", "0".to_owned()),
            ],
            format!("{{\"message\":\"attempt {attempt}\"}}").into_bytes(),
        ))
    });
    let error = call(&client_with(
        &server,
        RetryPolicy::new().with_max_retries(2),
    ))
    .await
    .expect_err("every attempt fails");
    assert_eq!(server.count(), 3);
    assert_eq!(error.status(), Some(StatusCode::SERVICE_UNAVAILABLE));
    assert_eq!(error.body(), Some(&json!({"message": "attempt 3"})));
    assert_eq!(error.request_id(), Some("request-3"));
    assert_eq!(
        error.to_string(),
        format!(
            "POST {}/v1/systemone: 503 attempt 3 (request_id=request-3)",
            server.base_url()
        )
    );
}

#[tokio::test]
async fn transport_failures_recover_into_server_delays() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let server = MockServer::start(move |_| match counter.fetch_add(1, Ordering::SeqCst) {
        0 => Action::close(),
        1 => Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "125")],
            br#"{"message":"slow down"}"#.to_vec(),
        )),
        _ => Action::json(200, system_one_body()),
    });
    let client = client_with(
        &server,
        RetryPolicy::new().with_max_retries(2).with_budget(None),
    );
    let started = Instant::now();
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .header("x-call", "recover")
        .send()
        .await
        .expect("the third attempt succeeds");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(120),
        "the server delay is honoured: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");
    assert_eq!(
        retry_counts(&server),
        vec![None, Some("1".to_owned()), Some("2".to_owned())]
    );
    for request in server.requests() {
        assert_eq!(request.header("authorization"), Some("Bearer test-key"));
        assert_eq!(request.json()["state"], json!("x"));
    }
}
