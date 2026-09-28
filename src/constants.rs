//! Public environment-variable names, client defaults, and protocol constants.

use std::time::Duration;

/// Environment variable for the API key: `TYPESAFE_API_KEY`.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// Environment variable for the API base URL: `TYPESAFE_BASE_URL`.
pub const BASE_URL_ENV: &str = "TYPESAFE_BASE_URL";

/// Environment variable for the default model: `TYPESAFE_DEFAULT_MODEL`.
pub const DEFAULT_MODEL_ENV: &str = "TYPESAFE_DEFAULT_MODEL";

/// Environment variable naming the logging level: `TYPESAFE_LOG_LEVEL`.
///
/// Informational only: this SDK never configures a logger, because the application owns logging
/// levels. Set the level of the `typesafe_sdk` target through your logger of choice, for example
/// `RUST_LOG=typesafe_sdk=debug` with [`env_logger`](https://docs.rs/env_logger).
pub const LOG_LEVEL_ENV: &str = "TYPESAFE_LOG_LEVEL";

/// Default API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

/// Default model name.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// Environment variable for the OpenJEV API key: `OPENJEV_API_KEY`.
///
/// OpenJEV is a free community gateway to the same Jev model built by TypeSafe.
/// When `OPENJEV_API_KEY` is set (and `TYPESAFE_API_KEY` is not), the SDK uses OpenJEV
/// automatically. Set `JEV_PROVIDER=openjev` to force it.
pub const OPENJEV_API_KEY_ENV: &str = "OPENJEV_API_KEY";

/// Environment variable for explicit provider selection: `JEV_PROVIDER`.
///
/// Set to `openjev` to use OpenJEV, or `typesafe` to use TypeSafe (the default).
/// An explicit `.provider(...)` on the builder takes precedence over this variable.
pub const JEV_PROVIDER_ENV: &str = "JEV_PROVIDER";

/// Default API base URL for OpenJEV.
pub const OPENJEV_DEFAULT_BASE_URL: &str = "https://api.openjev.sh";

/// Default model name for OpenJEV.
pub const OPENJEV_DEFAULT_MODEL: &str = "openjev";

/// Default timeout applied to each HTTP operation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Path of the System One endpoint.
pub(crate) const SYSTEM_ONE_PATH: &str = "/v1/systemone";

/// Path of the models listing endpoint.
pub(crate) const MODELS_PATH: &str = "/v1/models";

/// SDK name used in `User-Agent` and `X-TypeSafe-SDK`.
pub(crate) const SDK_NAME: &str = "typesafe-sdk";

/// Log target used by every log record this crate emits.
pub(crate) const LOG_TARGET: &str = "typesafe_sdk";

/// Maximum length of a raw response body embedded in an error message.
pub(crate) const MAX_ERROR_BODY_LENGTH: usize = 200;

/// Media type used for request bodies and `Accept` headers.
pub(crate) const JSON_CONTENT_TYPE: &str = "application/json";

/// Header carrying the API key.
pub(crate) const AUTHORIZATION_HEADER: &str = "Authorization";

/// Header listing acceptable response media types.
pub(crate) const ACCEPT_HEADER: &str = "Accept";

/// Header describing a request body's media type.
pub(crate) const CONTENT_TYPE_HEADER: &str = "Content-Type";

/// Header identifying the HTTP client library.
pub(crate) const USER_AGENT_HEADER: &str = "User-Agent";

/// Header identifying the SDK.
pub(crate) const SDK_HEADER: &str = "X-TypeSafe-SDK";

/// Header identifying the language runtime.
pub(crate) const RUNTIME_HEADER: &str = "X-TypeSafe-Runtime";

/// Header reporting how many retries preceded this attempt.
pub(crate) const RETRY_COUNT_HEADER: &str = "X-TypeSafe-Retry-Count";

/// Response header carrying the server-assigned request ID.
pub(crate) const REQUEST_ID_HEADER: &str = "x-typesafe-request-id";

/// Response header carrying a delay in seconds or as an HTTP date.
pub(crate) const RETRY_AFTER_HEADER: &str = "retry-after";

/// Response header carrying a delay in milliseconds.
pub(crate) const RETRY_AFTER_MS_HEADER: &str = "retry-after-ms";

/// Header names whose values are always redacted from log output.
pub(crate) const SECRET_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "set-cookie",
];
