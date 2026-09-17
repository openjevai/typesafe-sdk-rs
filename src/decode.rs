//! JSON to typed decoding, including the dotted field paths reported by validation errors.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Map, Value};

use crate::constants::LOG_TARGET;
use crate::json::JsonContent;
use crate::response::{
    Answer, ChoiceAnswer, ModelMetadata, NoulAnswer, ScoreAnswer, UnknownAnswer, Usage,
};

/// One step of a dotted field path.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    /// An object member, rendered as `.name`.
    Key(String),
    /// An array element, rendered as `[index]`.
    Index(usize),
    /// An object entry, rendered as `[.]` the way the Python SDK's decoder names it.
    Placeholder,
}

/// Dotted path to a field inside a response body, such as `answers.tone.confidence`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Path(Vec<Segment>);

impl Path {
    /// Creates the path of a response body's root.
    pub(crate) fn root() -> Self {
        Self(Vec::new())
    }

    /// Extends the path with an object member.
    pub(crate) fn key(&self, key: impl Into<String>) -> Self {
        let mut segments = self.0.clone();
        segments.push(Segment::Key(key.into()));
        Self(segments)
    }

    /// Extends the path with an array element.
    pub(crate) fn index(&self, index: usize) -> Self {
        let mut segments = self.0.clone();
        segments.push(Segment::Index(index));
        Self(segments)
    }

    /// Extends the path with an `[.]` placeholder, naming the entries of an object.
    fn placeholder(&self) -> Self {
        let mut segments = self.0.clone();
        segments.push(Segment::Placeholder);
        Self(segments)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, segment) in self.0.iter().enumerate() {
            match segment {
                Segment::Key(key) => {
                    if position > 0 {
                        formatter.write_str(".")?;
                    }
                    formatter.write_str(key)?;
                }
                Segment::Index(index) => write!(formatter, "[{index}]")?,
                Segment::Placeholder => formatter.write_str("[.]")?,
            }
        }
        Ok(())
    }
}

/// The first missing or structurally invalid field of a response body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldError {
    /// Dotted path to the offending field, empty for the body as a whole.
    pub(crate) path: String,
    /// Why the field was rejected.
    pub(crate) reason: String,
}

impl FieldError {
    /// Describes why the field at `path` was rejected.
    fn new(path: &Path, reason: impl Into<String>) -> Self {
        Self {
            path: path.to_string(),
            reason: reason.into(),
        }
    }
}

/// Decodes a System One response body into its model, usage, and answers.
pub(crate) fn system_one(
    value: &Value,
) -> Result<(String, Usage, BTreeMap<String, Answer>), FieldError> {
    let root = Path::root();
    let body = object(value, &root)?;
    let model = required_string(body, "model", &root)?;
    let usage = usage(field(body, "usage", &root)?.0, &root.key("usage"))?;
    let answers = object(field(body, "answers", &root)?.0, &root.key("answers"))?;
    let answers_path = root.key("answers");
    let mut decoded = BTreeMap::new();
    for (name, answer) in answers {
        decoded.insert(
            name.clone(),
            answer_value(answer, &answers_path.key(name), name)?,
        );
    }
    Ok((model, usage, decoded))
}

/// Decodes the models listing response body.
pub(crate) fn models(value: &Value) -> Result<Vec<ModelMetadata>, FieldError> {
    let root = Path::root();
    let body = object(value, &root)?;
    let (models, path) = field(body, "models", &root)?;
    let items = array(models, &path)?;
    let mut decoded = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        decoded.push(model_metadata(item, &path.index(index))?);
    }
    Ok(decoded)
}

/// Decodes one model metadata object.
pub(crate) fn model_metadata(value: &Value, path: &Path) -> Result<ModelMetadata, FieldError> {
    let object = object(value, path)?;
    Ok(ModelMetadata {
        name: required_string(object, "name", path)?,
        description: required_string(object, "description", path)?,
        release_date: required_string(object, "release_date", path)?,
    })
}

/// Decodes a usage object, treating an absent or `null` token count as unreported.
pub(crate) fn usage(value: &Value, path: &Path) -> Result<Usage, FieldError> {
    let object = object(value, path)?;
    Ok(Usage {
        input_tokens: optional_count(object, "input_tokens", path)?,
        output_tokens: optional_count(object, "output_tokens", path)?,
    })
}

/// Decodes one answer, dispatching on its `type` discriminator.
///
/// Answer kinds this SDK version does not model are kept as [`Answer::Unknown`] so a newer server
/// never breaks an older client.
pub(crate) fn answer_value(value: &Value, path: &Path, name: &str) -> Result<Answer, FieldError> {
    let type_path = path.key("type");
    let object = value
        .as_object()
        .ok_or_else(|| FieldError::new(&type_path, "expected an object with a string `type`"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| FieldError::new(&type_path, "expected a string `type`"))?;
    match kind {
        "noul" => Ok(Answer::Noul(NoulAnswer {
            noul: required_number(object, "noul", path)?,
        })),
        "choice" => Ok(Answer::Choice(ChoiceAnswer {
            choice: required_string(object, "choice", path)?,
            confidence: required_number(object, "confidence", path)?,
            probabilities: string_number_map(object, "probabilities", path)?,
        })),
        "score" => Ok(Answer::Score(ScoreAnswer {
            score: required_number(object, "score", path)?,
            confidence: required_number(object, "confidence", path)?,
            legend: score_map(object, "legend", path)?,
            probabilities: scored_number_map(object, "probabilities", path)?,
        })),
        other => {
            log::warn!(target: LOG_TARGET, "unrecognized answer type {other:?} for answer {name:?}; kept unparsed");
            Ok(Answer::Unknown(UnknownAnswer {
                kind: other.to_owned(),
                value: value.clone(),
            }))
        }
    }
}

/// Requires a JSON object.
fn object<'a>(value: &'a Value, path: &Path) -> Result<&'a Map<String, Value>, FieldError> {
    value
        .as_object()
        .ok_or_else(|| FieldError::new(path, "expected a JSON object"))
}

/// Requires a JSON array.
fn array<'a>(value: &'a Value, path: &Path) -> Result<&'a Vec<Value>, FieldError> {
    value
        .as_array()
        .ok_or_else(|| FieldError::new(path, "expected a JSON array"))
}

/// Requires a member of `base`, returning it with its own field path.
fn field<'a>(
    base: &'a Map<String, Value>,
    name: &str,
    path: &Path,
) -> Result<(&'a Value, Path), FieldError> {
    let field_path = path.key(name);
    let value = base
        .get(name)
        .ok_or_else(|| FieldError::new(&field_path, format!("missing required field `{name}`")))?;
    Ok((value, field_path))
}

/// Requires a member of `base` to be a string.
fn required_string(
    base: &Map<String, Value>,
    name: &str,
    path: &Path,
) -> Result<String, FieldError> {
    let (value, field_path) = field(base, name, path)?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| FieldError::new(&field_path, format!("field `{name}` must be a string")))
}

/// Requires a member of `base` to be a number.
fn required_number(base: &Map<String, Value>, name: &str, path: &Path) -> Result<f64, FieldError> {
    let (value, field_path) = field(base, name, path)?;
    value
        .as_f64()
        .ok_or_else(|| FieldError::new(&field_path, format!("field `{name}` must be a number")))
}

/// Reads an optional token count, which must be a non-negative integer when present.
fn optional_count(
    base: &Map<String, Value>,
    name: &str,
    path: &Path,
) -> Result<Option<u64>, FieldError> {
    match base.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            FieldError::new(
                &path.key(name),
                format!("field `{name}` must be a non-negative integer"),
            )
        }),
    }
}

/// Requires a member of `base` to be an object of label to number.
fn string_number_map(
    base: &Map<String, Value>,
    name: &str,
    path: &Path,
) -> Result<BTreeMap<String, f64>, FieldError> {
    let (value, field_path) = field(base, name, path)?;
    let entries = object(value, &field_path)?;
    let mut decoded = BTreeMap::new();
    for (label, probability) in entries {
        let probability = probability.as_f64().ok_or_else(|| {
            FieldError::new(
                &field_path.placeholder(),
                format!("`{name}` values must be numbers"),
            )
        })?;
        decoded.insert(label.clone(), probability);
    }
    Ok(decoded)
}

/// Requires a member of `base` to be an object keyed by integer score, valued by content.
fn score_map(
    base: &Map<String, Value>,
    name: &str,
    path: &Path,
) -> Result<BTreeMap<i64, JsonContent>, FieldError> {
    let (value, field_path) = field(base, name, path)?;
    let entries = object(value, &field_path)?;
    let mut decoded = BTreeMap::new();
    for (score, content) in entries {
        decoded.insert(
            integer_key(score, name, &field_path)?,
            content_from(content),
        );
    }
    Ok(decoded)
}

/// Requires a member of `base` to be an object keyed by integer score, valued by number.
fn scored_number_map(
    base: &Map<String, Value>,
    name: &str,
    path: &Path,
) -> Result<BTreeMap<i64, f64>, FieldError> {
    let (value, field_path) = field(base, name, path)?;
    let entries = object(value, &field_path)?;
    let mut decoded = BTreeMap::new();
    for (score, probability) in entries {
        let score = integer_key(score, name, &field_path)?;
        let probability = probability.as_f64().ok_or_else(|| {
            FieldError::new(
                &field_path.placeholder(),
                format!("`{name}` values must be numbers"),
            )
        })?;
        decoded.insert(score, probability);
    }
    Ok(decoded)
}

/// Parses an object key that must name an integer score, in canonical form like the Python decoder.
///
/// `-3` and `-0` are accepted, while leading zeros, a leading `+`, and surrounding space are not, so
/// the keys this SDK accepts match the ones the Python SDK accepts.
fn integer_key(key: &str, name: &str, path: &Path) -> Result<i64, FieldError> {
    let invalid = || FieldError::new(path, format!("`{name}` keys must be integer scores"));
    let digits = key.strip_prefix('-').unwrap_or(key);
    let canonical = !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits.len() == 1 || !digits.starts_with('0'));
    if !canonical {
        return Err(invalid());
    }
    key.parse::<i64>().map_err(|_| invalid())
}

/// Reads JSON content, mapping JSON strings onto [`JsonContent::Text`].
fn content_from(value: &Value) -> JsonContent {
    match value {
        Value::String(text) => JsonContent::text(text.clone()),
        other => JsonContent::Structured(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error_path(body: &Value) -> String {
        system_one(body).unwrap_err().path
    }

    fn with_answers(answers: Value) -> Value {
        json!({"model": "test", "usage": {"input_tokens": 1, "output_tokens": 1}, "answers": answers})
    }

    #[test]
    fn paths_render_object_members_and_array_elements() {
        let path = Path::root().key("answers").key("tone").key("confidence");
        assert_eq!(path.to_string(), "answers.tone.confidence");
        let path = Path::root().key("models").index(1).key("description");
        assert_eq!(path.to_string(), "models[1].description");
        assert_eq!(Path::root().to_string(), "");
    }

    #[test]
    fn malformed_bodies_report_the_offending_field() {
        assert_eq!(error_path(&json!({})), "model");
        assert_eq!(error_path(&json!([])), "");
        assert_eq!(error_path(&json!("not-an-object")), "");
        assert_eq!(error_path(&json!(null)), "");
        assert_eq!(error_path(&json!({"model": 1})), "model");
        assert_eq!(error_path(&json!({"model": "test"})), "usage");
        assert_eq!(error_path(&json!({"model": "test", "usage": []})), "usage");
        assert_eq!(
            error_path(&json!({"model": "test", "usage": {"input_tokens": -1}})),
            "usage.input_tokens"
        );
        assert_eq!(
            error_path(&json!({"model": "test", "usage": {"output_tokens": 1.5}})),
            "usage.output_tokens"
        );
        assert_eq!(
            error_path(&json!({"model": "test", "usage": {}, "answers": []})),
            "answers"
        );
        assert_eq!(
            error_path(&json!({"usage": {"input_tokens": 1}, "answers": {}})),
            "model"
        );
        assert_eq!(
            error_path(&json!({"model": "test", "usage": {}, "answers": {"n": 4}})),
            "answers.n.type"
        );
    }

    #[test]
    fn answer_paths_match_the_reference_implementation() {
        let cases = [
            (json!({"n": {"type": "noul"}}), "answers.n.noul"),
            (
                json!({"c": {"type": "choice", "choice": "a", "probabilities": {}}}),
                "answers.c.confidence",
            ),
            (
                json!({"c": {"type": "choice", "confidence": 0.5, "probabilities": {}}}),
                "answers.c.choice",
            ),
            (
                json!({"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": [], "probabilities": {}}}),
                "answers.s.legend",
            ),
            (
                json!({"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"x": "bad"}, "probabilities": {}}}),
                "answers.s.legend",
            ),
            (json!({"c": "not-a-mapping"}), "answers.c.type"),
            (json!({"c": {"type": 4}}), "answers.c.type"),
            (json!({"c": {}}), "answers.c.type"),
            (
                json!({"c": {"type": "choice", "choice": "a", "confidence": 1.0, "probabilities": {"a": "x"}}}),
                "answers.c.probabilities[.]",
            ),
            (
                json!({"c": {"type": "choice", "choice": "a", "confidence": "x", "probabilities": {}}}),
                "answers.c.confidence",
            ),
            (
                json!({"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"0": "bad"}, "probabilities": {"x": 1.0}}}),
                "answers.s.probabilities",
            ),
            (
                json!({"s": {"type": "score", "score": 1.0, "confidence": 1.0, "legend": {"0": "bad"}, "probabilities": {"0": "x"}}}),
                "answers.s.probabilities[.]",
            ),
            (
                json!({"s": {"type": "score", "score": 1.0, "confidence": 1.0, "probabilities": {}}}),
                "answers.s.legend",
            ),
        ];
        for (answers, expected) in cases {
            assert_eq!(
                error_path(&with_answers(answers.clone())),
                expected,
                "{answers}"
            );
        }
    }

    #[test]
    fn score_keys_must_be_canonical_integers() {
        let body = |key: &str| {
            json!({
                "model": "test",
                "usage": {},
                "answers": {"s": {"type": "score", "score": 1.0, "confidence": 1.0,
                    "legend": {key: "level"}, "probabilities": {"0": 1.0}}},
            })
        };
        // The Python decoder accepts these keys.
        for key in ["0", "-0", "-3", "42"] {
            assert!(system_one(&body(key)).is_ok(), "{key}");
        }
        // A leading zero, a leading plus, or surrounding space is rejected at the map's own path.
        for key in ["007", "01", "+1", " 1", "1.0", "1e2", ""] {
            assert_eq!(error_path(&body(key)), "answers.s.legend", "{key}");
        }
    }

    #[test]
    fn usage_treats_null_as_unreported() {
        let body = json!({"model": "test", "usage": {"input_tokens": null, "output_tokens": 3}, "answers": {}});
        let (_, usage, _) = system_one(&body).unwrap();
        assert_eq!(
            usage,
            Usage {
                input_tokens: None,
                output_tokens: Some(3)
            }
        );
        let body = json!({"model": "test", "usage": {}, "answers": {}});
        let (_, usage, _) = system_one(&body).unwrap();
        assert_eq!(usage, Usage::default());
    }

    #[test]
    fn answers_decode_into_typed_values() {
        let body = with_answers(json!({
            "spam": {"type": "noul", "noul": 0.98},
            "tone": {"type": "choice", "choice": "friendly", "confidence": 0.9, "probabilities": {"friendly": 0.9, "hostile": 0.1}},
            "quality": {
                "type": "score",
                "score": 1.7,
                "confidence": 0.8,
                "legend": {"0": {"examples": ["a"]}, "2": "great"},
                "probabilities": {"0": 0.1, "2": 0.8},
            },
        }));
        let (model, _, answers) = system_one(&body).unwrap();
        assert_eq!(model, "test");
        let Answer::Noul(noul) = &answers["spam"] else {
            panic!("expected a noul answer")
        };
        assert_eq!(noul.noul, 0.98);
        let Answer::Choice(choice) = &answers["tone"] else {
            panic!("expected a choice answer")
        };
        assert_eq!(choice.choice, "friendly");
        assert_eq!(choice.probability("hostile"), Some(0.1));
        assert_eq!(choice.probability("missing"), None);
        let Answer::Score(score) = &answers["quality"] else {
            panic!("expected a score answer")
        };
        assert_eq!(score.legend.keys().copied().collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(
            score.legend[&0],
            JsonContent::json(json!({"examples": ["a"]}))
        );
        assert_eq!(score.legend[&2], JsonContent::text("great"));
        assert_eq!(score.probabilities[&2], 0.8);
    }

    #[test]
    fn unknown_answer_types_are_kept_unparsed() {
        let body = with_answers(json!({
            "spam": {"type": "noul", "noul": 0.9},
            "mystery": {"type": "aurora", "value": 3},
        }));
        let (_, _, answers) = system_one(&body).unwrap();
        assert_eq!(answers.len(), 2);
        let Answer::Unknown(unknown) = &answers["mystery"] else {
            panic!("expected an unknown answer")
        };
        assert_eq!(unknown.kind, "aurora");
        assert_eq!(unknown.value, json!({"type": "aurora", "value": 3}));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let body = json!({
            "model": "test",
            "usage": {"input_tokens": 1, "output_tokens": 1, "reasoning_tokens": 9, "billing_units": 1},
            "answers": {"spam": {"type": "noul", "noul": 0.9, "explanation": "spammy"}},
            "extra": true,
        });
        let (model, usage, answers) = system_one(&body).unwrap();
        assert_eq!(model, "test");
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(1),
                output_tokens: Some(1)
            }
        );
        assert_eq!(answers.len(), 1);
    }

    #[test]
    fn model_metadata_paths_include_the_index() {
        let model =
            json!({"name": "test", "description": "Test model", "release_date": "2026-09-14"});
        for missing in ["name", "description", "release_date"] {
            let mut incomplete = model.clone();
            incomplete.as_object_mut().unwrap().remove(missing);
            let body = json!({"models": [model.clone(), incomplete]});
            assert_eq!(
                models(&body).unwrap_err().path,
                format!("models[1].{missing}")
            );
        }
        assert_eq!(models(&json!({})).unwrap_err().path, "models");
        assert_eq!(models(&json!({"models": {}})).unwrap_err().path, "models");
        assert_eq!(
            models(&json!({"models": [1]})).unwrap_err().path,
            "models[0]"
        );
        assert_eq!(
            models(&json!({"models": [{"name": "a", "description": "b", "release_date": "c"}]}))
                .unwrap()
                .len(),
            1
        );
    }
}
