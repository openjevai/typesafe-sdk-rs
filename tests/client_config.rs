//! Client construction, configuration resolution, and timeout propagation.
//!
//! Everything here is deterministic: every client passes an explicit API key, base URL, and model, so
//! only the single environment test reads the process environment.

mod support;

use std::time::{Duration, Instant};

use serde_json::json;
use support::{Action, MockServer, Reply, model_card, system_one_body};
use typesafe_sdk::constants::{API_KEY_ENV, BASE_URL_ENV, DEFAULT_MODEL_ENV, JEV_PROVIDER_ENV, OPENJEV_API_KEY_ENV};
use typesafe_sdk::{Error, HeaderMap, Noul, RetryPolicy, TypeSafeClient};

/// A client with retries disabled, so timing assertions observe exactly one attempt.
fn retry_free(server: &MockServer, timeout: Duration) -> TypeSafeClient {
    TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .timeout(timeout)
        .retry(RetryPolicy::new().with_max_retries(0))
        .build()
        .unwrap()
}

/// Restores the environment variables it captured, even when the test that owns it fails.
struct EnvGuard {
    /// Names and the values they held before the test ran.
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    /// Captures the current value of every named variable.
    fn capture(names: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            saved: names
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        }
    }
}

impl Drop for EnvGuard {
    /// Puts every captured variable back the way it was.
    fn drop(&mut self) {
        for (name, value) in &self.saved {
            // SAFETY: this is the only test file that touches the environment, and every variable it
            // writes is restored by this guard; no other thread reads these variables concurrently.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

#[test]
fn the_builder_exposes_the_resolved_configuration() {
    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url("https://code.test/prefix")
        .model("custom-model")
        .build()
        .unwrap();

    assert_eq!(client.default_model(), "custom-model");
    assert_eq!(client.base_url().as_str(), "https://code.test/prefix");
    assert_eq!(client.retry_policy().max_retries(), 2);
    assert_eq!(
        client.retry_policy().backoff_initial(),
        Duration::from_millis(500)
    );
}

#[tokio::test]
async fn entry_points_and_environment_values_are_resolved() {
    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let _guard = EnvGuard::capture([API_KEY_ENV, BASE_URL_ENV, DEFAULT_MODEL_ENV, OPENJEV_API_KEY_ENV, JEV_PROVIDER_ENV]);
    // SAFETY: see `EnvGuard::drop`; the guard restores these even if an assertion below panics.
    unsafe {
        std::env::remove_var(API_KEY_ENV);
        std::env::remove_var(BASE_URL_ENV);
        std::env::remove_var(DEFAULT_MODEL_ENV);
        std::env::remove_var(OPENJEV_API_KEY_ENV);
        std::env::remove_var(JEV_PROVIDER_ENV);
    }

    // Without a key, `from_env` fails with an error naming the variable to set.
    let error = TypeSafeClient::from_env().unwrap_err();
    assert!(matches!(error, Error::Config(_)), "{error:?}");
    assert!(
        error.to_string().contains("No API key was provided"),
        "{error}"
    );
    assert!(error.to_string().contains(API_KEY_ENV), "{error}");

    // `new` accepts an owned and a borrowed key and falls back to the SDK defaults.
    let owned = TypeSafeClient::new(String::from("owned-key")).unwrap();
    let borrowed = TypeSafeClient::new("borrowed-key").unwrap();
    for client in [&owned, &borrowed] {
        assert_eq!(client.default_model(), "jev-latest");
        assert_eq!(client.base_url().host_str(), Some("api.typesafe.ai"));
        assert_eq!(client.retry_policy().max_retries(), 2);
    }

    // Environment values are trimmed, and a key alone is enough to build a client.
    // SAFETY: as above.
    unsafe {
        std::env::set_var(API_KEY_ENV, "  env-key  ");
        std::env::set_var(BASE_URL_ENV, format!("  {}  ", server.base_url()));
        std::env::set_var(DEFAULT_MODEL_ENV, "  env-model  ");
    }
    let client = TypeSafeClient::from_env().unwrap();
    assert_eq!(client.default_model(), "env-model");
    assert_eq!(
        client.base_url().as_str().trim_end_matches('/'),
        server.base_url()
    );
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    let request = server.request(0);
    assert_eq!(request.header("authorization"), Some("Bearer env-key"));
    assert_eq!(request.json()["state"], json!("x"));
    assert_eq!(request.json()["model"], json!("env-model"));

    // Explicit options beat the environment.
    let explicit = TypeSafeClient::builder()
        .api_key("explicit-key")
        .base_url(server.base_url())
        .model("explicit-model")
        .build()
        .unwrap();
    assert_eq!(explicit.default_model(), "explicit-model");
    explicit
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    let request = server.request(1);
    assert_eq!(request.header("authorization"), Some("Bearer explicit-key"));
    assert_eq!(request.json()["model"], json!("explicit-model"));

    // Blank environment values are ignored: a blank key is treated as missing...
    // SAFETY: as above.
    unsafe {
        std::env::set_var(API_KEY_ENV, "   ");
    }
    let error = TypeSafeClient::from_env().unwrap_err();
    assert!(matches!(error, Error::Config(_)), "{error:?}");
    assert!(error.to_string().contains(API_KEY_ENV), "{error}");

    // ...and blank base URL and model values fall back to the defaults.
    // SAFETY: as above.
    unsafe {
        std::env::set_var(BASE_URL_ENV, " \t ");
        std::env::set_var(DEFAULT_MODEL_ENV, "\n");
    }
    let client = TypeSafeClient::builder()
        .api_key("explicit-key")
        .build()
        .unwrap();
    assert_eq!(client.base_url().host_str(), Some("api.typesafe.ai"));
    assert_eq!(client.default_model(), "jev-latest");
}

#[tokio::test]
async fn base_url_slashes_are_trimmed_and_prefixes_are_kept() {
    let server = MockServer::start(|request| match request.method.as_str() {
        "GET" => Action::json(200, json!({"models": [model_card()]})),
        _ => Action::json(200, system_one_body()),
    });

    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(format!("{}///", server.base_url()))
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    assert_eq!(
        client.base_url().as_str().trim_end_matches('/'),
        server.base_url()
    );
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    assert_eq!(server.request(0).path, "/v1/systemone");

    let prefixed = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(format!("{}/prefix", server.base_url()))
        .model("test-model")
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    prefixed.models().list().send().await.unwrap();
    let request = server.request(1);
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/prefix/v1/models");

    // A trailing-slash-only base URL keeps its host, so paths never gain a doubled separator.
    let trimmed = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url("https://code.test///")
        .build()
        .unwrap();
    assert_eq!(trimmed.base_url().host_str(), Some("code.test"));
    assert_eq!(trimmed.base_url().path(), "/");
}

#[tokio::test]
async fn a_per_call_timeout_is_enforced_against_a_delayed_reply() {
    // The reply is a full second away, so the elapsed bound below proves the *client* aborted the
    // request rather than the server answering late.
    let server = MockServer::start(|_| {
        Action::delay(Duration::from_secs(1), Reply::json(200, system_one_body()))
    });
    let client = retry_free(&server, Duration::from_secs(5));

    let started = Instant::now();
    let error = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .timeout(Duration::from_millis(50))
        .send()
        .await
        .unwrap_err();
    let elapsed = started.elapsed();

    assert!(matches!(error, Error::Timeout(_)), "{error:?}");
    assert!(
        error.to_string().starts_with("Request timed out (timeout="),
        "{error}"
    );
    assert!(
        elapsed < Duration::from_millis(700),
        "the timeout should fire long before the one second reply: {elapsed:?}"
    );
}

#[tokio::test]
async fn a_per_call_timeout_overrides_the_client_timeout() {
    let server = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(300),
            Reply::json(200, system_one_body()),
        )
    });
    let client = retry_free(&server, Duration::from_secs(5));

    let quick = client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .timeout(Duration::from_millis(50))
        .send()
        .await;
    assert!(matches!(quick, Err(Error::Timeout(_))), "{quick:?}");

    // The same server answers the same call when the per-call override is absent.
    let relaxed = support::client(&server).unwrap();
    let response = relaxed
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn a_caller_supplied_http_client_is_used_and_its_defaults_are_inherited() {
    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let mut defaults = HeaderMap::new();
    defaults.insert("x-default", "kept".parse().unwrap());
    defaults.insert("authorization", "Bearer impostor".parse().unwrap());
    let supplied = reqwest::Client::builder()
        .default_headers(defaults)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .http_client(supplied.clone())
        .build()
        .unwrap();
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();

    let request = server.request(0);
    assert_eq!(request.header("x-default"), Some("kept"));
    assert_eq!(request.header("authorization"), Some("Bearer test-key"));
    assert_ne!(request.header("authorization"), Some("Bearer impostor"));

    // The stored client is the supplied one: only it contributes `x-default`, and the SDK's own
    // clients never send it. The accessor returns one stable client for the life of the SDK client.
    let mut plain_defaults = HeaderMap::new();
    plain_defaults.insert("authorization", "Bearer impostor".parse().unwrap());
    let plain = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .http_client(
            reqwest::Client::builder()
                .default_headers(plain_defaults)
                .build()
                .unwrap(),
        )
        .build()
        .unwrap();
    plain
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    assert_eq!(server.request(1).header("x-default"), None);

    // An unset SDK timeout leaves the supplied client's own timeout in charge: the request is not
    // cut short by an SDK-level deadline.
    let delayed = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(300),
            Reply::json(200, system_one_body()),
        )
    });
    let relaxed = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(delayed.base_url())
        .model("test-model")
        .retry(RetryPolicy::new().with_max_retries(0))
        .http_client(supplied)
        .build()
        .unwrap();
    let response = relaxed
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    assert_eq!(response.model, "jev-latest");
    assert_eq!(delayed.count(), 1);
    assert_eq!(delayed.request(0).header("x-default"), Some("kept"));

    // The supplied client's own (short) timeout is what fires when the SDK sets none.
    let mut impatient = HeaderMap::new();
    impatient.insert("x-default", "kept".parse().unwrap());
    let slow = MockServer::start(|_| {
        Action::delay(
            Duration::from_millis(300),
            Reply::json(200, system_one_body()),
        )
    });
    let impatient_client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(slow.base_url())
        .model("test-model")
        .retry(RetryPolicy::new().with_max_retries(0))
        .http_client(
            reqwest::Client::builder()
                .default_headers(impatient)
                .timeout(Duration::from_millis(50))
                .build()
                .unwrap(),
        )
        .build()
        .unwrap();
    let error = impatient_client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Timeout(_)), "{error:?}");
}

#[tokio::test]
async fn the_reported_version_matches_the_crate() {
    assert_eq!(typesafe_sdk::VERSION, env!("CARGO_PKG_VERSION"));

    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let client = support::client(&server).unwrap();
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();

    let expected = format!("typesafe-sdk/{}", typesafe_sdk::VERSION);
    let request = server.request(0);
    assert_eq!(request.header("x-typesafe-sdk"), Some(expected.as_str()));
    assert_eq!(request.header("user-agent"), Some(expected.as_str()));
}

#[tokio::test]
async fn sdk_client_supplied_client_and_per_call_headers_layer_in_order() {
    let server = MockServer::start(|_| Action::json(200, system_one_body()));
    let mut supplied_defaults = HeaderMap::new();
    supplied_defaults.insert("x-layer", "supplied-client".parse().unwrap());
    let supplied = reqwest::Client::builder()
        .default_headers(supplied_defaults)
        .build()
        .unwrap();

    let client = TypeSafeClient::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .model("test-model")
        .header("x-layer", "sdk-client")
        .header("x-sdk-only", "kept")
        .http_client(supplied)
        .build()
        .unwrap();

    // Without a per-call value the SDK client's own header wins over the supplied client's default.
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .send()
        .await
        .unwrap();
    let request = server.request(0);
    assert_eq!(request.header("x-layer"), Some("sdk-client"));
    assert_eq!(request.header("x-sdk-only"), Some("kept"));

    // A per-call header wins over both.
    client
        .system_one()
        .state("x")
        .question("q", Noul::new())
        .header("x-layer", "per-call")
        .send()
        .await
        .unwrap();
    let request = server.request(1);
    assert_eq!(request.header("x-layer"), Some("per-call"));
    assert_eq!(request.header("x-sdk-only"), Some("kept"));
}
