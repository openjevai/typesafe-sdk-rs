//! The models listing endpoint.

use std::time::Duration;

use http::{HeaderMap, Method};

use crate::client::{CallOptions, TypeSafeClient, call};
use crate::constants::MODELS_PATH;
use crate::error::Result;
use crate::response::ListModelsResponse;
use crate::retry::RetryPolicy;

/// Access to the models available to the account, reached through [`TypeSafeClient::models`].
///
/// ```
/// # use typesafe_sdk::TypeSafeClient;
/// # async fn example(client: TypeSafeClient) -> typesafe_sdk::Result<()> {
/// for model in client.models().list().send().await?.models {
///     println!("{}: {}", model.name, model.description);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Models {
    /// Client used to send the request.
    client: TypeSafeClient,
}

impl Models {
    /// Creates the resource bound to `client`.
    pub(crate) fn new(client: TypeSafeClient) -> Self {
        Self { client }
    }

    /// Starts listing the models available to the account.
    pub fn list(self) -> ListModelsRequest {
        ListModelsRequest {
            client: self.client,
            options: CallOptions::default(),
        }
    }
}

/// A models listing request being built.
#[derive(Clone, Debug)]
pub struct ListModelsRequest {
    /// Client used to send the request.
    client: TypeSafeClient,
    /// Per-call timeouts, retries, and headers.
    options: CallOptions,
}

impl ListModelsRequest {
    /// Sets the timeout for this call only, overriding the client value.
    ///
    /// Pass `None` to fall back to the client's timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.options.set_timeout(timeout);
        self
    }

    /// Sets the retry policy for this call only, overriding the client value.
    ///
    /// Pass `None` to fall back to the client's policy.
    #[must_use]
    pub fn retry(mut self, retry: impl Into<Option<RetryPolicy>>) -> Self {
        self.options.set_retry(retry);
        self
    }

    /// Adds a request header for this call only.
    ///
    /// Authentication, SDK identification, and `Accept` remain protected.
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

    /// Sends the request and returns the available models.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Api`] and its variants when the server returns an unsuccessful
    /// response after any retries, and [`crate::Error::Connection`] or [`crate::Error::Timeout`] when
    /// the request cannot reach the server.
    pub async fn send(self) -> Result<ListModelsResponse> {
        call(&self.client, &self.options, Method::GET, MODELS_PATH, None).await
    }
}
