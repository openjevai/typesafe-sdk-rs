//! Errors returned by the SDK.
//!
//! Every fallible call returns [`Result`] with [`Error`], a single enum covering configuration
//! mistakes, invalid inputs, transport failures, HTTP failures, and structurally invalid responses.

use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;
use std::ops::Deref;
use std::time::Duration;

use http::{HeaderMap, StatusCode};
use serde_json::Value;

use crate::constants::{MAX_ERROR_BODY_LENGTH, REQUEST_ID_HEADER};
use crate::retry::parse_retry_after;

/// Result type used throughout this crate, defaulting to [`Error`].
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Everything that can go wrong when calling the TypeSafe AI API.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The client configuration is invalid, such as a missing API key.
    #[error(transparent)]
    Config(ConfigError),
    /// A request could not be built from the supplied inputs.
    #[error(transparent)]
    InvalidInput(InvalidInputError),
    /// The request failed without an HTTP response.
    #[error(transparent)]
    Connection(ConnectionError),
    /// The request exceeded its configured timeout.
    #[error(transparent)]
    Timeout(TimeoutError),
    /// The request was invalid (400).
    #[error(transparent)]
    BadRequest(ApiError),
    /// Authentication failed (401).
    #[error(transparent)]
    Authentication(ApiError),
    /// Access was denied (403).
    #[error(transparent)]
    PermissionDenied(ApiError),
    /// The resource was not found (404).
    #[error(transparent)]
    NotFound(ApiError),
    /// The request failed server validation (422).
    #[error(transparent)]
    UnprocessableEntity(ApiError),
    /// The rate limit was exceeded (429).
    #[error(transparent)]
    RateLimit(RateLimitError),
    /// The server failed to process the request (5xx).
    #[error(transparent)]
    InternalServerError(ApiError),
    /// The server returned any other unsuccessful status.
    #[error(transparent)]
    Api(ApiError),
    /// The server returned a successful response whose body is missing or structurally invalid.
    #[error(transparent)]
    ResponseValidation(ValidationError),
}

impl Error {
    /// Returns the unsuccessful HTTP status code, when the failure came from a response.
    pub fn status(&self) -> Option<StatusCode> {
        self.api().map(ApiError::status)
    }

    /// Returns the server's JSON error body, plain response text, or `None` for an empty body.
    pub fn body(&self) -> Option<&Value> {
        self.api().and_then(ApiError::body)
    }

    /// Returns the response headers, when the failure came from a response.
    pub fn headers(&self) -> Option<&HeaderMap> {
        self.api().map(ApiError::headers)
    }

    /// Returns the `x-typesafe-request-id` response header, when present.
    ///
    /// A response that repeats the header reports its first value; the retry layer uses the joined
    /// value instead, matching the Python SDK's client.
    pub fn request_id(&self) -> Option<&str> {
        self.api().and_then(ApiError::request_id)
    }

    /// Returns the request method and URL, without credentials, query parameters, or fragment.
    pub fn endpoint(&self) -> Option<&str> {
        self.api().and_then(ApiError::endpoint)
    }

    /// Returns the server's requested wait before retrying, for rate-limit failures.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimit(error) => error.retry_after(),
            _ => None,
        }
    }

    /// Returns whether this failure was caused by exceeding a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    /// Borrows the underlying API error, when this failure carries an HTTP response.
    fn api(&self) -> Option<&ApiError> {
        match self {
            Self::BadRequest(error)
            | Self::Authentication(error)
            | Self::PermissionDenied(error)
            | Self::NotFound(error)
            | Self::UnprocessableEntity(error)
            | Self::InternalServerError(error)
            | Self::Api(error) => Some(error),
            Self::RateLimit(error) => Some(error.api()),
            Self::ResponseValidation(error) => Some(error.api()),
            Self::Config(_) | Self::InvalidInput(_) | Self::Connection(_) | Self::Timeout(_) => {
                None
            }
        }
    }
}

/// The client configuration is incomplete or invalid.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ConfigError {
    /// Human-readable explanation of the configuration problem.
    message: String,
}

impl ConfigError {
    /// Creates a configuration error with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the explanation of the configuration problem.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A request could not be built from the supplied inputs.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct InvalidInputError {
    /// Human-readable explanation of the invalid input.
    message: String,
    /// The underlying encoding or conversion failure, when there was one.
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl InvalidInputError {
    /// Creates an invalid-input error with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Creates an invalid-input error with `message`, chaining `source`.
    pub(crate) fn from_source(
        message: impl Into<String>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Returns the explanation of the invalid input.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// An unsuccessful HTTP response with its body and request metadata.
#[derive(Clone)]
pub struct ApiError {
    /// Unsuccessful HTTP status code.
    status: StatusCode,
    /// The server's JSON error body, plain response text, or `None` for an empty body.
    body: Option<Value>,
    /// HTTP response headers.
    headers: HeaderMap,
    /// Error message, taken from the response body unless overridden.
    message: String,
    /// Request method and URL, without credentials, query parameters, or fragment.
    endpoint: Option<String>,
}

impl ApiError {
    /// Creates an API error, deriving the message from `body` when `message` is `None`.
    pub fn new(
        status: StatusCode,
        body: Option<Value>,
        headers: HeaderMap,
        message: Option<String>,
        endpoint: Option<String>,
    ) -> Self {
        let message = message.unwrap_or_else(|| resolve_message(body.as_ref()));
        Self {
            status,
            body,
            headers,
            message,
            endpoint,
        }
    }

    /// Returns the unsuccessful HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the server's JSON error body, plain response text, or `None` for an empty body.
    pub fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }

    /// Returns the HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Returns the error message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the `x-typesafe-request-id` response header, when present.
    pub fn request_id(&self) -> Option<&str> {
        self.headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
    }

    /// Returns the request method and URL, without credentials, query parameters, or fragment.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(endpoint) = &self.endpoint {
            write!(formatter, "{endpoint}: ")?;
        }
        if self.message.is_empty() {
            write!(formatter, "{}", self.status.as_u16())?;
        } else {
            write!(formatter, "{} {}", self.status.as_u16(), self.message)?;
        }
        if let Some(request_id) = self.request_id() {
            write!(formatter, " (request_id={request_id})")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ApiError {
    /// Renders the error without its response body or headers, which may hold credentials.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiError")
            .field("status", &self.status)
            .field("message", &self.message)
            .field("endpoint", &self.endpoint)
            .field("request_id", &self.request_id())
            .finish_non_exhaustive()
    }
}

impl StdError for ApiError {}

/// A rate-limited response (429) with the server's requested wait.
#[derive(Clone, Debug)]
pub struct RateLimitError {
    /// The underlying API error.
    api: ApiError,
    /// The server's requested wait, from `retry-after-ms` or `Retry-After`.
    retry_after: Option<Duration>,
}

impl RateLimitError {
    /// Creates a rate-limit error, reading the requested wait from `api.headers()`.
    pub fn new(api: ApiError) -> Self {
        let retry_after = parse_retry_after(api.headers());
        Self { api, retry_after }
    }

    /// Returns the server's requested wait before retrying.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// Returns the underlying API error.
    pub fn api(&self) -> &ApiError {
        &self.api
    }
}

impl Deref for RateLimitError {
    type Target = ApiError;

    fn deref(&self) -> &Self::Target {
        &self.api
    }
}

impl fmt::Display for RateLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.api.fmt(formatter)
    }
}

impl StdError for RateLimitError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.api)
    }
}

/// A successful HTTP response whose body is missing or structurally invalid required data.
#[derive(Clone, Debug)]
pub struct ValidationError {
    /// The underlying API error, whose message names the offending field.
    api: ApiError,
    /// Dotted path to the offending field, such as `answers.tone.confidence`.
    field_path: String,
    /// Why the field was rejected.
    reason: String,
}

impl ValidationError {
    /// Creates a validation error for the field at `field_path`.
    pub fn new(api: ApiError, field_path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            api,
            field_path: field_path.into(),
            reason: reason.into(),
        }
    }

    /// Returns the dotted path to the offending field.
    pub fn field_path(&self) -> &str {
        &self.field_path
    }

    /// Returns why the field was rejected.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Returns the underlying API error.
    pub fn api(&self) -> &ApiError {
        &self.api
    }
}

impl Deref for ValidationError {
    type Target = ApiError;

    fn deref(&self) -> &Self::Target {
        &self.api
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.api.fmt(formatter)
    }
}

impl StdError for ValidationError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.api)
    }
}

/// The request failed without an HTTP response.
#[derive(Debug, thiserror::Error)]
#[error("Connection error: {source}")]
pub struct ConnectionError {
    /// The failing transport error.
    source: reqwest::Error,
}

impl ConnectionError {
    /// Creates a connection error wrapping the transport failure.
    pub fn new(source: reqwest::Error) -> Self {
        Self { source }
    }
}

/// The request exceeded its configured timeout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeoutError {
    /// The timeout setting used for the request, when the SDK applied one.
    timeout: Option<Duration>,
}

impl TimeoutError {
    /// Creates a timeout error carrying the timeout setting used for the request.
    pub fn new(timeout: impl Into<Option<Duration>>) -> Self {
        Self {
            timeout: timeout.into(),
        }
    }

    /// Returns the timeout setting used for the request.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

impl fmt::Display for TimeoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.timeout {
            Some(timeout) => write!(formatter, "Request timed out (timeout={timeout:?})."),
            None => write!(formatter, "Request timed out (timeout=none)."),
        }
    }
}

impl StdError for TimeoutError {}

impl From<InvalidInputError> for Error {
    /// Wraps a request-building failure.
    fn from(error: InvalidInputError) -> Self {
        Self::InvalidInput(error)
    }
}

impl From<ConfigError> for Error {
    /// Wraps a configuration failure.
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<reqwest::Error> for Error {
    /// Maps a transport failure to [`Error::Timeout`] or [`Error::Connection`].
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout(TimeoutError::new(None))
        } else {
            Self::Connection(ConnectionError::new(error))
        }
    }
}

/// Reads a header, joining repeated values with `", "` the way the Python SDK's client does.
///
/// Borrows the value when the header appears once, which every well-behaved server does.
pub(crate) fn joined_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<Cow<'a, str>> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?.to_str().ok()?;
    let rest = values
        .map(|value| value.to_str().unwrap_or_default())
        .collect::<Vec<_>>();
    if rest.is_empty() {
        return Some(Cow::Borrowed(first));
    }
    let mut joined = String::from(first);
    for value in rest {
        joined.push_str(", ");
        joined.push_str(value);
    }
    Some(Cow::Owned(joined))
}

/// Renders `text` the way Python's `repr` renders a string, so messages match quote for quote.
pub(crate) fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut rendered = String::with_capacity(text.len() + 2);
    rendered.push(quote);
    for character in text.chars() {
        match character {
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            character if character == quote => {
                rendered.push('\\');
                rendered.push(character);
            }
            character => rendered.push(character),
        }
    }
    rendered.push(quote);
    rendered
}

/// Builds the API error message, preferring the server's message fields over the raw body.
pub(crate) fn resolve_message(body: Option<&Value>) -> String {
    if let Some(message) = body
        .and_then(extract_message)
        .filter(|message| !message.is_empty())
    {
        return message;
    }
    match body {
        None | Some(Value::Null) => "status code (no body)".to_owned(),
        Some(value) => truncate(match value {
            Value::String(text) => text.clone(),
            other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
        }),
    }
}

/// Shortens `raw` to [`MAX_ERROR_BODY_LENGTH`] characters, appending an ellipsis when cut.
fn truncate(raw: String) -> String {
    if raw.chars().count() <= MAX_ERROR_BODY_LENGTH {
        return raw;
    }
    let mut truncated: String = raw.chars().take(MAX_ERROR_BODY_LENGTH).collect();
    truncated.push('…');
    truncated
}

/// Extracts the server's error message from a response body.
///
/// Checks, in order: a plain string body, `error` as a string or as an object with a `message`,
/// `message`, `detail` as a string or as an object with a `message`, and `detail` as a list of
/// `{loc, msg}` entries joined with `"; "`.
pub(crate) fn extract_message(body: &Value) -> Option<String> {
    let Value::Object(map) = body else {
        return match body {
            Value::String(text) => Some(text.clone()),
            _ => None,
        };
    };
    let error = map.get("error");
    let detail = map.get("detail");
    if let Some(Value::String(error)) = error {
        return Some(error.clone());
    }
    if let Some(Value::Object(error)) = error {
        if let Some(Value::String(message)) = error.get("message") {
            return Some(message.clone());
        }
    }
    if let Some(Value::String(message)) = map.get("message") {
        return Some(message.clone());
    }
    if let Some(Value::String(detail)) = detail {
        return Some(detail.clone());
    }
    if let Some(Value::Object(detail)) = detail {
        if let Some(Value::String(message)) = detail.get("message") {
            return Some(message.clone());
        }
    }
    let Some(Value::Array(entries)) = detail else {
        return None;
    };
    let parts = entries
        .iter()
        .filter_map(|entry| {
            let entry = entry.as_object()?;
            let message = entry.get("msg")?.as_str()?;
            let path = entry
                .get("loc")
                .and_then(Value::as_array)
                .map(|location| {
                    location
                        .iter()
                        .map(render_location_segment)
                        .filter(|segment| segment != "body")
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .unwrap_or_default();
            Some(if path.is_empty() {
                message.to_owned()
            } else {
                format!("{path}: {message}")
            })
        })
        .collect::<Vec<_>>()
        .join("; ");
    (!parts.is_empty()).then_some(parts)
}

/// Renders one `detail[].loc` segment the way `str()` renders it in Python.
fn render_location_segment(segment: &Value) -> String {
    match segment {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Maps a non-success response onto the matching error variant.
pub(crate) fn classify(
    status: StatusCode,
    headers: &HeaderMap,
    body: Option<Value>,
    endpoint: Option<String>,
) -> Error {
    let api = || {
        ApiError::new(
            status,
            body.clone(),
            headers.clone(),
            None,
            endpoint.clone(),
        )
    };
    match status.as_u16() {
        400 => Error::BadRequest(api()),
        401 => Error::Authentication(api()),
        403 => Error::PermissionDenied(api()),
        404 => Error::NotFound(api()),
        422 => Error::UnprocessableEntity(api()),
        429 => Error::RateLimit(RateLimitError::new(api())),
        code if code >= 500 => Error::InternalServerError(api()),
        _ => Error::Api(api()),
    }
}

/// Describes the request method and URL without credentials, query parameters, or fragment.
pub(crate) fn endpoint_string(method: &http::Method, url: &reqwest::Url) -> String {
    let mut rendered = String::with_capacity(url.as_str().len());
    rendered.push_str(url.scheme());
    rendered.push_str("://");
    if let Some(host) = url.host_str() {
        rendered.push_str(host);
    }
    if let Some(port) = url.port() {
        rendered.push(':');
        rendered.push_str(&port.to_string());
    }
    rendered.push_str(url.path());
    format!("{method} {rendered}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn message_for(body: &Value) -> Option<String> {
        extract_message(body)
    }

    #[test]
    fn extracts_messages_in_priority_order() {
        assert_eq!(
            message_for(&json!("plain text")),
            Some("plain text".to_owned())
        );
        assert_eq!(message_for(&json!("")), Some(String::new()));
        assert_eq!(message_for(&json!(42)), None);
        assert_eq!(message_for(&json!({"error": "e1"})), Some("e1".to_owned()));
        assert_eq!(
            message_for(&json!({"error": {"message": "e2"}})),
            Some("e2".to_owned())
        );
        assert_eq!(
            message_for(&json!({"error": {"other": 1}, "message": "m"})),
            Some("m".to_owned())
        );
        assert_eq!(message_for(&json!({"message": "m"})), Some("m".to_owned()));
        assert_eq!(message_for(&json!({"detail": "d"})), Some("d".to_owned()));
        assert_eq!(
            message_for(&json!({"detail": {"message": "d2"}})),
            Some("d2".to_owned())
        );
        assert_eq!(message_for(&json!({"unknown": true})), None);
    }

    #[test]
    fn joins_validation_detail_entries() {
        assert_eq!(
            message_for(
                &json!({"detail": [{"loc": ["body", "questions", 0], "msg": "bad"}, {"loc": [], "msg": "other"}]})
            ),
            Some("questions.0: bad; other".to_owned())
        );
        assert_eq!(
            message_for(&json!({"detail": [null, 42, {"msg": 4}]})),
            None
        );
        assert_eq!(message_for(&json!({"detail": []})), None);
    }

    #[test]
    fn empty_messages_fall_back_to_the_raw_body() {
        // Python treats an empty extracted message as "not found" and renders the compact body.
        assert_eq!(
            resolve_message(Some(&json!({"error": "", "message": "ignored"}))),
            r#"{"error":"","message":"ignored"}"#
        );
        assert_eq!(
            resolve_message(Some(&json!({"detail": [null, 42, {"msg": 4}]}))),
            r#"{"detail":[null,42,{"msg":4}]}"#
        );
    }

    #[test]
    fn bodyless_and_plain_bodies_render_as_expected() {
        assert_eq!(resolve_message(None), "status code (no body)");
        assert_eq!(resolve_message(Some(&Value::Null)), "status code (no body)");
        assert_eq!(resolve_message(Some(&json!([]))), "[]");
        assert_eq!(resolve_message(Some(&json!(42))), "42");
        assert_eq!(
            resolve_message(Some(&json!("x".repeat(201)))),
            "x".repeat(201)
        );
    }

    #[test]
    fn long_unstructured_bodies_are_truncated() {
        let body = json!({"unknown": "x".repeat(201)});
        let raw = serde_json::to_string(&body).unwrap();
        let message = resolve_message(Some(&body));
        assert_eq!(message.len(), MAX_ERROR_BODY_LENGTH + '…'.len_utf8());
        assert_eq!(message, format!("{}…", &raw[..MAX_ERROR_BODY_LENGTH]));
    }

    #[test]
    fn repeated_headers_join_the_way_httpx_reads_them() {
        let mut headers = HeaderMap::new();
        headers.append(REQUEST_ID_HEADER, "req-1".parse().unwrap());
        assert_eq!(
            joined_header(&headers, REQUEST_ID_HEADER).as_deref(),
            Some("req-1")
        );
        headers.append(REQUEST_ID_HEADER, "req-2".parse().unwrap());
        assert_eq!(
            joined_header(&headers, REQUEST_ID_HEADER).as_deref(),
            Some("req-1, req-2")
        );
        assert_eq!(joined_header(&headers, "missing"), None);

        let api = ApiError::new(StatusCode::BAD_REQUEST, None, headers, None, None);
        assert_eq!(
            api.request_id(),
            Some("req-1"),
            "the accessor reports the first value"
        );
    }

    #[test]
    fn paths_are_quoted_the_way_python_repr_quotes_them() {
        assert_eq!(python_repr("answers.n.noul"), "'answers.n.noul'");
        assert_eq!(python_repr("answers.it's.noul"), "\"answers.it's.noul\"");
        assert_eq!(python_repr("both ' and \""), "'both \\' and \"'");
        assert_eq!(python_repr("line\nbreak"), "'line\\nbreak'");
    }

    #[test]
    fn raw_bodies_keep_document_order_and_big_integers_render_as_floats() {
        assert_eq!(
            resolve_message(Some(&json!({"zebra": 1, "alpha": 2}))),
            r#"{"zebra":1,"alpha":2}"#
        );
        // Literals beyond 64 bits come back from JSON parsing as floats, unlike Python's bignums, so
        // an unstructured body that carries one renders in scientific notation.
        let bignum: Value =
            serde_json::from_str(r#"{"count":123456789012345678901234567890}"#).unwrap();
        assert_eq!(
            resolve_message(Some(&bignum)),
            r#"{"count":1.2345678901234568e+29}"#
        );
    }

    #[test]
    fn extraction_prefers_error_over_message_and_detail() {
        assert_eq!(
            message_for(&json!({"error": "error", "message": "message", "detail": "detail"})),
            Some("error".to_owned())
        );
        assert_eq!(
            message_for(&json!({"message": "message", "detail": "detail"})),
            Some("message".to_owned())
        );
    }

    #[test]
    fn location_segments_that_are_not_strings_or_integers_render_as_json() {
        // The OpenAPI schema only ever puts strings and integers here; anything else renders as JSON,
        // where Python's `str()` would print `True`/`None`/`{'a': 1}`.
        assert_eq!(
            message_for(&json!({"detail": [{"loc": ["body", true], "msg": "bad"}]})),
            Some("true: bad".to_owned())
        );
        assert_eq!(
            message_for(&json!({"detail": [{"loc": [null], "msg": "bad"}]})),
            Some("null: bad".to_owned())
        );
    }

    #[test]
    fn display_includes_endpoint_and_request_id() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req-context".parse().unwrap());
        let error = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            Some(json!({"message": "Too many requests"})),
            headers,
            None,
            Some("GET https://api.example.test/prefix/v1/models".to_owned()),
        );
        assert_eq!(
            error.to_string(),
            "GET https://api.example.test/prefix/v1/models: 429 Too many requests (request_id=req-context)"
        );
        assert_eq!(error.request_id(), Some("req-context"));
    }

    #[test]
    fn display_degrades_without_message_endpoint_or_request_id() {
        let bare = ApiError::new(
            StatusCode::BAD_REQUEST,
            None,
            HeaderMap::new(),
            Some(String::new()),
            None,
        );
        assert_eq!(bare.to_string(), "400");
        let with_message = ApiError::new(
            StatusCode::BAD_REQUEST,
            None,
            HeaderMap::new(),
            Some("bad".to_owned()),
            None,
        );
        assert_eq!(with_message.to_string(), "400 bad");
    }

    #[test]
    fn debug_hides_body_and_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer private-api-key".parse().unwrap());
        let error = ApiError::new(
            StatusCode::BAD_REQUEST,
            Some(json!({"message": "Bad request", "body-only-field": "body-only-value"})),
            headers,
            None,
            Some("POST https://api.example.test/v1/systemone".to_owned()),
        );
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("private-api-key"), "{rendered}");
        assert!(!rendered.contains("body-only-value"), "{rendered}");
        assert!(
            rendered.contains("POST https://api.example.test/v1/systemone"),
            "{rendered}"
        );
    }

    #[test]
    fn endpoints_omit_credentials_query_and_fragment() {
        let url = reqwest::Url::parse(
            "https://user:password@example.test:8443/v1/models?token=secret#fragment",
        )
        .unwrap();
        assert_eq!(
            endpoint_string(&http::Method::GET, &url),
            "GET https://example.test:8443/v1/models"
        );
        let url = reqwest::Url::parse("https://api.typesafe.ai/v1/systemone").unwrap();
        assert_eq!(
            endpoint_string(&http::Method::POST, &url),
            "POST https://api.typesafe.ai/v1/systemone"
        );
    }

    #[test]
    fn statuses_map_to_variants() {
        let cases = [
            (400, "BadRequest"),
            (401, "Authentication"),
            (403, "PermissionDenied"),
            (404, "NotFound"),
            (422, "UnprocessableEntity"),
            (429, "RateLimit"),
            (500, "InternalServerError"),
            (599, "InternalServerError"),
            (302, "Api"),
            (409, "Api"),
        ];
        for (status, expected) in cases {
            let error = classify(
                StatusCode::from_u16(status).unwrap(),
                &HeaderMap::new(),
                Some(json!({})),
                None,
            );
            let rendered = format!("{error:?}");
            assert!(rendered.starts_with(expected), "{status} → {rendered}");
        }
    }

    #[test]
    fn an_explicit_message_override_survives_on_rate_limits() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", "125".parse().unwrap());
        let api = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            Some(json!({"message": "server"})),
            headers.clone(),
            Some(String::new()),
            None,
        );
        let rate_limit = RateLimitError::new(api);
        assert_eq!(rate_limit.to_string(), "429");
        assert_eq!(rate_limit.retry_after(), Some(Duration::from_millis(125)));
        let named = RateLimitError::new(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            headers,
            Some("custom".to_owned()),
            None,
        ));
        assert_eq!(named.to_string(), "429 custom");
    }

    #[test]
    fn timeouts_render_their_setting() {
        assert_eq!(
            TimeoutError::new(Some(Duration::from_secs(10))).to_string(),
            "Request timed out (timeout=10s)."
        );
        assert_eq!(
            TimeoutError::new(None).to_string(),
            "Request timed out (timeout=none)."
        );
    }
}
