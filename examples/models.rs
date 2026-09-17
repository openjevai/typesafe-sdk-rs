//! Lists the models available to the account.
//!
//! Run with: `TYPESAFE_API_KEY=sk-... cargo run --example models`

use typesafe_sdk::TypeSafeClient;

#[tokio::main]
async fn main() -> typesafe_sdk::Result<()> {
    let client = TypeSafeClient::from_env()?;

    let response = client.models().list().send().await?;

    for model in &response.models {
        println!(
            "{}: {} ({})",
            model.name, model.description, model.release_date
        );
    }

    if let Some(request_id) = response.request_id() {
        println!("request id: {request_id}");
    }

    Ok(())
}
