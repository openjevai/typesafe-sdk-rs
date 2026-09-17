//! Blocking client for the TypeSafe AI API, mirroring the asynchronous one.
//!
//! Every call blocks the current thread, including retry backoff. Enable the `blocking` feature:
//!
//! ```toml
//! typesafe-sdk = { version = "0.1", features = ["blocking"] }
//! ```
//!
//! A blocking client must not be created, used, or dropped inside an asynchronous runtime — the
//! underlying blocking HTTP client panics on its own runtime shutdown. Run asynchronous work in a
//! runtime that is dropped first, or use the asynchronous client in async code.
//!
//! ```ignore
//! use typesafe_sdk::blocking::TypeSafeClient;
//! use typesafe_sdk::{Choice, Noul};
//!
//! let client = TypeSafeClient::from_env()?;
//! let response = client
//!     .system_one()
//!     .state("I was charged twice.")
//!     .question("category", Choice::new(["billing", "technical"]))
//!     .question("urgent", Noul::new())
//!     .send()?;
//!
//! if let Some(answer) = response.choice("category") {
//!     println!("{}", answer.choice);
//! }
//! ```

use std::fmt;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use http::{HeaderMap, HeaderName, HeaderValue, Method};
use reqwest::Url;
use reqwest::blocking::Client as BlockingHttpClient;
use serde_json::Value;

use crate::client::{
    CallOptions, ClientBuilder as AsyncClientBuilder, body_summary, log_transport_failure,
    transport_error,
};
use crate::config::{Config, ConfigInput};
use crate::constants::{LOG_TARGET, MODELS_PATH, REQUEST_ID_HEADER, SYSTEM_ONE_PATH};
use crate::error::{ConfigError, Result};
use crate::http::PreparedRequest;
use crate::json::{JsonContent, decode_body};
use crate::logging::redact_headers;
use crate::question::Question;
use crate::request::SystemOneSpec;
use crate::response::{DecodeResponse, ListModelsResponse, RawResponse, SystemOneResponse};
use crate::retry::{Decision, RetryPolicy, decide};

/// Blocking client for the [TypeSafe AI](https://typesafe.ai) API.
///
/// Settings resolve exactly like the asynchronous client's: explicit options first, then non-empty
/// environment values, then the SDK defaults.
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
    http_client: BlockingHttpClient,
}

impl TypeSafeClient {
    /// Creates a client with the given API key and the SDK defaults.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Config`] when the key is missing or a setting is invalid.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        Self::builder().api_key(api_key).build()
    }

    /// Creates a client from `TYPESAFE_API_KEY`, `TYPESAFE_BASE_URL`, and `TYPESAFE_DEFAULT_MODEL`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Config`] when the API key is not set or a setting is invalid.
    pub fn from_env() -> Result<Self> {
        Self::builder().build()
    }

    /// Creates a builder for a blocking client with custom options.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Creates a blocking client from an asynchronous client builder's settings, ignoring any
    /// HTTP client supplied for the asynchronous client.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Config`] when the API key is missing or a setting is invalid.
    pub(crate) fn from_builder(builder: AsyncClientBuilder) -> Result<Self> {
        let (input, retry) = builder.into_parts();
        Self::from_input(input, retry, None)
    }

    /// Builds a client from resolved settings and an optional HTTP client.
    fn from_input(
        input: ConfigInput,
        retry: Option<RetryPolicy>,
        http_client: Option<BlockingHttpClient>,
    ) -> Result<Self> {
        let retry = retry.unwrap_or_default();
        retry.validate()?;
        let config = Config::resolve(input, http_client.is_some())?;
        let base_url = Url::parse(config.base_url()).map_err(|_| {
            ConfigError::new(format!(
                "base_url must be an absolute http(s) URL: {}",
                config.base_url()
            ))
        })?;
        let http_client = match http_client {
            Some(client) => client,
            None => {
                let mut builder =
                    BlockingHttpClient::builder().redirect(reqwest::redirect::Policy::none());
                if let Some(timeout) = config.timeout() {
                    builder = builder.timeout(timeout);
                }
                if let Some(connect_timeout) = config.connect_timeout() {
                    builder = builder.connect_timeout(connect_timeout);
                }
                builder.build().map_err(|error| {
                    ConfigError::new(format!("the HTTP client could not be created: {error}"))
                })?
            }
        };
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                base_url,
                retry,
                http_client,
            }),
        })
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
    pub fn http_client(&self) -> &BlockingHttpClient {
        &self.inner.http_client
    }

    /// Starts a System One request answering named questions about text or structured state.
    pub fn system_one(&self) -> SystemOneRequest {
        SystemOneRequest {
            client: self.clone(),
            spec: SystemOneSpec::default(),
        }
    }

    /// Starts listing the models available to the account.
    pub fn models(&self) -> Models {
        Models {
            client: self.clone(),
        }
    }

    /// Returns the resolved configuration.
    pub(crate) fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Sends a prepared request, decoding the response and retrying per `retry` or the client policy.
    pub(crate) fn send<T: DecodeResponse>(
        &self,
        request: PreparedRequest,
        retry: Option<&RetryPolicy>,
    ) -> Result<T> {
        let policy = retry.unwrap_or(&self.inner.retry);
        policy.validate()?;
        let endpoint = crate::error::endpoint_string(&request.method, &request.url);
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
            let outcome = self.attempt_once::<T>(&request, headers, &endpoint);
            match outcome {
                Ok(response) => return Ok(response),
                Err(error) => match decide(policy, attempt, started.elapsed(), &error) {
                    Decision::Retry(delay) => thread::sleep(delay),
                    Decision::Stop => return Err(error),
                },
            }
        }
    }

    /// Sends one attempt and decodes its response.
    fn attempt_once<T: DecodeResponse>(
        &self,
        request: &PreparedRequest,
        headers: HeaderMap,
        endpoint: &str,
    ) -> Result<T> {
        let started = Instant::now();
        let mut builder = self
            .inner
            .http_client
            .request(request.method.clone(), request.url.clone())
            .headers(headers);
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        let response = match builder.send() {
            Ok(response) => response,
            Err(error) => {
                log_transport_failure(request, &error);
                return Err(transport_error(error, request.timeout));
            }
        };
        let status = response.status();
        let response_headers = response.headers().clone();
        let bytes = match response.bytes() {
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

/// Builder for the blocking [`TypeSafeClient`].
///
/// ```
/// use std::time::Duration;
/// use typesafe_sdk::blocking::TypeSafeClient;
/// use typesafe_sdk::RetryPolicy;
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
    http_client: Option<BlockingHttpClient>,
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
    pub fn http_client(mut self, client: BlockingHttpClient) -> Self {
        self.http_client = Some(client);
        self
    }

    /// Builds the client.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Config`] when the API key is missing or a setting is invalid, such as a
    /// zero timeout or a base URL that is not an absolute `http(s)` URL.
    pub fn build(self) -> Result<TypeSafeClient> {
        let Self {
            input,
            retry,
            http_client,
        } = self;
        TypeSafeClient::from_input(input, retry, http_client)
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

/// A System One request being built, sent by blocking the current thread.
#[derive(Clone, Debug)]
pub struct SystemOneRequest {
    /// Client used to send the request.
    client: TypeSafeClient,
    /// The request being built.
    spec: SystemOneSpec,
}

impl SystemOneRequest {
    /// Sets the text, JSON object, or array to evaluate.
    #[must_use]
    pub fn state(mut self, state: impl Into<JsonContent>) -> Self {
        self.spec.state = Some(state.into());
        self
    }

    /// Adds a question identified by `name`, replacing any question already using that name.
    #[must_use]
    pub fn question(mut self, name: impl Into<String>, question: impl Into<Question>) -> Self {
        self.spec.questions.push((name.into(), question.into()));
        self
    }

    /// Adds several questions, keyed by the names used to identify their answers.
    #[must_use]
    pub fn questions<I, N, Q>(mut self, questions: I) -> Self
    where
        I: IntoIterator<Item = (N, Q)>,
        N: Into<String>,
        Q: Into<Question>,
    {
        self.spec.questions.extend(
            questions
                .into_iter()
                .map(|(name, question)| (name.into(), question.into())),
        );
        self
    }

    /// Sets the model, overriding the client default for this call only.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.spec.model = Some(model.into());
        self
    }

    /// Sets the timeout for this call only, overriding the client value.
    #[must_use]
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.spec.options.set_timeout(timeout);
        self
    }

    /// Sets the retry policy for this call only, overriding the client value.
    #[must_use]
    pub fn retry(mut self, retry: impl Into<Option<RetryPolicy>>) -> Self {
        self.spec.options.set_retry(retry);
        self
    }

    /// Adds a request header for this call only.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.spec.options.set_header(name, value);
        self
    }

    /// Adds request headers for this call only, overriding headers added with [`Self::header`].
    #[must_use]
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.spec.options.set_headers(headers);
        self
    }

    /// Adds top-level request-body fields, shallow-merged over the body.
    #[must_use]
    pub fn extra_body(mut self, body: impl Into<Value>) -> Self {
        self.spec.extra_body = Some(body.into());
        self
    }

    /// Adds one top-level request-body field, applied after [`Self::extra_body`].
    #[must_use]
    pub fn extra_field(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.spec.extra_fields.push((key.into(), value.into()));
        self
    }

    /// Sends the request, blocking until the response arrives.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidInput`] for malformed requests, [`crate::Error::Api`] and its
    /// variants for unsuccessful responses after any retries, and [`crate::Error::Connection`] or
    /// [`crate::Error::Timeout`] when the request cannot reach the server.
    pub fn send(self) -> Result<SystemOneResponse> {
        let model = self.client.default_model().to_owned();
        let body = self.spec.body(&model)?;
        let request = self.spec.options.prepare(
            self.client.config(),
            Method::POST,
            SYSTEM_ONE_PATH,
            Some(body),
        )?;
        self.client.send(request, self.spec.options.retry())
    }
}

/// Access to the models available to the account, reached through [`TypeSafeClient::models`].
#[derive(Clone, Debug)]
pub struct Models {
    /// Client used to send the request.
    client: TypeSafeClient,
}

impl Models {
    /// Starts listing the models available to the account.
    pub fn list(self) -> ListModelsRequest {
        ListModelsRequest {
            client: self.client,
            options: CallOptions::default(),
        }
    }
}

/// A models listing request being built, sent by blocking the current thread.
#[derive(Clone, Debug)]
pub struct ListModelsRequest {
    /// Client used to send the request.
    client: TypeSafeClient,
    /// Per-call timeouts, retries, and headers.
    options: CallOptions,
}

impl ListModelsRequest {
    /// Sets the timeout for this call only, overriding the client value.
    #[must_use]
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.options.set_timeout(timeout);
        self
    }

    /// Sets the retry policy for this call only, overriding the client value.
    #[must_use]
    pub fn retry(mut self, retry: impl Into<Option<RetryPolicy>>) -> Self {
        self.options.set_retry(retry);
        self
    }

    /// Adds a request header for this call only.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.options.set_header(name, value);
        self
    }

    /// Adds request headers for this call only, overriding headers added with [`Self::header`].
    #[must_use]
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.options.set_headers(headers);
        self
    }

    /// Sends the request, blocking until the response arrives.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Api`] and its variants for unsuccessful responses after any retries,
    /// and [`crate::Error::Connection`] or [`crate::Error::Timeout`] when the request cannot reach
    /// the server.
    pub fn send(self) -> Result<ListModelsResponse> {
        let request = self
            .options
            .prepare(self.client.config(), Method::GET, MODELS_PATH, None)?;
        self.client.send(request, self.options.retry())
    }
}
