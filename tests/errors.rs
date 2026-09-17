//! Error mapping: status codes, message extraction, request context, and Display output.

mod support;

use std::time::Duration;

use serde_json::json;
use support::{Action, MockServer, Reply};
use typesafe_sdk::{Error, RetryPolicy, StatusCode, TypeSafeClient};

/// Builds a retry-free client so error tests observe exactly one request.
fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::builder()
        .api_key("private-api-key")
        .base_url(server.base_url())
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .retry(RetryPolicy::new().with_max_retries(0))
        .build()
        .unwrap()
}

/// Runs a System One call and returns the error it produced.
async fn failing_call(client: &TypeSafeClient) -> Error {
    client
        .system_one()
        .state("hello")
        .question("q", typesafe_sdk::Noul::new())
        .send()
        .await
        .expect_err("the mock always fails")
}

#[tokio::test]
async fn statuses_map_to_error_variants() {
    let cases = [
        (400, "BadRequest"),
        (401, "Authentication"),
        (403, "PermissionDenied"),
        (404, "NotFound"),
        (422, "UnprocessableEntity"),
        (429, "RateLimit"),
        (500, "InternalServerError"),
        (503, "InternalServerError"),
        (599, "InternalServerError"),
        (302, "Api"),
        (408, "Api"),
        (409, "Api"),
    ];
    for (status, expected) in cases {
        let server = MockServer::start(move |_| Action::json(status, json!({"message": "failed"})));
        let error = failing_call(&client(&server)).await;
        assert_eq!(
            error.status(),
            Some(StatusCode::from_u16(status).unwrap()),
            "{status}"
        );
        assert_eq!(variant_name(&error), expected, "{status}");
        assert!(
            format!("{error:?}").starts_with(expected),
            "{status}: {error:?}"
        );
    }
}

/// Names the [`Error`] variant, so tests can assert the mapping directly.
fn variant_name(error: &Error) -> &'static str {
    match error {
        Error::Config(_) => "Config",
        Error::InvalidInput(_) => "InvalidInput",
        Error::Connection(_) => "Connection",
        Error::Timeout(_) => "Timeout",
        Error::BadRequest(_) => "BadRequest",
        Error::Authentication(_) => "Authentication",
        Error::PermissionDenied(_) => "PermissionDenied",
        Error::NotFound(_) => "NotFound",
        Error::UnprocessableEntity(_) => "UnprocessableEntity",
        Error::RateLimit(_) => "RateLimit",
        Error::InternalServerError(_) => "InternalServerError",
        Error::Api(_) => "Api",
        Error::ResponseValidation(_) => "ResponseValidation",
        _ => "Unknown",
    }
}

#[tokio::test]
async fn rate_limits_carry_the_requested_wait() {
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            429,
            [
                ("retry-after-ms", "125"),
                ("x-typesafe-request-id", "req-rate"),
            ],
            br#"{"message":"slow down"}"#.to_vec(),
        ))
    });
    let error = failing_call(&client(&server)).await;
    assert_eq!(error.retry_after(), Some(Duration::from_millis(125)));
    assert_eq!(error.request_id(), Some("req-rate"));
    assert_eq!(error.body(), Some(&json!({"message": "slow down"})));
    let Error::RateLimit(rate_limit) = &error else {
        panic!("expected a rate limit error")
    };
    assert_eq!(rate_limit.message(), "slow down");
    assert_eq!(rate_limit.retry_after(), Some(Duration::from_millis(125)));
}

#[tokio::test]
async fn responses_carry_request_context() {
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            400,
            [
                ("x-typesafe-request-id", "req-context"),
                ("x-extra", "kept"),
            ],
            br#"{"message":"Bad request"}"#.to_vec(),
        ))
    });
    let client = client(&server);
    let error = failing_call(&client).await;
    let endpoint = format!("POST {}/v1/systemone", server.base_url());
    assert_eq!(error.endpoint(), Some(endpoint.as_str()));
    assert_eq!(
        error.to_string(),
        format!("{endpoint}: 400 Bad request (request_id=req-context)")
    );
    assert_eq!(error.headers().unwrap().get("x-extra").unwrap(), "kept");
    assert_eq!(error.request_id(), Some("req-context"));
    assert!(error.body().is_some());
    let rendered = format!("{error:?}");
    assert!(!rendered.contains("private-api-key"), "{rendered}");
    assert!(!error.to_string().contains("private-api-key"));
}

#[tokio::test]
async fn endpoints_omit_url_credentials() {
    let server = MockServer::start(|_| Action::json(400, json!({"message": "Bad request"})));
    let client = TypeSafeClient::builder()
        .api_key("private-api-key")
        .base_url(format!(
            "http://user:password@{}",
            server.base_url().trim_start_matches("http://")
        ))
        .model("test-model")
        .retry(RetryPolicy::new().with_max_retries(0))
        .build()
        .unwrap();
    let error = failing_call(&client).await;
    assert_eq!(
        error.endpoint(),
        Some(format!("POST {}/v1/systemone", server.base_url()).as_str())
    );
    assert!(!error.to_string().contains("password"), "{error}");
    assert!(!error.to_string().contains("user"), "{error}");
}

#[tokio::test]
async fn messages_come_from_the_response_body() {
    let server = MockServer::start(|_| {
        Action::json(
            400,
            json!({"detail": [{"loc": ["body", "questions", 0], "msg": "bad"}, {"loc": [], "msg": "other"}]}),
        )
    });
    let error = failing_call(&client(&server)).await;
    assert_eq!(
        error.to_string(),
        format!(
            "POST {}/v1/systemone: 400 questions.0: bad; other",
            server.base_url()
        )
    );

    let server = MockServer::start(|_| Action::json(400, json!({"error": {"message": "nested"}})));
    let error = failing_call(&client(&server)).await;
    assert!(error.to_string().ends_with("400 nested"), "{error}");

    let server = MockServer::start(|_| Action::json(400, json!({"error": "top level"})));
    let error = failing_call(&client(&server)).await;
    assert!(error.to_string().ends_with("400 top level"), "{error}");

    let server = MockServer::start(|_| Action::json(400, json!({})));
    let error = failing_call(&client(&server)).await;
    assert!(error.to_string().ends_with("400 {}"), "{error}");
}

#[tokio::test]
async fn body_edge_cases_render_like_the_reference_implementation() {
    let cases: Vec<(Reply, String)> = vec![
        (Reply::status(400), "400 status code (no body)".to_owned()),
        (
            Reply::bytes(400, b"null".to_vec()),
            "400 status code (no body)".to_owned(),
        ),
        (Reply::bytes(400, b"[]".to_vec()), "400 []".to_owned()),
        (Reply::bytes(400, b"42".to_vec()), "400 42".to_owned()),
        (
            Reply::bytes(400, b"not JSON: \xff".to_vec()),
            "400 not JSON: \u{fffd}".to_owned(),
        ),
        (
            Reply::bytes(400, vec![b'x'; 201]),
            format!("400 {}", "x".repeat(201)),
        ),
        (
            Reply::bytes(
                400,
                format!("{{\"unknown\":\"{}\"}}", "x".repeat(201)).into_bytes(),
            ),
            format!("400 {{\"unknown\":\"{}…", "x".repeat(188)),
        ),
        (
            Reply::bytes(400, br#"{"error":"","message":"ignored"}"#.to_vec()),
            "400 {\"error\":\"\",\"message\":\"ignored\"}".to_owned(),
        ),
        (
            Reply::bytes(400, br#"{"detail":[null,42,{"msg":4}]}"#.to_vec()),
            "400 {\"detail\":[null,42,{\"msg\":4}]}".to_owned(),
        ),
    ];
    for (reply, expected) in cases {
        let server = MockServer::start(move |_| Action::Reply(reply.clone()));
        let error = failing_call(&client(&server)).await;
        assert_eq!(
            error.to_string(),
            format!("POST {}/v1/systemone: {expected}", server.base_url())
        );
        assert_eq!(error.request_id(), None);
    }
}

#[tokio::test]
async fn truncation_keeps_two_hundred_characters() {
    let body = json!({"unknown": "x".repeat(201)});
    let server = MockServer::start(move |_| Action::json(400, body.clone()));
    let error = failing_call(&client(&server)).await;
    let message = error.to_string();
    let raw = message.rsplit(": 400 ").next().unwrap();
    assert_eq!(raw.chars().count(), 201, "{raw}");
    assert!(raw.ends_with('…'), "{raw}");
}

#[tokio::test]
async fn display_degrades_without_endpoint_or_request_id() {
    let error = Error::Api(typesafe_sdk::ApiError::new(
        StatusCode::BAD_REQUEST,
        Some(json!({"message": "Bad request"})),
        typesafe_sdk::HeaderMap::new(),
        None,
        None,
    ));
    assert_eq!(error.to_string(), "400 Bad request");
    let error = Error::Api(typesafe_sdk::ApiError::new(
        StatusCode::BAD_REQUEST,
        None,
        typesafe_sdk::HeaderMap::new(),
        Some(String::new()),
        None,
    ));
    assert_eq!(error.to_string(), "400");
}

#[tokio::test]
async fn unknown_answer_kinds_do_not_fail_a_call() {
    let body = json!({
        "model": "test",
        "usage": {},
        "answers": {"mystery": {"type": "aurora", "value": 3}},
    });
    let server = MockServer::start(move |_| Action::json(200, body.clone()));
    let response = client(&server)
        .system_one()
        .state("x")
        .question("q", typesafe_sdk::Noul::new())
        .send()
        .await
        .unwrap();
    assert_eq!(response.answer("mystery").unwrap().kind(), "aurora");
}

#[tokio::test]
async fn models_failures_carry_the_same_context() {
    for (status, expected) in [
        (401, "Authentication"),
        (500, "InternalServerError"),
        (302, "Api"),
    ] {
        let server = MockServer::start(move |_| {
            Action::Reply(Reply::with_headers(
                status,
                [("x-typesafe-request-id", "req-123")],
                br#"{"detail":{"message":"Server explanation"}}"#.to_vec(),
            ))
        });
        let error = client(&server)
            .models()
            .list()
            .send()
            .await
            .expect_err("the listing fails");
        assert_eq!(
            error.status(),
            Some(StatusCode::from_u16(status).unwrap()),
            "{status}"
        );
        assert_eq!(variant_name(&error), expected, "{status}");
        let endpoint = format!("GET {}/v1/models", server.base_url());
        assert_eq!(error.endpoint(), Some(endpoint.as_str()));
        assert_eq!(
            error.to_string(),
            format!("{endpoint}: {status} Server explanation (request_id=req-123)")
        );
        assert_eq!(error.request_id(), Some("req-123"));
    }
}

#[tokio::test]
async fn repeated_response_headers_report_their_first_value() {
    let server = MockServer::start(|_| {
        Action::Reply(Reply::with_headers(
            400,
            [
                ("x-typesafe-request-id", "req-1"),
                ("x-typesafe-request-id", "req-2"),
            ],
            Vec::new(),
        ))
    });
    let error = failing_call(&client(&server)).await;
    assert_eq!(error.request_id(), Some("req-1"));
    assert!(error.to_string().ends_with("(request_id=req-1)"), "{error}");
}
