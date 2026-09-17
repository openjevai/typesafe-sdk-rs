//! Question and state content: free text or arbitrary JSON.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::InvalidInputError;

/// State or question text: either plain text or an arbitrary JSON document.
///
/// Text is sent as a JSON string; [`JsonContent::json`] wraps an object, array, or other JSON value.
/// A JSON `null` is a value like any other; use `Option<JsonContent>` when a field should be omitted.
///
/// ```
/// use typesafe_sdk::JsonContent;
///
/// let text = JsonContent::text("I was charged twice.");
/// let structured = JsonContent::json(serde_json::json!({"document": "hello"}));
///
/// assert_eq!(text.as_str(), Some("I was charged twice."));
/// assert!(structured.as_str().is_none());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonContent {
    /// Plain text, sent as a JSON string.
    Text(String),
    /// An arbitrary JSON value, such as an object or an array.
    Structured(Value),
}

impl JsonContent {
    /// Wraps plain text.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// Wraps an arbitrary JSON value, such as an object or an array.
    pub fn json(value: impl Into<Value>) -> Self {
        Self::Structured(value.into())
    }

    /// Serializes `value` into JSON content.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidInputError`] when `value` cannot be represented as JSON.
    pub fn from_serialize<T: Serialize>(value: T) -> Result<Self, InvalidInputError> {
        serde_json::to_value(value)
            .map(Self::Structured)
            .map_err(|error| {
                InvalidInputError::new(format!("the value could not be encoded as JSON: {error}"))
            })
    }

    /// Returns the text when this content is plain text.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Structured(_) => None,
        }
    }

    /// Returns the JSON value when this content is structured.
    pub fn as_structured(&self) -> Option<&Value> {
        match self {
            Self::Text(_) => None,
            Self::Structured(value) => Some(value),
        }
    }

    /// Converts this content into a JSON value.
    pub fn to_value(self) -> Value {
        match self {
            Self::Text(text) => Value::String(text),
            Self::Structured(value) => value,
        }
    }
}

impl From<&str> for JsonContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for JsonContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&String> for JsonContent {
    fn from(text: &String) -> Self {
        Self::Text(text.clone())
    }
}

impl From<Value> for JsonContent {
    fn from(value: Value) -> Self {
        Self::Structured(value)
    }
}

impl From<&Value> for JsonContent {
    fn from(value: &Value) -> Self {
        Self::Structured(value.clone())
    }
}

impl From<Map<String, Value>> for JsonContent {
    fn from(value: Map<String, Value>) -> Self {
        Self::Structured(Value::Object(value))
    }
}

impl From<Vec<Value>> for JsonContent {
    fn from(value: Vec<Value>) -> Self {
        Self::Structured(Value::Array(value))
    }
}

impl From<&[Value]> for JsonContent {
    fn from(value: &[Value]) -> Self {
        Self::Structured(Value::Array(value.to_vec()))
    }
}

/// Decodes a response body for response types and error reporting.
///
/// An empty body becomes JSON `null`; a body that is not valid JSON becomes the response text, with
/// invalid UTF-8 replaced, exactly as the Python SDK reports it.
pub(crate) fn decode_body(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct Document {
        title: String,
        size: u64,
    }

    #[test]
    fn content_round_trips_through_json() {
        let cases = [
            (
                JsonContent::text("hello"),
                Value::String("hello".to_owned()),
            ),
            (
                JsonContent::json(serde_json::json!({"a": [1, 2]})),
                serde_json::json!({"a": [1, 2]}),
            ),
            (
                JsonContent::json(serde_json::json!([1, 2])),
                serde_json::json!([1, 2]),
            ),
            (JsonContent::json(Value::Null), Value::Null),
        ];
        for (content, expected) in cases {
            let encoded = serde_json::to_value(&content).unwrap();
            assert_eq!(encoded, expected);
            let decoded: JsonContent = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, content);
        }
    }

    #[test]
    fn response_bodies_decode_leniently() {
        assert_eq!(decode_body(b""), Value::Null);
        assert_eq!(decode_body(b"{\"a\": 1}"), serde_json::json!({"a": 1}));
        assert_eq!(decode_body(br#""text""#), Value::String("text".to_owned()));
        assert_eq!(
            decode_body(b"not JSON: \xff"),
            Value::String("not JSON: \u{fffd}".to_owned())
        );
    }

    #[test]
    fn from_serialize_encodes_structs() {
        let content = JsonContent::from_serialize(Document {
            title: "t".to_owned(),
            size: 3,
        })
        .unwrap();
        assert_eq!(
            content.to_value(),
            serde_json::json!({"title": "t", "size": 3})
        );
    }

    #[test]
    fn from_serialize_encodes_abstract_containers() {
        // Python accepts any Mapping or Sequence; Rust callers serialize such values instead.
        let content =
            JsonContent::from_serialize(("read", serde_json::json!({"ctx": Value::Null}))).unwrap();
        assert_eq!(
            content.to_value(),
            serde_json::json!(["read", {"ctx": null}])
        );
        let content = JsonContent::from_serialize(vec!["low", "high"]).unwrap();
        assert_eq!(content.to_value(), serde_json::json!(["low", "high"]));
    }

    #[test]
    fn from_serialize_reports_encoding_failures() {
        struct Unencodable;

        impl Serialize for Unencodable {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("unencodable"))
            }
        }

        let error = JsonContent::from_serialize(Unencodable).unwrap_err();
        assert!(
            error.message().contains("could not be encoded as JSON"),
            "{}",
            error.message()
        );
    }
}
