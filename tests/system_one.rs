//! System One round trips: request bodies, defaults, overrides, and response accessors.

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use support::{Action, MockServer, model_card, system_one_body};
use typesafe_sdk::{
    Answer, Choice, HeaderMap, JsonContent, Noul, NoulCriteria, Question, Score, StatusCode,
    TypeSafeClient,
};

use crate::support::Reply;

fn reply_ok() -> Action {
    Action::json(200, system_one_body())
}

#[tokio::test]
async fn typed_questions_are_encoded_on_the_wire() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    let response = client
        .system_one()
        .state("I was charged twice.")
        .question(
            "spam",
            Noul::new()
                .instructions("Is this spam?")
                .criteria(NoulCriteria::new().yes("yes").no("no")),
        )
        .question(
            "tone",
            Choice::new(["friendly", "hostile"])
                .instructions("What is the tone?")
                .describe("friendly", "kind"),
        )
        .question(
            "quality",
            Score::new(["bad", "great"]).instructions(json!({"focus": "quality"})),
        )
        .send()
        .await
        .unwrap();

    let request = server.request(0);
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/systemone");
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    assert_eq!(
        request.header("x-typesafe-sdk"),
        Some(format!("typesafe-sdk/{}", typesafe_sdk::VERSION).as_str())
    );
    assert_eq!(
        request.header("user-agent"),
        Some(format!("typesafe-sdk/{}", typesafe_sdk::VERSION).as_str())
    );
    assert!(
        request
            .header("x-typesafe-runtime")
            .unwrap()
            .starts_with("rust/")
    );
    assert_eq!(
        request.json(),
        json!({
            "state": "I was charged twice.",
            "model": "test-model",
            "questions": {
                "spam": {"type": "noul", "instructions": "Is this spam?", "criteria": {"true": "yes", "false": "no"}},
                "tone": {"type": "choice", "instructions": "What is the tone?", "criteria": {"friendly": "kind", "hostile": null}},
                "quality": {"type": "score", "instructions": {"focus": "quality"}, "criteria": ["bad", "great"]},
            },
        })
    );
    assert_eq!(response.model, "jev-latest");
}

#[tokio::test]
async fn raw_and_structured_inputs_pass_through() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    let questions = vec![
        (
            "raw".to_owned(),
            Question::from(json!({"type": "noul", "weight": 3, "criteria": {"future": "kept"}})),
        ),
        ("typed".to_owned(), Question::from(Noul::new())),
    ];
    client
        .system_one()
        .state(json!({"document": "hello", "metadata": [1, 2]}))
        .questions(questions)
        .send()
        .await
        .unwrap();

    assert_eq!(
        server.request(0).json(),
        json!({
            "state": {"document": "hello", "metadata": [1, 2]},
            "model": "test-model",
            "questions": {
                "raw": {"type": "noul", "weight": 3, "criteria": {"future": "kept"}},
                "typed": {"type": "noul"},
            },
        })
    );
}

#[tokio::test]
async fn json_content_covers_text_arrays_and_objects() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    client
        .system_one()
        .state(JsonContent::text("plain"))
        .question(
            "array",
            Score::new([
                JsonContent::json(json!([])),
                JsonContent::json(vec![json!("a")]),
            ]),
        )
        .question("empty", Choice::from_criteria(BTreeMap::new()))
        .send()
        .await
        .unwrap();

    let body = server.request(0).json();
    assert_eq!(body["state"], json!("plain"));
    assert_eq!(body["questions"]["array"]["criteria"], json!([[], ["a"]]));
    assert_eq!(
        body["questions"]["empty"],
        json!({"type": "choice", "criteria": {}})
    );
}

#[tokio::test]
async fn models_are_passed_as_collected_pairs() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    let collected = vec![
        ("first".to_owned(), Noul::new()),
        ("second".to_owned(), Noul::new()),
    ];
    client
        .system_one()
        .state("x")
        .questions(collected)
        .send()
        .await
        .unwrap();
    let questions = server.request(0).json()["questions"].clone();
    assert_eq!(questions.as_object().unwrap().len(), 2);
    assert_eq!(questions["first"], json!({"type": "noul"}));
    assert_eq!(questions["second"], json!({"type": "noul"}));
}

#[tokio::test]
async fn models_and_extra_body_are_overridable_per_call() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .model("call-model")
        .extra_body(json!({"state": "overridden", "temperature": 0}))
        .extra_field("temperature", 1)
        .send()
        .await
        .unwrap();
    let body = server.request(0).json();
    assert_eq!(body["model"], json!("call-model"));
    assert_eq!(body["state"], json!("overridden"));
    assert_eq!(body["temperature"], json!(1));
}

#[tokio::test]
async fn response_accessors_expose_every_answer_kind() {
    let server = MockServer::start(|req| {
        assert_eq!(req.path, "/v1/systemone");
        Action::Reply(Reply::with_headers(
            200,
            [("x-typesafe-request-id", "req-42"), ("x-extra", "kept")],
            serde_json::to_vec(&system_one_body()).unwrap(),
        ))
    });
    let client = support::client(&server).unwrap();
    let response = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();

    assert_eq!(response.request_id(), Some("req-42"));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("x-extra").unwrap(), "kept");
    assert_eq!(response.raw().body(), &system_one_body());
    assert_eq!(response.usage.input_tokens, Some(12));
    assert_eq!(response.usage.output_tokens, Some(3));

    assert_eq!(response.answer("spam").unwrap().kind(), "noul");
    assert_eq!(response.noul("spam").unwrap().noul, 0.98);
    assert_eq!(response.choice("tone").unwrap().choice, "friendly");
    assert_eq!(
        response.choice("tone").unwrap().probability("hostile"),
        Some(0.1)
    );
    assert_eq!(response.choice("tone").unwrap().confidence, 0.9);
    assert_eq!(response.answer("quality").unwrap().kind(), "score");
    let score = response.score("quality").unwrap();
    assert_eq!(score.score, 1.7);
    assert_eq!(score.probabilities.get(&2), Some(&0.8));
    assert_eq!(score.legend.get(&1).unwrap().as_str(), Some("ok"));

    assert_eq!(
        response.nouls().map(|(name, _)| name).collect::<Vec<_>>(),
        vec!["spam"]
    );
    assert_eq!(
        response.choices().map(|(name, _)| name).collect::<Vec<_>>(),
        vec!["tone"]
    );
    assert_eq!(
        response.scores().map(|(name, _)| name).collect::<Vec<_>>(),
        vec!["quality"]
    );
    assert!(response.answer("missing").is_none());
    assert!(response.noul("tone").is_none());
}

#[tokio::test]
async fn unknown_answers_are_kept_and_skipped_by_typed_accessors() {
    let body = json!({
        "model": "test",
        "usage": {},
        "answers": {
            "spam": {"type": "noul", "noul": 0.9},
            "mystery": {"type": "aurora", "value": 3},
        },
    });
    let server = MockServer::start(move |_| Action::json(200, body.clone()));
    let client = support::client(&server).unwrap();
    let response = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();

    assert_eq!(response.answers.len(), 2);
    assert_eq!(response.answer("mystery").unwrap().kind(), "aurora");
    let Answer::Unknown(unknown) = response.answer("mystery").unwrap() else {
        panic!("expected an unknown answer")
    };
    assert_eq!(unknown.kind, "aurora");
    assert_eq!(unknown.value, json!({"type": "aurora", "value": 3}));
    assert_eq!(response.nouls().count(), 1);
    assert_eq!(response.choices().count(), 0);
    assert_eq!(response.usage, typesafe_sdk::Usage::default());
    assert_eq!(
        response.raw().body()["answers"]["mystery"]["type"],
        json!("aurora")
    );
}

#[tokio::test]
async fn unknown_fields_are_ignored() {
    let body = json!({
        "model": "test",
        "usage": {"input_tokens": 1, "output_tokens": 1, "reasoning_tokens": 9, "billing_units": 1},
        "answers": {"spam": {"type": "noul", "noul": 0.9, "explanation": "spammy"}},
    });
    let server = MockServer::start(move |_| Action::json(200, body.clone()));
    let client = support::client(&server).unwrap();
    let response = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.usage,
        typesafe_sdk::Usage {
            input_tokens: Some(1),
            output_tokens: Some(1)
        }
    );
    assert_eq!(response.noul("spam").unwrap().noul, 0.9);
}

#[tokio::test]
async fn models_listing_returns_cards_and_raw_metadata() {
    let body = json!({"models": [model_card(), {"name": "future", "description": "New", "release_date": "2027-01-01", "context_window": 4096}]});
    let server = MockServer::start(move |_| Action::json(200, body.clone()));
    let client = support::client(&server).unwrap();
    let response = client.models().list().send().await.unwrap();

    let request = server.request(0);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/v1/models");
    assert!(request.body.is_empty());
    assert!(request.header("content-type").is_none());
    assert_eq!(response.models.len(), 2);
    assert_eq!(response.models[0].name, "jev-latest");
    assert_eq!(response.models[0].description, "Fast model");
    assert_eq!(response.models[0].release_date, "2026-08-01");
    assert_eq!(response.models[1].name, "future");
    assert_eq!(
        response.raw().body()["models"][1]["context_window"],
        json!(4096)
    );
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn per_call_headers_and_timeouts_apply_to_models_listing() {
    let server = MockServer::start(|_| Action::json(200, json!({"models": []})));
    let client = support::client(&server).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-call", "two".parse().unwrap());
    client
        .models()
        .list()
        .header("x-call", "one")
        .headers(headers)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .unwrap();
    let request = server.request(0);
    assert_eq!(request.header("x-call"), Some("two"));
}

#[tokio::test]
async fn the_client_exposes_its_configuration() {
    let server = MockServer::start(|_| reply_ok());
    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("custom-model")
        .build()
        .unwrap();
    assert_eq!(
        client.base_url().as_str(),
        format!("{}/", server.base_url())
    );
    assert_eq!(client.default_model(), "custom-model");
    assert_eq!(client.retry_policy().max_retries(), 2);
    assert!(format!("{client:?}").contains("custom-model"));
    assert!(!format!("{client:?}").contains("test-key"));
}

#[tokio::test]
async fn value_and_array_states_are_sent_verbatim() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    for state in [
        json!([]),
        json!([1, "two", {"three": 3}]),
        json!({"nested": {"deep": true}}),
    ] {
        client
            .system_one()
            .state(JsonContent::json(state.clone()))
            .question("q", Noul::new())
            .send()
            .await
            .unwrap();
        assert_eq!(server.request(server.count() - 1).json()["state"], state);
    }
    client
        .system_one()
        .state("text")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    let body: Value = server.request(server.count() - 1).json();
    assert_eq!(body["state"], json!("text"));
}

#[tokio::test]
async fn nullable_json_values_survive_inside_structures() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    client
        .system_one()
        .state(json!({"missing": null, "items": [null, {"nested": null}]}))
        .question(
            "yes",
            Noul::new()
                .instructions(json!({"text": "Classify", "extra": null}))
                .criteria(NoulCriteria::new().yes(JsonContent::json(Value::Null))),
        )
        .question(
            "label",
            Choice::from_criteria(BTreeMap::from([
                ("a".to_owned(), None),
                (
                    "b".to_owned(),
                    Some(JsonContent::json(json!({"extra": null}))),
                ),
            ])),
        )
        .question(
            "rating",
            Score::new([JsonContent::json(json!({"extra": null}))]),
        )
        .send()
        .await
        .unwrap();

    let body = server.request(0).json();
    assert_eq!(
        body["state"],
        json!({"missing": null, "items": [null, {"nested": null}]})
    );
    assert_eq!(
        body["questions"]["yes"],
        json!({"type": "noul", "instructions": {"text": "Classify", "extra": null}, "criteria": {"true": null}})
    );
    assert_eq!(
        body["questions"]["label"],
        json!({"type": "choice", "criteria": {"a": null, "b": {"extra": null}}})
    );
    assert_eq!(
        body["questions"]["rating"],
        json!({"type": "score", "criteria": [{"extra": null}]})
    );
}

#[tokio::test]
async fn unicode_state_round_trips() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    client
        .system_one()
        .state(json!({"document": "Hello 🌍"}))
        .question("q", Noul::new().instructions("Héllo 🌍"))
        .send()
        .await
        .unwrap();
    let body = server.request(0).json();
    assert_eq!(body["state"], json!({"document": "Hello 🌍"}));
    assert_eq!(
        body["questions"]["q"],
        json!({"type": "noul", "instructions": "Héllo 🌍"})
    );
}

#[tokio::test]
async fn raw_questions_preserve_explicit_nulls() {
    let server = MockServer::start(|_| reply_ok());
    let client = support::client(&server).unwrap();
    client
        .system_one()
        .state("x")
        .questions(vec![
            (
                "yes".to_owned(),
                Question::from(json!({"type": "noul", "instructions": null, "criteria": null})),
            ),
            (
                "label".to_owned(),
                Question::from(
                    json!({"type": "choice", "instructions": null, "criteria": {"a": null}}),
                ),
            ),
            (
                "rating".to_owned(),
                Question::from(
                    json!({"type": "score", "instructions": null, "criteria": ["good"]}),
                ),
            ),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(
        server.request(0).json()["questions"],
        json!({
            "yes": {"type": "noul", "instructions": null, "criteria": null},
            "label": {"type": "choice", "instructions": null, "criteria": {"a": null}},
            "rating": {"type": "score", "instructions": null, "criteria": ["good"]},
        })
    );
}
