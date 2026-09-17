# typesafe-sdk

Rust SDK for the [TypeSafe AI](https://typesafe.ai) API — a community port of
[`typesafe-sdk-python`](https://github.com/typesafe-ai/typesafe-sdk-python) v0.6.0.

This crate is maintained by its contributors and is not affiliated with, or endorsed by, TypeSafe AI.
It is licensed under [MIT](#license).

TypeSafe answers typed questions about text or structured state in one request. This SDK exposes
System One (`POST /v1/systemone`) and model listing (`GET /v1/models`) through async builders, with a
synchronous mirror behind the `blocking` feature, structurally identical errors, and the same retry
semantics as the Python SDK.

## Installation

```sh
cargo add typesafe-sdk
```

The default build is asynchronous (`tokio` + `reqwest`), and the minimum supported Rust version is
1.87 (edition 2024). Enable the `blocking` feature for the synchronous client:

```toml
[dependencies]
typesafe-sdk = { version = "0.1", features = ["blocking"] }
```

## Quickstart

Set `TYPESAFE_API_KEY` in the environment, then send a System One request:

```rust
# async fn quickstart() -> typesafe_sdk::Result<()> {
use serde_json::json;
use typesafe_sdk::{Choice, TypeSafeClient};

let client = TypeSafeClient::from_env()?;

let response = client
    .system_one()
    .state(json!({"document": "I was charged twice. Please fix this ASAP."}))
    .question(
        "category",
        Choice::new(["billing", "technical", "other"]).instructions("What is this ticket about?"),
    )
    .send()
    .await?;

if let Some(answer) = response.choice("category") {
    println!("{}", answer.choice);
}
# Ok(())
# }
```

`TypeSafeClient::new("sk-...")` takes the key explicitly, and `TypeSafeClient::builder()` covers the
remaining options (see [Configuration](#configuration)). The snippets in this file are compiled as
doctests, and `examples/` holds runnable versions with `#[tokio::main]` on `main`.

## Questions

Every request carries state and one or more named questions. State is text, a JSON object, or a
JSON array; anything that converts into `JsonContent` works, including `&str`, `String`,
`serde_json::Value`, and `JsonContent::{text, json, from_serialize}`.

```rust
use std::collections::BTreeMap;

use serde_json::json;
use typesafe_sdk::{Choice, JsonContent, Noul, NoulCriteria, Question, Score, TypeSafeClient};

async fn ask(client: TypeSafeClient) -> typesafe_sdk::Result<()> {
    // A yes/no question, with descriptions of the outcomes.
    let urgent = Noul::new()
        .instructions("Is this urgent?")
        .criteria(NoulCriteria::new().yes("Needs an answer today").no("Can wait"));

    // A choice question over labels, each described individually.
    let category = Choice::new(["billing", "technical", "other"])
        .instructions("What is this ticket about?")
        .describe("billing", "Anything about invoices or payments")
        .undescribed("other");

    // A score question over ordered rubric levels, counting from zero.
    let priority = Score::new(["low", "medium", "high"]).instructions("How urgent is this?");

    // Criteria built as a map instead of one call per label; `None` leaves a label undescribed.
    let mut criteria: BTreeMap<String, Option<JsonContent>> = BTreeMap::new();
    criteria.insert("billing".to_owned(), Some(JsonContent::text("Invoices and payments")));
    criteria.insert("other".to_owned(), None);
    let from_criteria = Choice::from_criteria(criteria);

    // Raw JSON passes through verbatim, including fields this SDK does not model.
    let raw = Question::from(json!({
        "type": "noul",
        "instructions": {"focus": "urgency"},
        "criteria": {"true": "urgent", "false": "routine"},
    }));

    client
        .system_one()
        .state(JsonContent::text("I was charged twice."))
        .question("urgent", urgent)
        .question("category", category)
        .question("priority", priority)
        .question("billing", from_criteria)
        .question("spam", raw)
        .model("jev-preview") // overrides the client default for this call
        .timeout(std::time::Duration::from_secs(30))
        .header("x-ticket-id", "T-1234")
        .extra_body(json!({"temperature": 0}))
        .extra_field("top_p", json!(0.5))
        .send()
        .await?;

    Ok(())
}
```

Raw questions are validated before the request is sent: the value must be a JSON object with a
non-empty string `type`, `choice` and `score` questions must carry `criteria`, and score criteria
must not be empty. Violations fail with `Error::InvalidInput` before any network traffic.

`extra_body` shallow-merges top-level body fields last-write-wins, so a key colliding with `state`,
`model`, or `questions` replaces it.

## Responses

```rust
use typesafe_sdk::{Choice, Noul, Score, TypeSafeClient};

async fn print_answers(client: TypeSafeClient) -> typesafe_sdk::Result<()> {
    let response = client
        .system_one()
        .state("I was charged twice. Please fix this ASAP.")
        .question("category", Choice::new(["billing", "technical", "other"]))
        .question("urgent", Noul::new().instructions("Is this urgent?"))
        .question("priority", Score::new(["low", "medium", "high"]))
        .send()
        .await?;

    // Typed accessors return `Some` only when the answer has that type.
    if let Some(answer) = response.choice("category") {
        println!("{} ({:.2})", answer.choice, answer.confidence);
        println!("{:?}", answer.probability("billing"));
        println!("{:?}", answer.probabilities);
    }

    if let Some(answer) = response.noul("urgent") {
        println!("{:.2}", answer.noul);
    }

    if let Some(answer) = response.score("priority") {
        println!("{:.2}", answer.score);
        for (level, description) in &answer.legend {
            println!("{level}: {}", description.as_str().unwrap_or_default());
        }
    }

    // Any answer, whatever its type, plus the wire type of the answer.
    if let Some(answer) = response.answer("category") {
        println!("{}", answer.kind());
    }

    // Iterators over the typed answers, keyed by question name.
    for (name, answer) in response.choices() {
        println!("{name}: {}", answer.choice);
    }
    for (_, answer) in response.nouls() {
        println!("{:.2}", answer.noul);
    }
    for (_, answer) in response.scores() {
        println!("{:.2}", answer.score);
    }

    // The model that produced the answers, and the tokens the request used.
    println!("{}", response.model);
    println!("{:?} {:?}", response.usage.input_tokens, response.usage.output_tokens);

    // HTTP metadata for the response.
    println!("{}", response.status());
    println!("{:?}", response.request_id());
    println!("{}", response.raw().body());

    Ok(())
}
```

Unknown fields anywhere in a response are ignored, so a newer server keeps working. An answer whose
`type` this SDK does not know is kept as `Answer::Unknown`, carrying the raw document in
`UnknownAnswer { kind, value }`; the typed iterators (`nouls`, `choices`, `scores`) skip it, while
`answer(name)` still returns it and a warning is logged.

`client.models().list().send().await?` returns `ListModelsResponse { models }`, where each
`ModelMetadata` has `name`, `description`, and `release_date`; `raw()`, `request_id()`, and
`status()` work the same way.

## Errors

Every fallible call returns `typesafe_sdk::Result<T>`, whose error is `typesafe_sdk::Error`:

| Variant | Cause |
| --- | --- |
| `Config` | Missing API key, invalid base URL, zero timeout |
| `InvalidInput` | Malformed questions, missing state, non-object `extra_body` |
| `Connection` / `Timeout` | The request never reached the server |
| `BadRequest`, `Authentication`, `PermissionDenied`, `NotFound`, `UnprocessableEntity` | The matching non-2xx status: `400`, `401`, `403`, `404`, `422` |
| `Api` | Any other non-2xx status |
| `RateLimit` | `429`, including `retry_after()` |
| `InternalServerError` | Any 5xx |
| `ResponseValidation` | A 2xx body that does not match the documented schema |

```rust
use typesafe_sdk::{Error, Noul, TypeSafeClient};

async fn report(client: TypeSafeClient) -> typesafe_sdk::Result<()> {
    let outcome = client
        .system_one()
        .state("I was charged twice.")
        .question("urgent", Noul::new().instructions("Is this urgent?"))
        .send()
        .await;

    match outcome {
        Ok(response) => println!("{}", response.model),
        Err(error) => {
            eprintln!("{error}");

            if let Some(status) = error.status() {
                eprintln!("status: {status}");
            }
            if let Some(request_id) = error.request_id() {
                eprintln!("request id: {request_id}");
            }
            if let Some(endpoint) = error.endpoint() {
                eprintln!("endpoint: {endpoint}");
            }
            if let Some(body) = error.body() {
                eprintln!("body: {body}");
            }
            if let Some(retry_after) = error.retry_after() {
                eprintln!("retry after: {retry_after:?}");
            }
            if let Error::ResponseValidation(validation) = &error {
                eprintln!("invalid field: {}", validation.field_path());
                eprintln!("reason: {}", validation.reason());
            }
        }
    }

    Ok(())
}
```

`Error::status()`, `body()`, `headers()`, `request_id()`, and `endpoint()` are available for any
failure that came from a response; a `RateLimit` failure also exposes the server's requested wait
through `Error::retry_after()`, and `endpoint()` renders the method and URL without credentials, query
parameters, or fragment. Validation failures name the offending field with a dotted path such as
`answers.category.confidence` or `models[1].description`, and their message is
`Invalid response data at '<path>'.`. The API key never appears in a `Display` or `Debug` rendering.

## Retries

The default policy retries `2` times with a `500 ms` initial backoff that doubles up to `5 s`,
subtracting up to `0.25` of each delay as jitter, for the statuses `408`, `429`, and every `5xx`,
while respecting `Retry-After` and `retry-after-ms` (server-provided delays are honoured however
long). Connection errors and timeouts are retried too, and each call has a `30 s` budget covering its
attempts and delays; a retry that would reach or pass the budget is not made and the last error is
returned. Retried requests carry an `X-TypeSafe-Retry-Count` header.

```rust
use std::time::Duration;

use typesafe_sdk::{Noul, RetryPolicy, StatusCode, TypeSafeClient};

fn build_client() -> typesafe_sdk::Result<TypeSafeClient> {
    TypeSafeClient::builder()
        .api_key("sk-...")
        .retry(RetryPolicy::new().with_max_retries(4).with_backoff_initial(Duration::from_millis(250)))
        .build()
}

// A per-call policy, replacing the client's for this request only.
fn policy() -> RetryPolicy {
    RetryPolicy::new()
        .with_max_retries(1)
        .with_backoff_initial(Duration::from_millis(100))
        .with_backoff_max(Duration::from_secs(1))
        .with_backoff_jitter(0.0)
        .with_retry_statuses([StatusCode::TOO_MANY_REQUESTS, StatusCode::SERVICE_UNAVAILABLE])
        .with_respect_retry_after(true)
        .with_retry_connection_errors(true)
        .with_retry_timeout_errors(false)
        .with_budget(Duration::from_secs(10))
        .retry_if(|error| error.status() == Some(StatusCode::NOT_FOUND))
}

async fn send_with_policy(client: &TypeSafeClient) -> typesafe_sdk::Result<()> {
    client
        .system_one()
        .state("I was charged twice.")
        .question("urgent", Noul::new())
        .retry(policy())
        .send()
        .await?;

    Ok(())
}
```

`RetryPolicy::new()` starts from the defaults above; its getters are `max_retries()`,
`backoff_initial()`, `backoff_max()`, `backoff_jitter()`, `statuses()`, `respects_retry_after()`,
`retries_connection_errors()`, `retries_timeout_errors()`, and `budget()`. A predicate added with
`retry_if` retries an error in addition to the policy's own rules, and `.retry(None)` falls back to
the client's policy.

## Blocking client

The `blocking` feature mirrors the whole API with synchronous calls that block the current thread,
including retry backoff.

```rust,ignore
use typesafe_sdk::blocking::TypeSafeClient;
use typesafe_sdk::{Choice, Noul};

fn main() -> typesafe_sdk::Result<()> {
    let client = TypeSafeClient::from_env()?;

    let response = client
        .system_one()
        .state("I was charged twice. Please fix this ASAP.")
        .question("category", Choice::new(["billing", "technical", "other"]))
        .question("urgent", Noul::new().instructions("Is this urgent?"))
        .send()?;

    if let Some(answer) = response.choice("category") {
        println!("{}", answer.choice);
    }

    Ok(())
}
```

`blocking::ClientBuilder` takes the same options, with `http_client(reqwest::blocking::Client)`
replacing the async client, and `client.models().list().send()?` lists models. The snippet above is
skipped by doctests because the module only exists with the feature enabled; `examples/blocking.rs` is
compiled with it.

## Configuration

Explicit options win over environment variables, and empty or whitespace-only environment values are
ignored. `base_url` has trailing slashes stripped.

| Option | Environment variable | Default |
| --- | --- | --- |
| `api_key` | `TYPESAFE_API_KEY` | none; required |
| `base_url` | `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` |
| `model` | `TYPESAFE_DEFAULT_MODEL` | `jev-latest` |
| `timeout` | — | 10 s per request |
| `connect_timeout` | — | none |
| `retry` | — | the [default policy](#retries) |

```rust
use std::time::Duration;

use typesafe_sdk::{Noul, TypeSafeClient};

async fn configure() -> typesafe_sdk::Result<()> {
    let client = TypeSafeClient::builder()
        .api_key("sk-...")
        .base_url("https://api.typesafe.ai")
        .model("jev-preview")
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .header("x-service", "billing")
        .build()?;

    client
        .system_one()
        .state("I was charged twice.")
        .question("urgent", Noul::new())
        .model("jev-latest") // overrides the client's default model for this call
        .timeout(Duration::from_secs(5))
        .send()
        .await?;

    Ok(())
}
```

Per-call `.model(...)`, `.timeout(...)`, `.retry(...)`, `.header(...)`, and `.headers(...)` override
the client for that request only. Authentication, `Accept`, `User-Agent`, `X-TypeSafe-SDK`, and
`X-TypeSafe-Runtime` are always set by the SDK and cannot be overridden.

Supplying your own HTTP client hands its defaults to the SDK (its timeout is kept when none is set,
and the client owns its own connect timeout), while SDK headers still win:

```rust
use std::time::Duration;

use typesafe_sdk::{Noul, TypeSafeClient};

async fn custom_http_client() -> typesafe_sdk::Result<()> {
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("the HTTP client should build");

    let client = TypeSafeClient::builder()
        .api_key("sk-...")
        .http_client(http_client)
        .build()?;

    client
        .system_one()
        .state("I was charged twice.")
        .question("urgent", Noul::new())
        .send()
        .await?;

    Ok(())
}
```

## Logging

The SDK never installs a logger; records go to the `log` crate under the `typesafe_sdk` target and are
visible when your application enables that target:

```sh
RUST_LOG=typesafe_sdk=debug cargo run --example system_one
```

Debug records show each request and response with its method, URL, headers, and body; info records
report status, duration, and the request id, plus retries and transport failures; an unrecognized
answer type is reported at warn level.

Credential-bearing header values (`authorization`, `proxy-authorization`, `x-api-key`, `api-key`,
`cookie`, `set-cookie`, and any header whose name contains `token` or `secret`) are always redacted
to `***`. Request and response bodies are **not** redacted, so avoid placing secrets in state or
questions.

`TYPESAFE_LOG_LEVEL` is not read by this SDK: the application owns logger configuration, so set the
level of the `typesafe_sdk` target with your logger of choice.

## Testing

```sh
cargo test                      # unit tests, doctests, and the hermetic mock-server suite
cargo test --all-features       # includes the blocking tests
TYPESAFE_API_KEY=sk-... cargo test --test live -- --ignored   # live platform tests
TYPESAFE_API_KEY=sk-... cargo run --example system_one        # end-to-end smoke test
```

The default suite is hermetic: it runs against a mock server on a loopback port, with no network
egress.

## Divergences from the Python SDK

Every behaviour below was checked against `typesafe-sdk-python` v0.6.0; request bodies, headers, error
variants, `field_path`s, and error `Display` strings match it byte for byte on the cases the test
suites and a side-by-side harness exercise. The differences that remain are deliberate.

**API shape**

- The SDK never configures logging, so `TYPESAFE_LOG_LEVEL` is ignored; set the `typesafe_sdk` target
  through your logger instead (`RUST_LOG=typesafe_sdk=debug`).
- Python's `transport=` injection maps to supplying a `reqwest::Client` (or
  `reqwest::blocking::Client`) through `.http_client(...)`; `build_blocking()` ignores a client supplied
  for the asynchronous client.
- There is no explicit `close()` or context manager: dropping a client releases its connection pool,
  and a client supplied through `http_client(...)` is released once the last handle drops. A supplied
  client is never closed by this SDK.
- `RetryPolicy` uses `with_*` setter names, because Rust cannot give a getter and a setter the same
  name; `new()` replaces the Python constructor's keyword arguments. Python's `exceptions` set maps to
  `retry_if`, which receives the whole `Error` value.
- Policies are validated when a client is built or a call is sent (yielding `Error::Config`) instead of
  in the constructor; `RetryPolicy::validate()` performs the same check up front.
- Answer kinds this SDK does not recognize are kept as `Answer::Unknown` instead of being dropped.
- Timeouts and retry delays are `std::time::Duration`, not seconds as floats, so there is no per-phase
  `httpx2.Timeout` equivalent; `Request timed out (timeout=10s).` also differs from Python's float form.
  A timeout inherited from a supplied HTTP client is reported as `timeout=none`.
- Redirects are not followed, matching the Python SDK.

**HTTP behaviour**

- An invalid `base_url` fails at build time with `Error::Config`, where Python surfaces it at request
  time; a supplied HTTP client without a timeout leaves requests unbounded (reqwest's default), and
  there is no SDK switch to disable the timeout other than supplying such a client.
- The SDK neither requests nor decodes compressed responses; enable reqwest's `gzip`/`brotli` features
  on a supplied client if a proxy compresses them.
- A response that repeats a singleton header reports the first value from `request_id()`; the retry
  layer reads repeated `Retry-After`/`retry-after-ms` values joined with `", "`, as httpx does.
- `Retry-After` numbers use Rust's float grammar, so PEP 515 underscores (`1_0`) are not accepted, and
  delays beyond `Duration::MAX` saturate instead of staying exact.
- An HTTP date without a timezone is read as UTC; Python reads it in the process's local time.
- `usage` token counts must be non-negative integers, where Python accepts any integer, and a raw error
  body containing an integer beyond 64 bits renders it in scientific notation.
- `ScoreAnswer::legend` values are kept as arbitrary JSON keyed by `i64`, where Python requires string,
  object, or array values and accepts arbitrarily large integer keys; both accept the same key forms
  (`-3`, `0`) and reject leading zeros, a leading `+`, and space. Iterating
  `SystemOneResponse::answers` yields name order (a `BTreeMap`), while decoding and error reporting
  follow the response's own order, as Python does.
- Credentials embedded in `base_url` are never sent: the API key always authenticates, where Python
  lets the URL's userinfo replace the `Authorization` header with HTTP basic credentials.
- Validation messages quote the offending path the way Python's `repr` does for quotes, backslashes,
  and tabs; other control characters are not escaped. `detail[].loc` entries that are neither strings
  nor integers render as JSON, where Python's `str()` prints `True`/`None`/`{'a': 1}`.

## License

MIT. See the `LICENSE` file in the repository.
