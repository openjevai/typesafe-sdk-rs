//! Blocking client parity: request shapes, retry accounting, error mapping, and builder interop.

#![cfg(feature = "blocking")]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use support::{Action, MockServer, model_card, system_one_body};
use typesafe_sdk::blocking::TypeSafeClient;
use typesafe_sdk::{Choice, Error, HeaderMap, Noul, RetryPolicy, StatusCode};

use crate::support::Reply;

/// A blocking client aimed at `server` that never retries, so one call means one request.
fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .retry(RetryPolicy::new().with_max_retries(0))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

/// Sends one System One request and returns the error it produced.
fn error_from(server: &MockServer) -> Error {
    client(server)
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .unwrap_err()
}

#[test]
fn happy_path_returns_a_decoded_response() {
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            200,
            [("x-typesafe-request-id", "req-blocking-1")],
            serde_json::to_vec(&system_one_body()).unwrap(),
        ))
    });
    let response = client(&server)
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .unwrap();

    assert_eq!(response.model, "jev-latest");
    assert_eq!(response.noul("spam").unwrap().noul, 0.98);
    assert_eq!(response.choice("tone").unwrap().choice, "friendly");
    assert_eq!(response.request_id(), Some("req-blocking-1"));
    assert_eq!(response.status(), StatusCode::OK);

    let captured = server.request(0);
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/v1/systemone");
    assert_eq!(captured.header("authorization"), Some("Bearer test-key"));
    assert_eq!(
        captured.header("x-typesafe-sdk"),
        Some(format!("typesafe-sdk/{}", typesafe_sdk::VERSION).as_str())
    );
    assert_eq!(
        captured.header("user-agent"),
        Some(format!("typesafe-sdk/{}", typesafe_sdk::VERSION).as_str())
    );
    assert_eq!(captured.header("accept"), Some("application/json"));
    assert_eq!(captured.header("content-type"), Some("application/json"));
    assert_eq!(
        captured.json(),
        json!({"state": "x", "model": "test-model", "questions": {"q": {"type": "noul"}}})
    );
}

#[test]
fn every_request_setter_reaches_the_wire() {
    let server = MockServer::start(|request| {
        if request.path == "/v1/models" {
            Action::json(200, json!({"models": [model_card()]}))
        } else {
            Action::json(200, system_one_body())
        }
    });
    let client = client(&server);

    let mut headers = HeaderMap::new();
    headers.insert("x-call", "two".parse().unwrap());
    let response = client
        .system_one()
        .state("hello")
        .question("first", Noul::new())
        .questions(vec![("second".to_owned(), Choice::new(["a", "b"]))])
        .model("call-model")
        .timeout(Duration::from_secs(2))
        .retry(RetryPolicy::new().with_max_retries(0))
        .header("x-call", "one")
        .headers(headers)
        .extra_body(json!({"temperature": 0}))
        .extra_field("temperature", 1)
        .send()
        .unwrap();
    assert_eq!(response.model, "jev-latest");

    let captured = server.request(0);
    assert_eq!(captured.header("x-call"), Some("two"));
    assert_eq!(
        captured.json(),
        json!({
            "state": "hello",
            "model": "call-model",
            "temperature": 1,
            "questions": {
                "first": {"type": "noul"},
                "second": {"type": "choice", "criteria": {"a": null, "b": null}},
            },
        })
    );

    let mut headers = HeaderMap::new();
    headers.insert("x-call", "two".parse().unwrap());
    let response = client
        .models()
        .list()
        .timeout(Duration::from_secs(2))
        .retry(RetryPolicy::new().with_max_retries(0))
        .header("x-call", "one")
        .headers(headers)
        .send()
        .unwrap();
    assert_eq!(response.models[0].name, "jev-latest");
    assert_eq!(response.status(), StatusCode::OK);

    let captured = server.request(1);
    assert_eq!(captured.method, "GET");
    assert_eq!(captured.path, "/v1/models");
    assert!(captured.body.is_empty());
    assert!(captured.header("content-type").is_none());
    assert_eq!(captured.header("x-call"), Some("two"));
    assert_eq!(captured.header("authorization"), Some("Bearer test-key"));
}

#[test]
fn retries_report_the_attempt_count() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let server = MockServer::start(move |_| {
        if counter.fetch_add(1, Ordering::SeqCst) < 2 {
            Action::Reply(Reply::with_headers(
                429,
                [("retry-after-ms", "0")],
                Vec::new(),
            ))
        } else {
            Action::json(200, system_one_body())
        }
    });
    let retrying = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .retry(
            RetryPolicy::new()
                .with_max_retries(2)
                .with_backoff_initial(Duration::from_millis(1))
                .with_backoff_max(Duration::from_millis(1)),
        )
        .build()
        .unwrap();

    let response = retrying
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(server.count(), 3);
    assert_eq!(server.request(0).retry_count(), None);
    assert_eq!(server.request(1).retry_count(), Some("1"));
    assert_eq!(server.request(2).retry_count(), Some("2"));

    let single = MockServer::start(|_| Action::json(200, system_one_body()));
    client(&single)
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .unwrap();
    assert_eq!(single.count(), 1);
}

#[test]
fn errors_are_mapped_like_the_async_client() {
    let server = MockServer::start(|_| Action::Reply(Reply::status(400)));
    assert!(matches!(error_from(&server), Error::BadRequest(_)));

    // Failures carry the same endpoint, request id, and Display string as the async client.
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            403,
            [("x-typesafe-request-id", "req-blocking")],
            br#"{"message":"denied"}"#.to_vec(),
        ))
    });
    let error = error_from(&server);
    let endpoint = format!("POST {}/v1/systemone", server.base_url());
    assert_eq!(error.endpoint(), Some(endpoint.as_str()));
    assert_eq!(error.request_id(), Some("req-blocking"));
    assert_eq!(
        error.to_string(),
        format!("{endpoint}: 403 denied (request_id=req-blocking)")
    );
    assert!(matches!(error, Error::PermissionDenied(_)));

    let server = MockServer::start(|_| Action::Reply(Reply::status(401)));
    assert!(matches!(error_from(&server), Error::Authentication(_)));

    let server = MockServer::start(|_| Action::Reply(Reply::status(404)));
    assert!(matches!(error_from(&server), Error::NotFound(_)));

    let server = MockServer::start(|_| Action::Reply(Reply::status(500)));
    assert!(matches!(error_from(&server), Error::InternalServerError(_)));

    let server = MockServer::start(|_| Action::Reply(Reply::status(302)));
    assert!(matches!(error_from(&server), Error::Api(_)));

    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            429,
            [("retry-after-ms", "250")],
            Vec::new(),
        ))
    });
    let error = error_from(&server);
    let Error::RateLimit(rate_limit) = &error else {
        panic!("expected a rate limit error, got {error:?}")
    };
    assert_eq!(rate_limit.retry_after(), Some(Duration::from_millis(250)));
    assert_eq!(error.status(), Some(StatusCode::TOO_MANY_REQUESTS));

    let server = MockServer::start(|_| Action::json(200, json!({"usage": {}, "answers": {}})));
    let error = error_from(&server);
    let Error::ResponseValidation(validation) = &error else {
        panic!("expected a validation error, got {error:?}")
    };
    assert_eq!(validation.field_path(), "model");

    let server = MockServer::start(|_| Action::close());
    assert!(matches!(error_from(&server), Error::Connection(_)));

    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(150),
            Reply::json(200, system_one_body()),
        )
    });
    let error = client(&server)
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .timeout(Duration::from_millis(50))
        .send()
        .unwrap_err();
    assert!(matches!(error, Error::Timeout(_)));
}

#[test]
fn client_surface_is_available() {
    let server = MockServer::start(|_| Action::json(200, json!({"models": [model_card()]})));

    assert!(TypeSafeClient::new("key").is_ok());
    let _: fn() -> typesafe_sdk::Result<TypeSafeClient> = TypeSafeClient::from_env;

    let error = TypeSafeClient::builder().build().unwrap_err();
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert!(
        config.message().contains("TYPESAFE_API_KEY"),
        "{}",
        config.message()
    );

    let mut default_headers = HeaderMap::new();
    default_headers.insert("x-from-http-client", "yes".parse().unwrap());
    let http_client = reqwest::blocking::Client::builder()
        .default_headers(default_headers)
        .build()
        .unwrap();
    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("custom-model")
        .retry(RetryPolicy::new().with_max_retries(3))
        .http_client(http_client)
        .build()
        .unwrap();

    assert_eq!(
        client.base_url().as_str(),
        format!("{}/", server.base_url())
    );
    assert_eq!(client.default_model(), "custom-model");
    assert_eq!(client.retry_policy().max_retries(), 3);
    let supplied: &reqwest::blocking::Client = client.http_client();
    let _ = supplied;

    client.models().list().send().unwrap();
    assert_eq!(server.request(0).header("x-from-http-client"), Some("yes"));
}

#[test]
fn async_builder_builds_a_blocking_client() {
    let server = MockServer::start(|_| Action::json(200, json!({"models": [model_card()]})));
    let client = typesafe_sdk::TypeSafeClient::builder()
        .api_key("key")
        .base_url(server.base_url())
        .model("test-model")
        .build_blocking()
        .unwrap();

    let response = client.models().list().send().unwrap();
    assert_eq!(response.models[0].name, "jev-latest");
    assert_eq!(response.status(), StatusCode::OK);

    let captured = server.request(0);
    assert_eq!(captured.method, "GET");
    assert_eq!(captured.path, "/v1/models");
}
