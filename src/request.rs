//! The System One request builder.

use std::time::Duration;

use http::{HeaderMap, Method};
use serde_json::{Map, Value};

use crate::client::{CallOptions, TypeSafeClient, call};
use crate::constants::SYSTEM_ONE_PATH;
use crate::error::{InvalidInputError, Result};
use crate::json::JsonContent;
use crate::question::{self, Question};
use crate::response::SystemOneResponse;
use crate::retry::RetryPolicy;

/// A System One request being built, answering named questions about text or structured state.
///
/// See [System One](https://docs.typesafe.ai/concepts/system-one) for details.
///
/// ```
/// use typesafe_sdk::{Choice, Noul, Score, TypeSafeClient};
///
/// # fn example(client: TypeSafeClient) {
/// let request = client
///     .system_one()
///     .state("I was charged twice. Please fix this ASAP.")
///     .question(
///         "category",
///         Choice::new(["billing", "technical", "other"]).instructions("What is this ticket about?"),
///     )
///     .question("urgent", Noul::new().instructions("Is this urgent?"))
///     .question("quality", Score::new(["unusable", "usable", "excellent"]));
/// # let _ = request;
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct SystemOneRequest {
    /// Client used to send the request.
    client: TypeSafeClient,
    /// The request being built.
    spec: SystemOneSpec,
}

/// Everything a [`SystemOneRequest`] carries until it is sent, shared with the blocking client.
#[derive(Clone, Debug, Default)]
pub(crate) struct SystemOneSpec {
    /// Text or structured state to evaluate.
    pub(crate) state: Option<JsonContent>,
    /// Questions keyed by the names used to identify their answers.
    pub(crate) questions: Vec<(String, Question)>,
    /// Model overriding the client default.
    pub(crate) model: Option<String>,
    /// Top-level request-body fields, shallow-merged over the body.
    pub(crate) extra_body: Option<Value>,
    /// Single top-level request-body fields, applied after `extra_body`.
    pub(crate) extra_fields: Vec<(String, Value)>,
    /// Per-call timeouts, retries, and headers.
    pub(crate) options: CallOptions,
}

impl SystemOneRequest {
    /// Creates a request bound to `client`.
    pub(crate) fn new(client: TypeSafeClient) -> Self {
        Self {
            client,
            spec: SystemOneSpec::default(),
        }
    }

    /// Sets the text, JSON object, or array to evaluate.
    ///
    /// See [state](https://docs.typesafe.ai/concepts/state) for details.
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
    ///
    /// Pass `None` to fall back to the client's timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: impl Into<Option<Duration>>) -> Self {
        self.spec.options.set_timeout(timeout);
        self
    }

    /// Sets the retry policy for this call only, overriding the client value.
    ///
    /// Pass `None` to fall back to the client's policy.
    #[must_use]
    pub fn retry(mut self, retry: impl Into<Option<RetryPolicy>>) -> Self {
        self.spec.options.set_retry(retry);
        self
    }

    /// Adds a request header for this call only.
    ///
    /// Authentication, SDK identification, and `Accept` remain protected.
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

    /// Adds top-level request-body fields, shallow-merged over the body after `state`, `model`, and
    /// `questions` are set.
    ///
    /// Merging is last-write-wins: a key that collides with `state`, `model`, or `questions` overrides
    /// it, and object values are replaced rather than deep-merged. The value must be a JSON object.
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

    /// Sends the request and returns the answers.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidInput`] when `state` is missing, no questions were added, a
    /// question is malformed, or `extra_body` is not a JSON object; [`crate::Error::Api`] and its
    /// variants when the server returns an unsuccessful response after any retries; and
    /// [`crate::Error::Connection`] or [`crate::Error::Timeout`] when the request cannot reach the
    /// server.
    pub async fn send(self) -> Result<SystemOneResponse> {
        let name = self.client.default_model().to_owned();
        call(
            &self.client,
            &self.spec.options,
            Method::POST,
            SYSTEM_ONE_PATH,
            Some(self.spec.body(&name)?),
        )
        .await
    }
}

impl SystemOneSpec {
    /// Builds the request body, applying question validation and the extra-field merge.
    pub(crate) fn body(&self, client_model: &str) -> Result<Value, InvalidInputError> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| InvalidInputError::new("state is required"))?;
        let questions = question::normalize(&self.questions)?;
        let mut body = Map::with_capacity(3 + self.extra_fields.len());
        body.insert("state".to_owned(), state.clone().to_value());
        body.insert(
            "model".to_owned(),
            Value::String(
                self.model
                    .clone()
                    .unwrap_or_else(|| client_model.to_owned()),
            ),
        );
        body.insert("questions".to_owned(), questions);
        if let Some(extra) = &self.extra_body {
            let extra = extra
                .as_object()
                .ok_or_else(|| InvalidInputError::new("extra_body must be a JSON object"))?;
            body.extend(extra.clone());
        }
        for (key, value) in &self.extra_fields {
            body.insert(key.clone(), value.clone());
        }
        Ok(Value::Object(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> SystemOneSpec {
        SystemOneSpec::default()
    }

    #[test]
    fn bodies_use_the_client_model_unless_overridden() {
        let mut spec = spec();
        spec.state = Some(JsonContent::text("hello"));
        spec.questions
            .push(("q".to_owned(), Question::from(crate::Noul::new())));
        assert_eq!(
            spec.body("client-model").unwrap(),
            json!({"state": "hello", "model": "client-model", "questions": {"q": {"type": "noul"}}})
        );
        spec.model = Some("call-model".to_owned());
        assert_eq!(
            spec.body("client-model").unwrap()["model"],
            json!("call-model")
        );
    }

    #[test]
    fn missing_state_is_reported() {
        let mut spec = spec();
        spec.questions
            .push(("q".to_owned(), Question::from(crate::Noul::new())));
        assert_eq!(
            spec.body("model").unwrap_err().message(),
            "state is required"
        );
    }

    #[test]
    fn extra_body_merges_last_write_wins() {
        let mut spec = spec();
        spec.state = Some(JsonContent::text("hello"));
        spec.questions
            .push(("q".to_owned(), Question::from(crate::Noul::new())));
        spec.extra_body = Some(json!({"state": {"replaced": true}, "temperature": 0}));
        spec.extra_fields.push(("temperature".to_owned(), json!(1)));
        spec.extra_fields.push(("top_p".to_owned(), json!(0.5)));
        let body = spec.body("model").unwrap();
        assert_eq!(body["state"], json!({"replaced": true}));
        assert_eq!(body["temperature"], json!(1));
        assert_eq!(body["top_p"], json!(0.5));
    }

    #[test]
    fn extra_body_must_be_an_object() {
        let mut spec = spec();
        spec.state = Some(JsonContent::text("hello"));
        spec.questions
            .push(("q".to_owned(), Question::from(crate::Noul::new())));
        spec.extra_body = Some(json!([1, 2]));
        assert_eq!(
            spec.body("model").unwrap_err().message(),
            "extra_body must be a JSON object"
        );
    }
}
