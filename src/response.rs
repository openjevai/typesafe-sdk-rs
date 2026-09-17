//! Responses returned by the API, with their raw HTTP metadata.

use std::collections::BTreeMap;
use std::fmt;

use http::{HeaderMap, StatusCode};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::constants::REQUEST_ID_HEADER;
use crate::decode::{self, FieldError};
use crate::error::{ApiError, Error, Result, ValidationError, classify};
use crate::json::JsonContent;

/// Token counts for a request, when reported by the API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input tokens used, or `None` when the API did not report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Number of output tokens used, or `None` when the API did not report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

/// The HTTP response a decoded response came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawResponse {
    /// HTTP status code.
    status: StatusCode,
    /// HTTP response headers.
    headers: HeaderMap,
    /// Response body: the parsed JSON document, or the response text when it was not JSON.
    body: Value,
}

impl RawResponse {
    /// Creates a raw response from its HTTP parts.
    pub(crate) fn new(status: StatusCode, headers: HeaderMap, body: Value) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Returns the response body, parsed as JSON when possible.
    pub fn body(&self) -> &Value {
        &self.body
    }

    /// Returns the `x-typesafe-request-id` response header, when present.
    pub fn request_id(&self) -> Option<&str> {
        self.headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
    }

    /// Returns the response body as an error body, where a JSON `null` counts as no body.
    fn body_option(&self) -> Option<Value> {
        match &self.body {
            Value::Null => None,
            body => Some(body.clone()),
        }
    }
}

/// A yes/no answer.
///
/// See the [noul primitive](https://docs.typesafe.ai/primitives/noul) for details.
#[derive(Clone, Debug, PartialEq)]
pub struct NoulAnswer {
    /// Probability of a yes answer, from zero to one.
    pub noul: f64,
}

/// A selected label and its probabilities.
#[derive(Clone, Debug, PartialEq)]
pub struct ChoiceAnswer {
    /// The selected label.
    pub choice: String,
    /// Reported confidence in the selected label.
    pub confidence: f64,
    /// Probabilities keyed by label.
    pub probabilities: BTreeMap<String, f64>,
}

impl ChoiceAnswer {
    /// Returns the probability of `label`, when the API reported one.
    pub fn probability(&self, label: &str) -> Option<f64> {
        self.probabilities.get(label).copied()
    }
}

/// An expected score with its rubric and probabilities.
#[derive(Clone, Debug, PartialEq)]
pub struct ScoreAnswer {
    /// Expected score, which may fall between the integer rubric levels.
    pub score: f64,
    /// Reported confidence in the score.
    pub confidence: f64,
    /// Rubric descriptions keyed by integer score.
    pub legend: BTreeMap<i64, JsonContent>,
    /// Probabilities keyed by integer score.
    pub probabilities: BTreeMap<i64, f64>,
}

/// An answer whose `type` this SDK version does not model.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownAnswer {
    /// The unrecognized `type` discriminator.
    pub kind: String,
    /// The answer document, exactly as the server sent it.
    pub value: Value,
}

/// An answer to a single question, identified by its `type`.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// A yes/no answer.
    Noul(NoulAnswer),
    /// A selected label with its probabilities.
    Choice(ChoiceAnswer),
    /// An expected score with its rubric.
    Score(ScoreAnswer),
    /// An answer whose `type` this SDK version does not model.
    Unknown(UnknownAnswer),
}

impl Answer {
    /// Returns the wire `type` of this answer.
    pub fn kind(&self) -> &str {
        match self {
            Self::Noul(_) => "noul",
            Self::Choice(_) => "choice",
            Self::Score(_) => "score",
            Self::Unknown(answer) => &answer.kind,
        }
    }
}

/// Answers grouped by question name, with model and usage metadata.
#[derive(Clone)]
pub struct SystemOneResponse {
    /// The model used to answer the request.
    pub model: String,
    /// Token usage for the request.
    pub usage: Usage,
    /// All answer objects keyed by question name.
    pub answers: BTreeMap<String, Answer>,
    /// The underlying HTTP response, exposing status, headers, and body.
    raw: RawResponse,
}

impl SystemOneResponse {
    /// Creates a response from its decoded fields and raw HTTP response.
    pub(crate) fn new(
        model: String,
        usage: Usage,
        answers: BTreeMap<String, Answer>,
        raw: RawResponse,
    ) -> Self {
        Self {
            model,
            usage,
            answers,
            raw,
        }
    }

    /// Returns the answer to the question named `name`, whatever its type.
    pub fn answer(&self, name: &str) -> Option<&Answer> {
        self.answers.get(name)
    }

    /// Returns the yes/no answers keyed by question name.
    pub fn nouls(&self) -> impl Iterator<Item = (&str, &NoulAnswer)> {
        self.answers
            .iter()
            .filter_map(|(name, answer)| match answer {
                Answer::Noul(answer) => Some((name.as_str(), answer)),
                _ => None,
            })
    }

    /// Returns the choice answers keyed by question name.
    pub fn choices(&self) -> impl Iterator<Item = (&str, &ChoiceAnswer)> {
        self.answers
            .iter()
            .filter_map(|(name, answer)| match answer {
                Answer::Choice(answer) => Some((name.as_str(), answer)),
                _ => None,
            })
    }

    /// Returns the score answers keyed by question name.
    pub fn scores(&self) -> impl Iterator<Item = (&str, &ScoreAnswer)> {
        self.answers
            .iter()
            .filter_map(|(name, answer)| match answer {
                Answer::Score(answer) => Some((name.as_str(), answer)),
                _ => None,
            })
    }

    /// Returns the yes/no answer to the question named `name`, when it is one.
    pub fn noul(&self, name: &str) -> Option<&NoulAnswer> {
        match self.answers.get(name) {
            Some(Answer::Noul(answer)) => Some(answer),
            _ => None,
        }
    }

    /// Returns the choice answer to the question named `name`, when it is one.
    pub fn choice(&self, name: &str) -> Option<&ChoiceAnswer> {
        match self.answers.get(name) {
            Some(Answer::Choice(answer)) => Some(answer),
            _ => None,
        }
    }

    /// Returns the score answer to the question named `name`, when it is one.
    pub fn score(&self, name: &str) -> Option<&ScoreAnswer> {
        match self.answers.get(name) {
            Some(Answer::Score(answer)) => Some(answer),
            _ => None,
        }
    }

    /// Returns the `x-typesafe-request-id` response header, when present.
    pub fn request_id(&self) -> Option<&str> {
        self.raw.request_id()
    }

    /// Returns the underlying HTTP response.
    pub fn raw(&self) -> &RawResponse {
        &self.raw
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.raw.status
    }

    /// Returns the HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.raw.headers()
    }
}

impl fmt::Debug for SystemOneResponse {
    /// Renders the decoded fields; the raw body is available through [`Self::raw`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SystemOneResponse")
            .field("model", &self.model)
            .field("usage", &self.usage)
            .field("answers", &self.answers)
            .field("status", &self.raw.status)
            .field("request_id", &self.raw.request_id())
            .finish_non_exhaustive()
    }
}

impl PartialEq for SystemOneResponse {
    /// Compares the decoded fields; HTTP metadata is not part of a response's value.
    fn eq(&self, other: &Self) -> bool {
        self.model == other.model && self.usage == other.usage && self.answers == other.answers
    }
}

/// Metadata for one model available to the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// Model name, as passed to `model`.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Release date reported by the API.
    pub release_date: String,
}

/// The models available to the account.
#[derive(Clone)]
pub struct ListModelsResponse {
    /// The available models.
    pub models: Vec<ModelMetadata>,
    /// The underlying HTTP response, exposing status, headers, and body.
    raw: RawResponse,
}

impl ListModelsResponse {
    /// Creates a response from its decoded fields and raw HTTP response.
    pub(crate) fn new(models: Vec<ModelMetadata>, raw: RawResponse) -> Self {
        Self { models, raw }
    }

    /// Returns the `x-typesafe-request-id` response header, when present.
    pub fn request_id(&self) -> Option<&str> {
        self.raw.request_id()
    }

    /// Returns the underlying HTTP response.
    pub fn raw(&self) -> &RawResponse {
        &self.raw
    }

    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.raw.status
    }

    /// Returns the HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        self.raw.headers()
    }
}

impl fmt::Debug for ListModelsResponse {
    /// Renders the decoded fields; the raw body is available through [`Self::raw`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ListModelsResponse")
            .field("models", &self.models)
            .field("status", &self.raw.status)
            .field("request_id", &self.raw.request_id())
            .finish_non_exhaustive()
    }
}

impl PartialEq for ListModelsResponse {
    /// Compares the decoded fields; HTTP metadata is not part of a response's value.
    fn eq(&self, other: &Self) -> bool {
        self.models == other.models
    }
}

impl Serialize for NoulAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({"type": "noul", "noul": self.noul}).serialize(serializer)
    }
}

impl Serialize for ChoiceAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({
            "type": "choice",
            "choice": self.choice,
            "confidence": self.confidence,
            "probabilities": self.probabilities,
        })
        .serialize(serializer)
    }
}

impl Serialize for ScoreAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({
            "type": "score",
            "score": self.score,
            "confidence": self.confidence,
            "legend": self.legend,
            "probabilities": self.probabilities,
        })
        .serialize(serializer)
    }
}

impl Serialize for UnknownAnswer {
    /// Emits the answer document exactly as the server sent it.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

impl Serialize for Answer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Noul(answer) => answer.serialize(serializer),
            Self::Choice(answer) => answer.serialize(serializer),
            Self::Score(answer) => answer.serialize(serializer),
            Self::Unknown(answer) => answer.serialize(serializer),
        }
    }
}

impl Serialize for SystemOneResponse {
    /// Emits the wire payload without HTTP metadata.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut body = Map::with_capacity(3);
        body.insert("model".to_owned(), Value::String(self.model.clone()));
        body.insert(
            "usage".to_owned(),
            serde_json::to_value(&self.usage).map_err(serde::ser::Error::custom)?,
        );
        body.insert(
            "answers".to_owned(),
            serde_json::to_value(&self.answers).map_err(serde::ser::Error::custom)?,
        );
        Value::Object(body).serialize(serializer)
    }
}

impl Serialize for ListModelsResponse {
    /// Emits the wire payload without HTTP metadata.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({"models": self.models}).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SystemOneResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let body = Value::deserialize(deserializer)?;
        let (model, usage, answers) = decode::system_one(&body).map_err(invalid_response)?;
        Ok(Self {
            model,
            usage,
            answers,
            raw: RawResponse::new(StatusCode::OK, HeaderMap::new(), body),
        })
    }
}

impl<'de> Deserialize<'de> for ListModelsResponse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let body = Value::deserialize(deserializer)?;
        let models = decode::models(&body).map_err(invalid_response)?;
        Ok(Self {
            models,
            raw: RawResponse::new(StatusCode::OK, HeaderMap::new(), body),
        })
    }
}

/// Renders a field error the way the public `Deserialize` impls report it.
fn invalid_response<E: serde::de::Error>(error: FieldError) -> E {
    E::custom(format!(
        "invalid response data at '{}': {}",
        error.path, error.reason
    ))
}

/// Builds the error for a response body that could not be decoded.
fn validation_error(raw: &RawResponse, endpoint: Option<String>, error: FieldError) -> Error {
    let status = raw.status;
    let body = raw.body_option();
    let message = format!(
        "Invalid response data at {}.",
        crate::error::python_repr(&error.path)
    );
    let api = ApiError::new(status, body, raw.headers.clone(), Some(message), endpoint);
    Error::ResponseValidation(ValidationError::new(api, error.path, error.reason))
}

/// Decodes a raw HTTP response into an SDK response type.
pub(crate) trait DecodeResponse: Sized {
    /// Classifies an unsuccessful response, or decodes a successful one.
    fn decode_response(raw: RawResponse, endpoint: &str) -> Result<Self>;
}

impl DecodeResponse for SystemOneResponse {
    fn decode_response(raw: RawResponse, endpoint: &str) -> Result<Self> {
        if !raw.status.is_success() {
            return Err(classify(
                raw.status,
                &raw.headers,
                raw.body_option(),
                Some(endpoint.to_owned()),
            ));
        }
        match decode::system_one(&raw.body) {
            Ok((model, usage, answers)) => Ok(Self::new(model, usage, answers, raw)),
            Err(error) => Err(validation_error(&raw, Some(endpoint.to_owned()), error)),
        }
    }
}

impl DecodeResponse for ListModelsResponse {
    fn decode_response(raw: RawResponse, endpoint: &str) -> Result<Self> {
        if !raw.status.is_success() {
            return Err(classify(
                raw.status,
                &raw.headers,
                raw.body_option(),
                Some(endpoint.to_owned()),
            ));
        }
        match decode::models(&raw.body) {
            Ok(models) => Ok(Self::new(models, raw)),
            Err(error) => Err(validation_error(&raw, Some(endpoint.to_owned()), error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The reference response body, mirroring the Python SDK's test fixture.
    fn result() -> Value {
        json!({
            "model": "jev-latest",
            "usage": {"input_tokens": 12, "output_tokens": 3},
            "answers": {
                "spam": {"type": "noul", "noul": 0.98},
                "tone": {"type": "choice", "choice": "friendly", "confidence": 0.9, "probabilities": {"friendly": 0.9, "hostile": 0.1}},
                "quality": {
                    "type": "score",
                    "score": 1.7,
                    "confidence": 0.8,
                    "legend": {"0": "bad", "1": "ok", "2": "great"},
                    "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8},
                },
            },
        })
    }

    #[test]
    fn responses_round_trip_through_serde() {
        let response: SystemOneResponse = serde_json::from_value(result()).unwrap();
        assert_eq!(response.model, "jev-latest");
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: Some(12),
                output_tokens: Some(3)
            }
        );
        assert_eq!(serde_json::to_value(&response).unwrap(), result());
        assert_eq!(
            serde_json::from_value::<SystemOneResponse>(serde_json::to_value(&response).unwrap())
                .unwrap(),
            response
        );

        let models = json!({"models": [{"name": "test", "description": "Test model", "release_date": "2026-09-14"}]});
        let response: ListModelsResponse = serde_json::from_value(models.clone()).unwrap();
        assert_eq!(response.models.len(), 1);
        assert_eq!(serde_json::to_value(&response).unwrap(), models);
    }

    #[test]
    fn answer_serialization_carries_its_type_tag() {
        let response: SystemOneResponse = serde_json::from_value(result()).unwrap();
        let answers = serde_json::to_value(&response.answers).unwrap();
        assert_eq!(answers, result()["answers"]);
        assert_eq!(response.answer("spam").unwrap().kind(), "noul");
        let noul = NoulAnswer { noul: 0.98 };
        assert_eq!(
            serde_json::to_value(&noul).unwrap(),
            json!({"type": "noul", "noul": 0.98})
        );
    }

    #[test]
    fn accessors_group_answers_by_type() {
        let response: SystemOneResponse = serde_json::from_value(result()).unwrap();
        assert_eq!(
            response.nouls().map(|(name, _)| name).collect::<Vec<_>>(),
            vec!["spam"]
        );
        assert_eq!(
            response.choices().map(|(name, _)| name).collect::<Vec<_>>(),
            vec!["tone"]
        );
        assert_eq!(
            response.scores().map(|(name, _)| name).collect::<Vec<_>>(),
            vec!["quality"]
        );
        assert_eq!(response.noul("spam").unwrap().noul, 0.98);
        assert_eq!(response.choice("tone").unwrap().choice, "friendly");
        assert_eq!(response.score("quality").unwrap().score, 1.7);
        assert!(response.noul("tone").is_none());
        assert!(response.choice("spam").is_none());
        assert!(response.score("missing").is_none());
    }

    #[test]
    fn unknown_answers_are_skipped_by_typed_accessors() {
        let body = json!({
            "model": "test",
            "usage": {},
            "answers": {"mystery": {"type": "aurora", "value": 3}},
        });
        let response: SystemOneResponse = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(response.nouls().count(), 0);
        assert_eq!(response.answer("mystery").unwrap().kind(), "aurora");
        assert_eq!(serde_json::to_value(&response).unwrap(), body);
    }

    #[test]
    fn decode_response_classifies_failures() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req-123".parse().unwrap());
        let raw = RawResponse::new(
            StatusCode::TOO_MANY_REQUESTS,
            headers.clone(),
            json!({"message": "slow down"}),
        );
        let error =
            SystemOneResponse::decode_response(raw, "POST https://api.typesafe.ai/v1/systemone")
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "POST https://api.typesafe.ai/v1/systemone: 429 slow down (request_id=req-123)"
        );
        assert_eq!(error.request_id(), Some("req-123"));

        let raw = RawResponse::new(StatusCode::OK, headers, json!({"usage": {}, "answers": {}}));
        let error =
            SystemOneResponse::decode_response(raw, "POST https://api.typesafe.ai/v1/systemone")
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "POST https://api.typesafe.ai/v1/systemone: 200 Invalid response data at 'model'. (request_id=req-123)"
        );
        let Error::ResponseValidation(validation) = &error else {
            panic!("expected a validation error")
        };
        assert_eq!(validation.field_path(), "model");
        assert!(!validation.reason().is_empty());
    }

    #[test]
    fn malformed_json_bodies_report_an_empty_path() {
        let raw = RawResponse::new(
            StatusCode::OK,
            HeaderMap::new(),
            Value::String("not JSON: \u{fffd}".to_owned()),
        );
        let error =
            SystemOneResponse::decode_response(raw, "GET https://api.typesafe.ai/v1/models")
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "GET https://api.typesafe.ai/v1/models: 200 Invalid response data at ''."
        );
    }

    #[test]
    fn debug_reports_fields_without_the_raw_body() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req-42".parse().unwrap());
        let raw = RawResponse::new(StatusCode::OK, headers, result());
        let response =
            SystemOneResponse::decode_response(raw, "POST https://api.typesafe.ai/v1/systemone")
                .unwrap();
        assert_eq!(response.request_id(), Some("req-42"));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.raw().body(), &result());
        let rendered = format!("{response:?}");
        assert!(rendered.contains("req-42"), "{rendered}");
        assert!(rendered.contains("answers"), "{rendered}");
        assert!(!rendered.contains(r#""body""#), "{rendered}");
    }
}
