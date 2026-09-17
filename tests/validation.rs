//! Validation: response field paths for malformed bodies, request-side input checks, and config errors.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use support::{Action, MockServer, model_card};
use typesafe_sdk::{Error, HeaderMap, Noul, RetryPolicy, Score, TypeSafeClient};

/// Builds a retry-free client so failures are observed on the first attempt.
fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .retry(RetryPolicy::new().with_max_retries(0))
        .build()
        .unwrap()
}

/// Sends a System One call against a server that replies with `body` and returns the error.
async fn error_for_system_one(body: Value) -> Error {
    let server = MockServer::start(move |_| Action::json(200, body.clone()));
    client(&server)
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .expect_err("the reply is malformed")
}

/// Asserts the field path and the Display form of a response validation error.
async fn assert_field_path(body: Value, expected: &str) {
    let error = error_for_system_one(body.clone()).await;
    let Error::ResponseValidation(validation) = &error else {
        panic!("expected a validation error for {body}, got {error:?}");
    };
    assert_eq!(validation.field_path(), expected, "{body}");
    assert_eq!(validation.status().as_u16(), 200, "{body}");
    assert_eq!(validation.body(), Some(&body), "{body}");
    assert!(!validation.reason().is_empty(), "{body}");
    assert!(
        error
            .to_string()
            .contains(&format!("Invalid response data at '{expected}'.")),
        "{error}"
    );
}

#[tokio::test]
async fn malformed_system_one_bodies_report_the_offending_field() {
    let usage = json!({"input_tokens": 1, "output_tokens": 1});
    let cases: Vec<(Value, &str)> = vec![
        (json!({}), "model"),
        (json!({"usage": usage, "answers": {}}), "model"),
        (json!({"model": 4, "usage": usage, "answers": {}}), "model"),
        (json!({"model": "test", "answers": {}}), "usage"),
        (
            json!({"model": "test", "usage": [], "answers": {}}),
            "usage",
        ),
        (
            json!({"model": "test", "usage": {"input_tokens": -1}, "answers": {}}),
            "usage.input_tokens",
        ),
        (
            json!({"model": "test", "usage": {"output_tokens": 1.5}, "answers": {}}),
            "usage.output_tokens",
        ),
        (json!({"model": "test", "usage": usage}), "answers"),
        (
            json!({"model": "test", "usage": usage, "answers": []}),
            "answers",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"n": {"type": "noul"}}}),
            "answers.n.noul",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"n": {"noul": 1.0}}}),
            "answers.n.type",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"n": "noul"}}),
            "answers.n.type",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"c": {"type": "choice", "choice": "a", "probabilities": {}}}}),
            "answers.c.confidence",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"c": {"type": "choice", "confidence": 0.5, "probabilities": {}}}}),
            "answers.c.choice",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": [], "probabilities": {}}}}),
            "answers.s.legend",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"x": "bad"}, "probabilities": {}}}}),
            "answers.s.legend",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"0": "bad"}, "probabilities": {"x": 1.0}}}}),
            "answers.s.probabilities",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"0": "bad"}, "probabilities": {"0": "x"}}}}),
            "answers.s.probabilities[.]",
        ),
        (
            json!({"model": "test", "usage": usage, "answers": {"c": {"type": "choice", "choice": "a", "confidence": 1.0, "probabilities": {"a": "x"}}}}),
            "answers.c.probabilities[.]",
        ),
    ];
    for (body, expected) in cases {
        assert_field_path(body, expected).await;
    }
}

#[tokio::test]
async fn malformed_answers_are_reported_in_document_order() {
    // Answers are decoded in the order the response body lists them, so the first malformed answer in
    // the document is the one named, matching the Python SDK.
    let body = json!({
        "model": "test",
        "usage": {},
        "answers": {"zeta": {"type": "noul"}, "alpha": {"type": "choice", "criteria": {}}},
    });
    let error = error_for_system_one(body).await;
    let Error::ResponseValidation(validation) = &error else {
        panic!("expected a validation error")
    };
    assert_eq!(validation.field_path(), "answers.zeta.noul");

    let body = json!({
        "model": "test",
        "usage": {},
        "answers": {"alpha": {"type": "choice", "criteria": {}}, "zeta": {"type": "noul"}},
    });
    let error = error_for_system_one(body).await;
    let Error::ResponseValidation(validation) = &error else {
        panic!("expected a validation error")
    };
    assert_eq!(validation.field_path(), "answers.alpha.choice");
}

#[tokio::test]
async fn unparseable_bodies_report_an_empty_path() {
    for (body, expected) in [
        (
            b"not JSON: \xff".to_vec(),
            "200 Invalid response data at ''.",
        ),
        (b"null".to_vec(), "200 Invalid response data at ''."),
        (Vec::new(), "200 Invalid response data at ''."),
    ] {
        let reply = body.clone();
        let server =
            MockServer::start(move |_| Action::Reply(support::Reply::bytes(200, reply.clone())));
        let error = client(&server)
            .system_one()
            .state("x")
            .question("q", Noul::new())
            .send()
            .await
            .expect_err("the reply is not a response body");
        let Error::ResponseValidation(validation) = &error else {
            panic!("expected a validation error for {body:?}")
        };
        assert_eq!(validation.field_path(), "");
        assert_eq!(
            error.to_string(),
            format!("POST {}/v1/systemone: {expected}", server.base_url())
        );
    }
}

#[tokio::test]
async fn nested_model_cards_report_indexed_paths() {
    for missing in ["name", "description", "release_date"] {
        let mut incomplete = model_card();
        incomplete.as_object_mut().unwrap().remove(missing);
        let body = json!({"models": [model_card(), incomplete]});
        let server = MockServer::start(move |_| Action::json(200, body.clone()));
        let error = client(&server)
            .models()
            .list()
            .send()
            .await
            .expect_err("the card is malformed");
        let Error::ResponseValidation(validation) = &error else {
            panic!("expected a validation error")
        };
        assert_eq!(validation.field_path(), format!("models[1].{missing}"));
        assert_eq!(
            error.to_string(),
            format!(
                "GET {}/v1/models: 200 Invalid response data at 'models[1].{missing}'.",
                server.base_url()
            )
        );
    }

    let server = MockServer::start(|_| Action::json(200, json!({"models": [1]})));
    let error = client(&server)
        .models()
        .list()
        .send()
        .await
        .expect_err("the card is not an object");
    let Error::ResponseValidation(validation) = &error else {
        panic!("expected a validation error")
    };
    assert_eq!(validation.field_path(), "models[0]");

    let server = MockServer::start(|_| Action::json(200, json!({})));
    let error = client(&server)
        .models()
        .list()
        .send()
        .await
        .expect_err("models is missing");
    let Error::ResponseValidation(validation) = &error else {
        panic!("expected a validation error")
    };
    assert_eq!(validation.field_path(), "models");
}

#[tokio::test]
async fn validation_errors_carry_the_request_id_and_headers() {
    let body = json!({"usage": {}, "answers": {}});
    let server = MockServer::start(move |_| {
        Action::Reply(support::Reply::with_headers(
            200,
            [("x-typesafe-request-id", "req-123"), ("x-extra", "kept")],
            serde_json::to_vec(&body).unwrap(),
        ))
    });
    let error = client(&server)
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .expect_err("model is missing");
    assert_eq!(error.request_id(), Some("req-123"));
    assert_eq!(error.headers().unwrap().get("x-extra").unwrap(), "kept");
    assert_eq!(
        error.to_string(),
        format!(
            "POST {}/v1/systemone: 200 Invalid response data at 'model'. (request_id=req-123)",
            server.base_url()
        )
    );
}

#[tokio::test]
async fn request_inputs_are_validated_before_the_network() {
    /// One invalid request: a builder step applied to a fresh request, and the message it must produce.
    type InvalidRequest = (
        Box<dyn FnOnce(typesafe_sdk::SystemOneRequest) -> typesafe_sdk::SystemOneRequest>,
        String,
    );

    let cases: Vec<InvalidRequest> = vec![
        (
            Box::new(|request| request.state("x")),
            "At least one question is required.".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!({}))),
            "Question \"invalid\" must be a question object or a dictionary with a nonempty string \"type\".".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!({"type": ""}))),
            "Question \"invalid\" must be a question object or a dictionary with a nonempty string \"type\".".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!({"type": 4}))),
            "Question \"invalid\" must be a question object or a dictionary with a nonempty string \"type\".".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!("noul"))),
            "Question \"invalid\" must be a question object or a dictionary with a nonempty string \"type\".".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!({"type": "choice"}))),
            "Question \"invalid\" requires \"criteria\".".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!({"type": "score"}))),
            "Question \"invalid\" requires \"criteria\".".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", json!({"type": "score", "criteria": []}))),
            "Score question \"invalid\" has no criteria; at least one score is required.".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("invalid", Score::new(Vec::<String>::new()))),
            "Score question \"invalid\" has no criteria; at least one score is required.".to_owned(),
        ),
        (
            Box::new(|request| request.question("q", Noul::new())),
            "state is required".to_owned(),
        ),
        (
            Box::new(|request| request.state("x").question("q", Noul::new()).extra_body(json!([1, 2]))),
            "extra_body must be a JSON object".to_owned(),
        ),
    ];
    let server = MockServer::start(|_| {
        Action::json(200, json!({"model": "test", "usage": {}, "answers": {}}))
    });
    let client = client(&server);
    for (build, expected) in cases {
        let error = build(client.system_one())
            .send()
            .await
            .expect_err("the request is invalid");
        let Error::InvalidInput(invalid) = &error else {
            panic!("expected an invalid-input error, got {error:?}")
        };
        assert_eq!(invalid.message(), expected);
        assert_eq!(error.to_string(), expected);
    }
    assert_eq!(
        server.count(),
        0,
        "invalid requests never reach the network"
    );

    for question in [
        json!({"type": "future", "nested": {"k": null}}),
        json!({"type": "noul", "weight": 3}),
    ] {
        client
            .system_one()
            .state("x")
            .question("q", question.clone())
            .send()
            .await
            .unwrap_or_else(|error| panic!("{question} should be accepted: {error}"));
    }
    assert_eq!(
        server.count(),
        2,
        "raw questions without known requirements are sent as-is"
    );
}

#[tokio::test]
async fn per_call_settings_are_validated() {
    let server = MockServer::start(|_| Action::json(200, support::system_one_body()));
    let client = client(&server);
    let error = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .timeout(Duration::ZERO)
        .send()
        .await
        .expect_err("a zero timeout is invalid");
    let Error::InvalidInput(invalid) = &error else {
        panic!("expected an invalid-input error")
    };
    assert_eq!(
        invalid.message(),
        "timeout must be a positive, finite number of seconds."
    );

    let error = client
        .models()
        .list()
        .timeout(Duration::ZERO)
        .send()
        .await
        .expect_err("a zero timeout is invalid");
    assert!(matches!(error, Error::InvalidInput(_)), "{error:?}");

    let error = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .header("bad name", "value")
        .send()
        .await
        .expect_err("an invalid header name is rejected");
    let Error::InvalidInput(invalid) = &error else {
        panic!("expected an invalid-input error")
    };
    assert_eq!(invalid.message(), "invalid header name \"bad name\"");

    let error = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .retry(RetryPolicy::new().with_budget(Duration::ZERO))
        .send()
        .await
        .expect_err("a zero retry budget is rejected");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert_eq!(
        config.message(),
        "timeout must be a positive, finite number of seconds."
    );

    assert_eq!(
        server.count(),
        0,
        "invalid requests never reach the network"
    );
}

#[tokio::test]
async fn client_configuration_is_validated() {
    let error = TypeSafeClient::builder()
        .build()
        .expect_err("the API key is missing");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert!(
        config.message().contains("No API key was provided"),
        "{}",
        config.message()
    );
    assert!(
        config.message().contains("TYPESAFE_API_KEY"),
        "{}",
        config.message()
    );

    let error = TypeSafeClient::builder()
        .api_key("key")
        .base_url("/v1")
        .build()
        .expect_err("the base URL is relative");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert_eq!(
        config.message(),
        "base_url must be an absolute http(s) URL: /v1"
    );

    let error = TypeSafeClient::builder()
        .api_key("key")
        .timeout(Duration::ZERO)
        .build()
        .expect_err("a zero timeout is invalid");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert_eq!(
        config.message(),
        "timeout must be a positive, finite number of seconds."
    );

    let error = TypeSafeClient::builder()
        .api_key("key")
        .header("bad name", "value")
        .build()
        .expect_err("an invalid header name is rejected");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert_eq!(config.message(), "invalid header name \"bad name\"");

    let error = TypeSafeClient::builder()
        .api_key("key")
        .header("x-bad", "value\n")
        .build()
        .expect_err("an invalid header value is rejected");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert_eq!(config.message(), "invalid header value for \"x-bad\"");

    let error = TypeSafeClient::builder()
        .api_key("key")
        .retry(RetryPolicy::new().with_budget(Duration::ZERO))
        .build()
        .expect_err("a zero retry budget is rejected");
    let Error::Config(config) = &error else {
        panic!("expected a config error, got {error:?}")
    };
    assert_eq!(
        config.message(),
        "timeout must be a positive, finite number of seconds."
    );
}

#[tokio::test]
async fn header_maps_are_validated_like_pairs() {
    let server = MockServer::start(|_| Action::json(200, support::system_one_body()));
    let client = client(&server);
    let mut headers = HeaderMap::new();
    headers.insert("x-call", "from-map".parse().unwrap());
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .header("x-call", "from-pair")
        .headers(headers)
        .send()
        .await
        .unwrap();
    assert_eq!(server.request(0).header("x-call"), Some("from-map"));
}

#[tokio::test]
async fn malformed_raw_question_shapes_are_left_to_the_api() {
    let server = MockServer::start(|_| Action::json(422, json!({"message": "Invalid question"})));
    let client = client(&server);
    let cases = [
        json!({"type": "noul", "instructions": 1}),
        json!({"type": "choice", "criteria": ["invalid", "shape"]}),
    ];
    for question in cases {
        let error = client
            .system_one()
            .state("x")
            .question("q", question.clone())
            .send()
            .await
            .expect_err("the server rejects the question");
        let Error::UnprocessableEntity(api) = &error else {
            panic!("expected a 422 for {question}, got {error:?}");
        };
        assert_eq!(api.message(), "Invalid question");
        assert_eq!(
            server.request(server.count() - 1).json()["questions"]["q"],
            question
        );
    }
    assert_eq!(server.count(), 2, "each malformed question is sent once");
}
