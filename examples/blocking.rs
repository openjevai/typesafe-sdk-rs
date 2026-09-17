//! The same System One request as `system_one.rs`, using the blocking client.
//!
//! Run with: `TYPESAFE_API_KEY=sk-... cargo run --features blocking --example blocking`

use serde_json::json;
use typesafe_sdk::blocking::TypeSafeClient;
use typesafe_sdk::{Choice, Noul, Score};

fn main() -> typesafe_sdk::Result<()> {
    let client = TypeSafeClient::from_env()?;

    let response = client
        .system_one()
        .state(json!({"document": "I was charged twice. Please fix this ASAP."}))
        .question(
            "category",
            Choice::new(["billing", "technical", "other"])
                .instructions("What is this ticket about?"),
        )
        .question("urgent", Noul::new().instructions("Is this urgent?"))
        .question(
            "priority",
            Score::new(["low", "medium", "high"]).instructions("How urgent is this?"),
        )
        .send()?;

    if let Some(answer) = response.choice("category") {
        println!("category: {}", answer.choice);
    }

    if let Some(answer) = response.noul("urgent") {
        println!("urgent: {:.2}", answer.noul);
    }

    if let Some(answer) = response.score("priority") {
        println!("priority: {:.2}", answer.score);
    }

    println!("model: {}", response.model);

    if let Some(request_id) = response.request_id() {
        println!("request id: {request_id}");
    }

    Ok(())
}
