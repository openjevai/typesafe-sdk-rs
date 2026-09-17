//! The asynchronous client, its builder, and the shared request/retry loop.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::{HeaderMap, HeaderName, HeaderValue, Method};
use reqwest::Url;
use serde_json::Value;

use crate::config::{Config, ConfigInput};
use crate::constants::{LOG_TARGET, REQUEST_ID_HEADER};
use crate::error::{
    ConfigError, ConnectionError, Error, InvalidInputError, Result, TimeoutError, endpoint_string,
};
use crate::http::PreparedRequest;
use crate::json::decode_body;
use crate::logging::redact_headers;
use crate::models::Models;
use crate::request::SystemOneRequest;
use crate::response::{DecodeResponse, RawResponse};
use crate::retry::{Decision, RetryPolicy, decide};

/// Asynchronous client for the [TypeSafe AI](https://typesafe.ai) API.
///
/// Explicit options take precedence over environment variables; empty or whitespace-only environment
/// values are ignored. The client is cheap to clone and shares one HTTP connection pool.
///
/// ```no_run
/// # async fn example() -> typesafe_sdk::Result<()> {
/// use typesafe_sdk::{Noul, TypeSafeClient};
///
/// let client = TypeSafeClient::new("sk-...")?;
/// let response = client
///     .system_one()
///     .state("I was charged twice.")
///     .question("billing", Noul::new().instructions("Is this about billing?"))
///     .send()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TypeSafeClient {
    /// Shared client state.
    inner: Arc<Inner>,
}

/// State shared by every clone of a client.
struct Inner {
    /// Resolved configuration.
    config: Config,
    /// Parsed form of [`Config::base_url`], exposed through [`TypeSafeClient::base_url`].
    base_url: Url,
    /// Retry policy applied to every call that does not override it.
    retry: RetryPolicy,
    /// HTTP client used for every request.
    http_client: reqwest::Client,
}

impl TypeSafeClient {
    /// Creates a client with the given API key and the SDK defaults.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the key is missing or a setting is invalid.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Self::builder().api_key(api_key).build()
    }

    /// Creates a client from `TYPESAFE_API_KEY`, `TYPESAFE_BASE_URL`, and `TYPESAFE_DEFAULT_MODEL`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the API key is not set or a setting is invalid.
    pub fn from_env() -> Result<Self> {
        Self::builder().build()
    }

    /// Creates a builder for a client with custom options.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Returns the API root, without trailing slashes.
    pub fn base_url(&self) -> &Url {
        &self.inner.base_url
    }

    /// Returns the model used when a call does not override it.
    pub fn default_model(&self) -> &str {
        self.inner.config.default_model()
    }

    /// Returns the retry policy applied to every call that does not override it.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.inner.retry
    }

    /// Returns the HTTP client used for every request.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.inner.http_client
    }

    /// Starts a System One request answering named questions about text or structured state.
    ///
    /// ```
    /// # use typesafe_sdk::{Noul, TypeSafeClient};
    /// # fn example(client: TypeSafeClient) {
    /// let request = client
    ///     .system_one()
    ///     .state("I was charged twice.")
    ///     .question("billing", Noul::new().instructions("Is this about billing?"));
    /// # let _ = request;
    /// # }
    /// ```
    pub fn system_one(&self) -> SystemOneRequest {
        SystemOneRequest::new(self.clone())
    }

    /// Starts listing the models available to the account.
    ///
    /// ```
    /// # use typesafe_sdk::TypeSafeClient;
    /// # async fn example(client: TypeSafeClient) -> typesafe_sdk::Result<()> {
    /// let models = client.models().list().send().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn models(&self) -> Models {
        Models::new(self.clone())
    }

    /// Returns the resolved configuration.
    pub(crate) fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Sends a prepared request, decoding the response and retrying per `retry` or the client policy.
    pub(crate) async fn send<T: DecodeResponse>(
        &self,
        request: PreparedRequest,
        retry: Option<&RetryPolicy>,
    ) -> Result<T> {
        let policy = retry.unwrap_or(&self.inner.retry);
        policy.validate().map_err(Error::Config)?;
        let endpoint = endpoint_string(&request.method, &request.url);
        let started = Instant::now();
        let mut attempt = 0_u32;
        loop {
            attempt += 1;
            let mut headers = request.headers.clone();
            if attempt > 1 {
                headers.insert(
                    HeaderName::from_static("x-typesafe-retry-count"),
                    HeaderValue::from(attempt - 1),
                );
                log::info!(target: LOG_TARGET, "{} {} retry {}", request.method, request.url, attempt - 1);
            }
            if log::log_enabled!(target: LOG_TARGET, log::Level::Debug) {
                log::debug!(
                    target: LOG_TARGET,
                    "{} {} -> headers={} body={}",
                    request.method,
                    request.url,
                    redact_headers(&headers),
                    body_summary(request.body.as_deref()),
                );
            }
            let outcome =
                attempt_once::<T>(&self.inner.http_client, &request, headers, &endpoint).await;
            match outcome {
                Ok(response) => return Ok(response),
                Err(error) => match decide(policy, attempt, started.elapsed(), &error) {
                    Decision::Retry(delay) => tokio::time::sleep(delay).await,
                    Decision::Stop => return Err(error),
                },
            }
        }
    }
}

impl fmt::Debug for TypeSafeClient {
    /// Renders the client's settings without the API key.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypeSafeClient")
            .field("config", &self.inner.config)
            .field("retry", &self.inner.retry)
            .finish_non_exhaustive()
    }
}

/// Builder for [`TypeSafeClient`].
///
/// ```
/// use std::time::Duration;
/// use typesafe_sdk::{RetryPolicy, TypeSafeClient};
///
/// # fn example() -> typesafe_sdk::Result<()> {
/// let client = TypeSafeClient::builder()
///     .api_key("sk-...")
///     .model("jev-preview")
///     .timeout(Duration::from_secs(30))
///     .header("X-Client", "billing-service")
///     .retry(RetryPolicy::new().with_max_retries(4))
///     .build()?;
/// # let _ = client;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default)]
pub struct ClientBuilder {
    /// Settings resolved into the client's configuration.
    input: ConfigInput,
    /// Retry policy; the SDK defaults apply when unset.
    retry: Option<RetryPolicy>,
    /// Caller-supplied HTTP client, whose defaults are inherited.
    http_client: Option<reqwest::Client>,
}

impl ClientBuilder {
    /// Sets the API key.
    #[must_use]
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.input.api_key = Some(api_key.into());
        self
    }

    /// Sets the API root; trailing slashes are stripped.
    #[must_use]
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.input.base_url = Some(base_url.into());
        self
    }

    /// Sets the model used when a call does not override it.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.input.model = Some(model.into());
        self
    }

    /// Sets the retry policy applied to every call that does not override it.
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// Sets the timeout applied to each HTTP operation.
    ///
    /// A caller-supplied [`Self::http_client`] keeps its own timeout when this is not set.
    #[must_use]
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.input.timeout = timeout.into();
        self
    }

    /// Sets the timeout applied to connecting.
    ///
    /// Applies to HTTP clients this builder creates; a client supplied with [`Self::http_client`]
    /// owns its own settings, including its connect timeout.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.input.connect_timeout = timeout.into();
        self
    }

    /// Adds a header applied to every request; the last value set for a name wins.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.input.header_pairs.push((name.into(), value.into()));
        self
    }

    /// Adds headers applied to every request; they override headers set with [`Self::header`].
    #[must_use]
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.input.headers = Some(headers);
        self
    }

    /// Uses `client` for every request, inheriting its timeout and default headers.
    #[must_use]
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Splits the builder into its settings, dropping any async HTTP client.
    #[cfg(feature = "blocking")]
    pub(crate) fn into_parts(self) -> (ConfigInput, Option<RetryPolicy>) {
        (self.input, self.retry)
    }

    /// Builds a blocking client, mirroring this builder's settings.
    #[cfg(feature = "blocking")]
    pub fn build_blocking(self) -> Result<crate::blocking::TypeSafeClient> {
        crate::blocking::TypeSafeClient::from_builder(self)
    }

    /// Builds the client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Config`] when the API key is missing or a setting is invalid, such as a
    /// zero timeout or a base URL that is not an absolute `http(s)` URL.
    pub fn build(self) -> Result<TypeSafeClient> {
        let Self {
            input,
            retry,
            http_client,
        } = self;
        let retry = retry.unwrap_or_default();
        retry.validate().map_err(Error::Config)?;
        let config = Config::resolve(input, http_client.is_some()).map_err(Error::Config)?;
        let base_url = Url::parse(config.base_url()).map_err(|_| {
            Error::Config(ConfigError::new(format!(
                "base_url must be an absolute http(s) URL: {}",
                config.base_url()
            )))
        })?;
        let http_client = match http_client {
            Some(client) => client,
            None => {
                let mut builder =
                    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
                if let Some(timeout) = config.timeout() {
                    builder = builder.timeout(timeout);
                }
                if let Some(connect_timeout) = config.connect_timeout() {
                    builder = builder.connect_timeout(connect_timeout);
                }
                builder.build().map_err(|error| {
                    Error::Config(ConfigError::new(format!(
                        "the HTTP client could not be created: {error}"
                    )))
                })?
            }
        };
        Ok(TypeSafeClient {
            inner: Arc::new(Inner {
                config,
                base_url,
                retry,
                http_client,
            }),
        })
    }
}

impl fmt::Debug for ClientBuilder {
    /// Renders the builder's settings without the API key or header values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientBuilder")
            .field("api_key", &self.input.api_key.as_ref().map(|_| "***"))
            .field("base_url", &self.input.base_url)
            .field("model", &self.input.model)
            .field("retry", &self.retry)
            .field("timeout", &self.input.timeout)
            .field("connect_timeout", &self.input.connect_timeout)
            .field(
                "header_pairs",
                &crate::logging::redact_pairs(self.input.header_pairs.iter().cloned()),
            )
            .field(
                "headers",
                &self
                    .input
                    .headers
                    .as_ref()
                    .map(crate::logging::redact_headers),
            )
            .field(
                "http_client",
                &self.http_client.as_ref().map(|_| "<supplied>"),
            )
            .finish()
    }
}

/// Per-call settings shared by the request builders.
#[derive(Clone, Default)]
pub(crate) struct CallOptions {
    /// Timeout overriding the client's, or `None` to inherit it.
    timeout: Option<Duration>,
    /// Retry policy overriding the client's, or `None` to inherit it.
    retry: Option<RetryPolicy>,
    /// Headers set one name at a time, in the order they were set.
    header_pairs: Vec<(String, String)>,
    /// Headers set as a map, overriding headers set one name at a time.
    headers: HeaderMap,
}

impl fmt::Debug for CallOptions {
    /// Renders the per-call settings without credential-bearing header values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallOptions")
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .field(
                "header_pairs",
                &crate::logging::redact_pairs(self.header_pairs.iter().cloned()),
            )
            .field("headers", &crate::logging::redact_headers(&self.headers))
            .finish()
    }
}

impl CallOptions {
    /// Sets the per-call timeout.
    pub(crate) fn set_timeout(&mut self, timeout: impl Into<Option<Duration>>) {
        self.timeout = timeout.into();
    }

    /// Sets the per-call retry policy.
    pub(crate) fn set_retry(&mut self, retry: impl Into<Option<RetryPolicy>>) {
        self.retry = retry.into();
    }

    /// Adds a per-call header.
    pub(crate) fn set_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.header_pairs.push((name.into(), value.into()));
    }

    /// Adds per-call headers, overriding headers added one name at a time.
    pub(crate) fn set_headers(&mut self, headers: HeaderMap) {
        for (name, value) in &headers {
            self.headers.insert(name.clone(), value.clone());
        }
    }

    /// Returns the per-call retry policy, when one is set.
    pub(crate) fn retry(&self) -> Option<&RetryPolicy> {
        self.retry.as_ref()
    }

    /// Builds the prepared request for one call.
    pub(crate) fn prepare(
        &self,
        config: &Config,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<PreparedRequest, InvalidInputError> {
        crate::http::prepare(
            config,
            method,
            path,
            body,
            self.timeout,
            &self.header_pairs,
            &self.headers,
        )
    }
}

/// Builds and sends one request, returning a decoded response.
pub(crate) async fn call<T: DecodeResponse>(
    client: &TypeSafeClient,
    options: &CallOptions,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<T> {
    let request = options.prepare(client.config(), method, path, body)?;
    client.send(request, options.retry()).await
}

/// Sends one attempt and decodes its response.
async fn attempt_once<T: DecodeResponse>(
    http_client: &reqwest::Client,
    request: &PreparedRequest,
    headers: HeaderMap,
    endpoint: &str,
) -> Result<T> {
    let started = Instant::now();
    let mut builder = http_client
        .request(request.method.clone(), request.url.clone())
        .headers(headers);
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    if let Some(timeout) = request.timeout {
        builder = builder.timeout(timeout);
    }
    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => {
            log_transport_failure(request, &error);
            return Err(transport_error(error, request.timeout));
        }
    };
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            log_transport_failure(request, &error);
            return Err(transport_error(error, request.timeout));
        }
    };
    let request_id = response_headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    log::info!(
        target: LOG_TARGET,
        "{} {} <- {} in {:.0}ms (request {})",
        request.method,
        request.url,
        status.as_u16(),
        started.elapsed().as_secs_f64() * 1000.0,
        request_id,
    );
    if log::log_enabled!(target: LOG_TARGET, log::Level::Debug) {
        log::debug!(
            target: LOG_TARGET,
            "{} {} <- headers={} body={}",
            request.method,
            request.url,
            redact_headers(&response_headers),
            body_summary(Some(&bytes)),
        );
    }
    T::decode_response(
        RawResponse::new(status, response_headers, decode_body(&bytes)),
        endpoint,
    )
}

/// Logs a transport failure at info level, naming its kind the way the Python SDK names it.
pub(crate) fn log_transport_failure(request: &PreparedRequest, error: &reqwest::Error) {
    log::info!(target: LOG_TARGET, "{} {} <- {}", request.method, request.url, error_kind(error));
}

/// Maps a transport failure onto [`Error::Timeout`] or [`Error::Connection`].
pub(crate) fn transport_error(error: reqwest::Error, timeout: Option<Duration>) -> Error {
    if error.is_timeout() {
        Error::Timeout(TimeoutError::new(timeout))
    } else {
        Error::Connection(ConnectionError::new(error))
    }
}

/// Names a transport failure for logging.
pub(crate) fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "Timeout"
    } else if error.is_connect() {
        "Connect"
    } else if error.is_body() {
        "Body"
    } else if error.is_decode() {
        "Decode"
    } else if error.is_redirect() {
        "Redirect"
    } else if error.is_request() {
        "Request"
    } else {
        "Error"
    }
}

/// Renders a request or response body for logging.
pub(crate) fn body_summary(body: Option<&[u8]>) -> String {
    match body {
        None => "none".to_owned(),
        Some(body) => format!("{:?}", String::from_utf8_lossy(body)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_do_not_leak_credentials() {
        let builder = ClientBuilder::default()
            .api_key("private-api-key")
            .header("x-token", "private-token")
            .header("x-visible", "visible");
        let rendered = format!("{builder:?}");
        assert!(!rendered.contains("private-api-key"), "{rendered}");
        assert!(!rendered.contains("private-token"), "{rendered}");
        assert!(rendered.contains("visible"), "{rendered}");
    }

    #[test]
    fn header_maps_override_earlier_pairs() {
        let mut options = CallOptions::default();
        options.set_header("x-call", "from-pair");
        let mut headers = HeaderMap::new();
        headers.insert("x-call", "from-map".parse().unwrap());
        options.set_headers(headers);
        assert_eq!(options.headers.get("x-call").unwrap(), "from-map");
        assert_eq!(options.header_pairs.len(), 1);
    }

    #[test]
    fn request_builders_do_not_leak_credentials() {
        let client = TypeSafeClient::builder()
            .api_key("private-api-key")
            .base_url("http://127.0.0.1:9")
            .header("x-token", "private-token")
            .build()
            .unwrap();
        let request = client
            .system_one()
            .state("x")
            .header("x-secret", "private-secret");
        let rendered = format!("{request:?}");
        for secret in ["private-api-key", "private-token", "private-secret"] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert!(rendered.contains("x-secret"), "{rendered}");
    }
}
