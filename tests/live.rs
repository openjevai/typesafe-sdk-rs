//! Live smoke tests against the TypeSafe platform.
//!
//! Every test here reaches the real API, so all of them are `#[ignore]`d and need a real key:
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo test --test live -- --ignored --nocapture
//! ```
//!
//! `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL` are honoured when set, exactly as the SDK's own
//! environment resolution does.

mod support;

use std::time::Duration;

use typesafe_sdk::constants::API_KEY_ENV;
use typesafe_sdk::{Choice, Error, Noul, Score, StatusCode, TypeSafeClient};

/// How long a live answer may take; generation is slower than the SDK's 10 second default.
const LIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// Builds a client from `TYPESAFE_API_KEY`, panicking with instructions when the key is absent.
///
/// Only the key and the generous timeout are passed explicitly, so `TYPESAFE_BASE_URL` and
/// `TYPESAFE_DEFAULT_MODEL` still take effect when they are set.
fn live_client() -> TypeSafeClient {
    let key = std::env::var(API_KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| {
            panic!("set TYPESAFE_API_KEY to run the live tests: TYPESAFE_API_KEY=... cargo test --test live -- --ignored")
        });
    TypeSafeClient::builder()
        .api_key(key)
        .timeout(LIVE_TIMEOUT)
        .build()
        .expect("the live client should build")
}

#[tokio::test]
#[ignore = "requires TYPESAFE_API_KEY"]
async fn models_lists_non_empty_cards() {
    let client = live_client();
    let response = client
        .models()
        .list()
        .send()
        .await
        .expect("listing the models should succeed");

    assert!(
        !response.models.is_empty(),
        "the account should expose at least one model"
    );
    for card in &response.models {
        println!(
            "{}: {} ({})",
            card.name, card.description, card.release_date
        );
        assert!(!card.name.is_empty(), "every card needs a name");
        assert!(
            !card.description.is_empty(),
            "every card needs a description"
        );
        assert!(
            !card.release_date.is_empty(),
            "every card needs a release date"
        );
    }
    assert!(
        response.models.iter().any(|card| card.name == "jev-latest"),
        "jev-latest should be available"
    );
}

#[tokio::test]
#[ignore = "requires TYPESAFE_API_KEY"]
async fn system_one_answers_a_support_ticket() {
    const LABELS: [&str; 3] = ["calm", "frustrated", "angry"];
    const LEVELS: [&str; 3] = ["low", "medium", "high"];

    let client = live_client();
    let response = client
        .system_one()
        .state("I was charged twice for order A-100. Please refund the duplicate. This is very frustrating.")
        .question("billing", Noul::new().instructions("Is this about billing or payments?"))
        .question("tone", Choice::new(LABELS))
        .question("urgency", Score::new(LEVELS).instructions("How urgent is this?"))
        .send()
        .await
        .expect("the System One request should succeed");

    let noul = response
        .noul("billing")
        .expect("the billing answer should be a noul");
    assert!(
        (0.0..=1.0).contains(&noul.noul),
        "noul {} is out of range",
        noul.noul
    );

    let choice = response
        .choice("tone")
        .expect("the tone answer should be a choice");
    assert!(
        LABELS.contains(&choice.choice.as_str()),
        "unexpected label {:?}",
        choice.choice
    );
    let total: f64 = choice.probabilities.values().sum();
    assert!((total - 1.0).abs() <= 0.05, "probabilities sum to {total}");
    for label in LABELS {
        let probability = choice
            .probability(label)
            .unwrap_or_else(|| panic!("missing probability for {label}"));
        assert!(
            (0.0..=1.0).contains(&probability),
            "probability {probability} for {label} is out of range"
        );
    }

    let score = response
        .score("urgency")
        .expect("the urgency answer should be a score");
    assert_eq!(
        score.legend.len(),
        LEVELS.len(),
        "the legend should describe every rubric level"
    );
    for (index, level) in LEVELS.iter().enumerate() {
        let description = score
            .legend
            .get(&(index as i64))
            .unwrap_or_else(|| panic!("missing legend entry {index}"));
        assert_eq!(description.as_str(), Some(*level));
    }

    assert!(
        response.usage.input_tokens.is_some(),
        "usage should report input tokens"
    );
    assert!(
        response
            .request_id()
            .is_some_and(|id| id.starts_with("req_")),
        "unexpected request id"
    );
}

#[tokio::test]
#[ignore = "requires TYPESAFE_API_KEY"]
async fn an_invalid_key_is_rejected() {
    let client = TypeSafeClient::builder()
        .api_key("invalid-key")
        .timeout(LIVE_TIMEOUT)
        .build()
        .expect("the client should build");
    let error = client
        .models()
        .list()
        .send()
        .await
        .expect_err("an invalid key should be rejected");

    assert_eq!(error.status(), Some(StatusCode::UNAUTHORIZED));
    let Error::Authentication(api) = error else {
        panic!("expected an authentication error, got {error:?}");
    };
    assert!(
        !api.message().is_empty(),
        "the server should explain the failure"
    );
}
