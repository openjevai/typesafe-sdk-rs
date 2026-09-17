//! Question objects sent to System One.
//!
//! A question is either a typed builder ([`Noul`], [`Choice`], [`Score`]) or a raw JSON document
//! ([`Question::Raw`]), which is forwarded to the server unchanged.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::InvalidInputError;
use crate::json::JsonContent;

/// The `type` discriminator every question carries.
const TYPE_FIELD: &str = "type";

/// Optional descriptions of the yes and no outcomes of a [`Noul`] question.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoulCriteria {
    /// Description of the yes outcome, sent as `"true"`.
    pub yes: Option<JsonContent>,
    /// Description of the no outcome, sent as `"false"`.
    pub no: Option<JsonContent>,
}

impl NoulCriteria {
    /// Creates empty criteria; both outcomes are left undescribed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Describes the yes outcome.
    #[must_use]
    pub fn yes(mut self, description: impl Into<JsonContent>) -> Self {
        self.yes = Some(description.into());
        self
    }

    /// Describes the no outcome.
    #[must_use]
    pub fn no(mut self, description: impl Into<JsonContent>) -> Self {
        self.no = Some(description.into());
        self
    }

    /// Renders the criteria as a JSON object, omitting undescribed outcomes.
    fn to_wire(&self) -> Value {
        let mut object = Map::new();
        if let Some(yes) = &self.yes {
            object.insert("true".to_owned(), yes.clone().to_value());
        }
        if let Some(no) = &self.no {
            object.insert("false".to_owned(), no.clone().to_value());
        }
        Value::Object(object)
    }
}

/// A yes/no question with optional descriptions for either outcome.
///
/// ```
/// use typesafe_sdk::{Noul, NoulCriteria};
///
/// let question = Noul::new()
///     .instructions("Is this about billing?")
///     .criteria(NoulCriteria::new().yes("It mentions an invoice"));
///
/// assert_eq!(
///     serde_json::to_value(&question).unwrap(),
///     serde_json::json!({
///         "type": "noul",
///         "instructions": "Is this about billing?",
///         "criteria": {"true": "It mentions an invoice"},
///     }),
/// );
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Noul {
    /// The question to ask, expressed as text, a JSON object, or an array.
    pub instructions: Option<JsonContent>,
    /// Optional descriptions of the yes and no outcomes.
    pub criteria: Option<NoulCriteria>,
}

impl Noul {
    /// Creates a question with no instructions or criteria.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the question to ask.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<JsonContent>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Sets the descriptions of the yes and no outcomes.
    #[must_use]
    pub fn criteria(mut self, criteria: NoulCriteria) -> Self {
        self.criteria = Some(criteria);
        self
    }

    /// Renders the question as its wire object.
    fn to_wire(&self) -> Value {
        let mut fields = Map::new();
        if let Some(instructions) = &self.instructions {
            fields.insert("instructions".to_owned(), instructions.clone().to_value());
        }
        if let Some(criteria) = &self.criteria {
            fields.insert("criteria".to_owned(), criteria.to_wire());
        }
        with_tag("noul", fields)
    }

    /// Reads a question from its wire object.
    fn from_wire(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "a noul question must be a JSON object".to_owned())?;
        Ok(Self {
            instructions: optional_content(object, "instructions")?,
            criteria: match object.get("criteria") {
                None => None,
                Some(Value::Null) => None,
                Some(criteria) => {
                    let criteria = criteria
                        .as_object()
                        .ok_or_else(|| "noul criteria must be a JSON object".to_owned())?;
                    Some(NoulCriteria {
                        yes: optional_content(criteria, "true")?,
                        no: optional_content(criteria, "false")?,
                    })
                }
            },
        })
    }
}

/// A question that selects between named alternatives.
///
/// ```
/// use typesafe_sdk::Choice;
///
/// let question = Choice::new(["billing", "technical"])
///     .instructions("What is this ticket about?")
///     .describe("billing", "Invoices and payments");
///
/// assert_eq!(
///     serde_json::to_value(&question).unwrap(),
///     serde_json::json!({
///         "type": "choice",
///         "instructions": "What is this ticket about?",
///         "criteria": {"billing": "Invoices and payments", "technical": null},
///     }),
/// );
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Choice {
    /// The question to ask, expressed as text, a JSON object, or an array.
    pub instructions: Option<JsonContent>,
    /// Labels mapped to descriptions, or `None` for undescribed labels.
    pub criteria: BTreeMap<String, Option<JsonContent>>,
}

impl Choice {
    /// Creates a choice question from undescribed labels.
    pub fn new(labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            instructions: None,
            criteria: labels
                .into_iter()
                .map(|label| (label.into(), None))
                .collect(),
        }
    }

    /// Creates a choice question from fully specified criteria.
    pub fn from_criteria(criteria: BTreeMap<String, Option<JsonContent>>) -> Self {
        Self {
            instructions: None,
            criteria,
        }
    }

    /// Sets the question to ask.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<JsonContent>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Describes `label`, adding it when it is not already present.
    #[must_use]
    pub fn describe(
        mut self,
        label: impl Into<String>,
        description: impl Into<JsonContent>,
    ) -> Self {
        self.criteria.insert(label.into(), Some(description.into()));
        self
    }

    /// Adds `label` without a description.
    #[must_use]
    pub fn undescribed(mut self, label: impl Into<String>) -> Self {
        self.criteria.insert(label.into(), None);
        self
    }

    /// Renders the question as its wire object.
    fn to_wire(&self) -> Value {
        let mut fields = Map::new();
        if let Some(instructions) = &self.instructions {
            fields.insert("instructions".to_owned(), instructions.clone().to_value());
        }
        let criteria = self
            .criteria
            .iter()
            .map(|(label, description)| {
                (
                    label.clone(),
                    description
                        .clone()
                        .map_or(Value::Null, JsonContent::to_value),
                )
            })
            .collect::<Map<_, _>>();
        fields.insert("criteria".to_owned(), Value::Object(criteria));
        with_tag("choice", fields)
    }

    /// Reads a question from its wire object.
    fn from_wire(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "a choice question must be a JSON object".to_owned())?;
        let criteria = object
            .get("criteria")
            .and_then(Value::as_object)
            .ok_or_else(|| "missing field `criteria`".to_owned())?;
        Ok(Self {
            instructions: optional_content(object, "instructions")?,
            criteria: criteria
                .iter()
                .map(|(label, description)| {
                    let description = match description {
                        Value::Null => None,
                        other => Some(content_from(other)),
                    };
                    (label.clone(), description)
                })
                .collect(),
        })
    }
}

/// A question that assigns a score using an ordered rubric.
///
/// ```
/// use typesafe_sdk::Score;
///
/// let question = Score::new(["unusable", "usable", "excellent"]).instructions("How was it?");
///
/// assert_eq!(
///     serde_json::to_value(&question).unwrap(),
///     serde_json::json!({
///         "type": "score",
///         "instructions": "How was it?",
///         "criteria": ["unusable", "usable", "excellent"],
///     }),
/// );
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Score {
    /// The question to ask, expressed as text, a JSON object, or an array.
    pub instructions: Option<JsonContent>,
    /// Ordered descriptions, one per score from zero.
    pub criteria: Vec<JsonContent>,
}

impl Score {
    /// Creates a score question from ordered rubric descriptions.
    pub fn new(criteria: impl IntoIterator<Item = impl Into<JsonContent>>) -> Self {
        Self {
            instructions: None,
            criteria: criteria.into_iter().map(Into::into).collect(),
        }
    }

    /// Sets the question to ask.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<JsonContent>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Renders the question as its wire object.
    fn to_wire(&self) -> Value {
        let mut fields = Map::new();
        if let Some(instructions) = &self.instructions {
            fields.insert("instructions".to_owned(), instructions.clone().to_value());
        }
        fields.insert(
            "criteria".to_owned(),
            Value::Array(
                self.criteria
                    .iter()
                    .cloned()
                    .map(JsonContent::to_value)
                    .collect(),
            ),
        );
        with_tag("score", fields)
    }

    /// Reads a question from its wire object.
    fn from_wire(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "a score question must be a JSON object".to_owned())?;
        let criteria = object
            .get("criteria")
            .and_then(Value::as_array)
            .ok_or_else(|| "missing field `criteria`".to_owned())?;
        Ok(Self {
            instructions: optional_content(object, "instructions")?,
            criteria: criteria.iter().map(content_from).collect(),
        })
    }
}

/// A question to ask about the state, as a typed object or a raw JSON document.
///
/// Raw documents are forwarded verbatim, so any field the API adds later can be sent without an SDK
/// update.
///
/// ```
/// use typesafe_sdk::Question;
///
/// let question: Question = serde_json::json!({"type": "noul", "weight": 3}).into();
/// assert_eq!(serde_json::to_value(&question).unwrap(), serde_json::json!({"type": "noul", "weight": 3}));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum Question {
    /// A yes/no question.
    Noul(Noul),
    /// A question that selects between named alternatives.
    Choice(Choice),
    /// A question that assigns a score using an ordered rubric.
    Score(Score),
    /// A raw question document, sent unchanged.
    Raw(Value),
}

impl Question {
    /// Renders the question as its wire object.
    pub(crate) fn to_wire(&self) -> Value {
        match self {
            Self::Noul(question) => question.to_wire(),
            Self::Choice(question) => question.to_wire(),
            Self::Score(question) => question.to_wire(),
            Self::Raw(value) => value.clone(),
        }
    }

    /// Returns the `type` discriminator of a raw question, when it has a nonempty string one.
    pub(crate) fn raw_type(&self) -> Option<&str> {
        match self {
            Self::Raw(value) => value
                .as_object()
                .and_then(|object| object.get(TYPE_FIELD))
                .and_then(Value::as_str)
                .filter(|tag| !tag.is_empty()),
            _ => None,
        }
    }

    /// Returns the raw question document, when this question is raw.
    pub(crate) fn raw_object(&self) -> Option<&Map<String, Value>> {
        match self {
            Self::Raw(value) => value.as_object(),
            _ => None,
        }
    }
}

impl From<Noul> for Question {
    fn from(question: Noul) -> Self {
        Self::Noul(question)
    }
}

impl From<Choice> for Question {
    fn from(question: Choice) -> Self {
        Self::Choice(question)
    }
}

impl From<Score> for Question {
    fn from(question: Score) -> Self {
        Self::Score(question)
    }
}

impl From<Value> for Question {
    fn from(value: Value) -> Self {
        Self::Raw(value)
    }
}

impl From<Map<String, Value>> for Question {
    fn from(value: Map<String, Value>) -> Self {
        Self::Raw(Value::Object(value))
    }
}

impl Serialize for NoulCriteria {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl Serialize for Noul {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl Serialize for Choice {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl Serialize for Score {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl Serialize for Question {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_wire().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NoulCriteria {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Noul::from_wire(&value)
            .map(|question| question.criteria.unwrap_or_default())
            .map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Noul {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        check_tag(&value, "noul").map_err(serde::de::Error::custom)?;
        Self::from_wire(&value).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Choice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        check_tag(&value, "choice").map_err(serde::de::Error::custom)?;
        Self::from_wire(&value).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Score {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        check_tag(&value, "score").map_err(serde::de::Error::custom)?;
        Self::from_wire(&value).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Question {
    /// Decodes a question, keeping anything that is not a decodable known type as [`Question::Raw`].
    ///
    /// Raw documents are what the request path sends for question types this SDK does not model, so
    /// serializing a decoded question reproduces the document it was read from.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let typed = match value
            .as_object()
            .and_then(|object| object.get(TYPE_FIELD))
            .and_then(Value::as_str)
        {
            Some("noul") => Noul::from_wire(&value).map(Self::Noul).ok(),
            Some("choice") => Choice::from_wire(&value).map(Self::Choice).ok(),
            Some("score") => Score::from_wire(&value).map(Self::Score).ok(),
            _ => None,
        };
        Ok(typed.unwrap_or(Self::Raw(value)))
    }
}

/// Adds the `type` discriminator to a question's wire object.
fn with_tag(tag: &str, fields: Map<String, Value>) -> Value {
    let mut object = Map::with_capacity(fields.len() + 1);
    object.insert(TYPE_FIELD.to_owned(), Value::String(tag.to_owned()));
    object.extend(fields);
    Value::Object(object)
}

/// Reads an optional content field, treating `null` and absence alike.
fn optional_content(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<JsonContent>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(content_from(value))),
    }
}

/// Reads JSON content, mapping JSON strings onto [`JsonContent::Text`].
fn content_from(value: &Value) -> JsonContent {
    match value {
        Value::String(text) => JsonContent::text(text.clone()),
        other => JsonContent::Structured(other.clone()),
    }
}

/// Rejects a wire object whose `type` is present and different from `expected`.
fn check_tag(value: &Value, expected: &str) -> Result<(), String> {
    match value.as_object().and_then(|object| object.get(TYPE_FIELD)) {
        Some(Value::String(tag)) if tag == expected => Ok(()),
        Some(Value::String(tag)) => Err(format!(
            "expected question type {expected:?}, found {tag:?}"
        )),
        _ => Ok(()),
    }
}

/// Validates the questions and renders the `questions` object of a request body.
pub(crate) fn normalize(questions: &[(String, Question)]) -> Result<Value, InvalidInputError> {
    if questions.is_empty() {
        return Err(InvalidInputError::new("At least one question is required."));
    }
    let mut object = Map::with_capacity(questions.len());
    for (name, question) in questions {
        validate(name, question)?;
        object.insert(name.clone(), question.to_wire());
    }
    Ok(Value::Object(object))
}

/// Applies the pre-network validation rules for one question.
fn validate(name: &str, question: &Question) -> Result<(), InvalidInputError> {
    match question {
        Question::Noul(_) | Question::Choice(_) => Ok(()),
        Question::Score(score) => validate_score_criteria(name, score.criteria.len()),
        Question::Raw(_) => {
            let Some(object) = question.raw_object() else {
                return Err(invalid_question(name));
            };
            let Some(tag) = question.raw_type() else {
                return Err(invalid_question(name));
            };
            if (tag == "choice" || tag == "score") && !object.contains_key("criteria") {
                return Err(InvalidInputError::new(format!(
                    "Question \"{name}\" requires \"criteria\"."
                )));
            }
            if tag == "score" && !object.get("criteria").is_some_and(is_truthy) {
                return Err(score_criteria_error(name));
            }
            Ok(())
        }
    }
}

/// Reports a raw question that is not an object with a nonempty string `type`.
fn invalid_question(name: &str) -> InvalidInputError {
    InvalidInputError::new(format!(
        "Question \"{name}\" must be a question object or a dictionary with a nonempty string \"type\"."
    ))
}

/// Reports a score question that carries no rubric entries.
fn score_criteria_error(name: &str) -> InvalidInputError {
    InvalidInputError::new(format!(
        "Score question \"{name}\" has no criteria; at least one score is required."
    ))
}

/// Rejects a score question with no criteria; at least one score is required.
fn validate_score_criteria(name: &str, criteria: usize) -> Result<(), InvalidInputError> {
    if criteria == 0 {
        return Err(score_criteria_error(name));
    }
    Ok(())
}

/// Mirrors Python truthiness for raw `criteria` values, which are not necessarily arrays.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(question: impl Serialize) -> Value {
        serde_json::to_value(&question).unwrap()
    }

    fn question(name: &str, question: impl Into<Question>) -> (String, Question) {
        (name.to_owned(), question.into())
    }

    #[test]
    fn noul_omits_only_unset_fields() {
        assert_eq!(wire(Noul::new()), json!({"type": "noul"}));
        assert_eq!(
            wire(Noul::new().instructions("")),
            json!({"type": "noul", "instructions": ""})
        );
        assert_eq!(
            wire(
                Noul::new()
                    .instructions(Vec::<Value>::new())
                    .criteria(NoulCriteria::new())
            ),
            json!({"type": "noul", "instructions": [], "criteria": {}})
        );
        assert_eq!(
            wire(Noul::new().criteria(NoulCriteria::new().yes(Value::Null))),
            json!({"type": "noul", "criteria": {"true": null}})
        );
        assert_eq!(
            wire(Noul::new().criteria(NoulCriteria::new().no("no").yes("yes"))),
            json!({"type": "noul", "criteria": {"true": "yes", "false": "no"}})
        );
        assert_eq!(
            wire(Noul::new().instructions(json!({"summary": "spam", "examples": ["Buy now"]}))),
            json!({"type": "noul", "instructions": {"summary": "spam", "examples": ["Buy now"]}})
        );
    }

    #[test]
    fn choice_emits_criteria_and_keeps_undescribed_labels_null() {
        assert_eq!(
            wire(Choice::new(["a"])),
            json!({"type": "choice", "criteria": {"a": null}})
        );
        assert_eq!(
            wire(
                Choice::new(["a"])
                    .instructions("Tone?")
                    .describe("b", "second")
                    .undescribed("c")
            ),
            json!({
                "type": "choice",
                "instructions": "Tone?",
                "criteria": {"a": null, "b": "second", "c": null},
            })
        );
        let criteria = BTreeMap::from([("only".to_owned(), None)]);
        assert_eq!(
            wire(Choice::from_criteria(criteria)),
            json!({"type": "choice", "criteria": {"only": null}})
        );
        assert_eq!(
            wire(Choice::new(Vec::<String>::new())),
            json!({"type": "choice", "criteria": {}})
        );
    }

    #[test]
    fn score_emits_an_ordered_criteria_array() {
        assert_eq!(
            wire(Score::new(["bad", "good"])),
            json!({"type": "score", "criteria": ["bad", "good"]})
        );
        assert_eq!(
            wire(Score::new([json!({"level": 0}), json!({"level": 1})]).instructions("Quality?")),
            json!({"type": "score", "instructions": "Quality?", "criteria": [{"level": 0}, {"level": 1}]})
        );
        assert_eq!(
            wire(Score::new(Vec::<String>::new())),
            json!({"type": "score", "criteria": []})
        );
    }

    #[test]
    fn raw_questions_pass_through_verbatim() {
        let raw = json!({"type": "noul", "instructions": "Spam?", "weight": 3, "criteria": {"future": "kept"}});
        let questions = vec![
            question("raw", raw.clone()),
            question("typed", Noul::new().instructions("Spam?")),
        ];
        assert_eq!(
            normalize(&questions).unwrap(),
            json!({
                "raw": {"type": "noul", "instructions": "Spam?", "weight": 3, "criteria": {"future": "kept"}},
                "typed": {"type": "noul", "instructions": "Spam?"},
            })
        );
    }

    #[test]
    fn questions_round_trip_through_serde() {
        let questions = vec![
            Question::from(
                Noul::new()
                    .instructions("Spam?")
                    .criteria(NoulCriteria::new().yes("yes")),
            ),
            Question::from(Choice::new(["a", "b"]).instructions("Tone?")),
            Question::from(Score::new(["bad", "good"])),
        ];
        for question in questions {
            let encoded = wire(&question);
            let decoded: Question = serde_json::from_value(encoded.clone()).unwrap();
            assert_eq!(decoded, question);
        }
        // Anything that is not a decodable known type stays raw, so nothing is lost on a round trip.
        for raw in [json!({"type": "future"}), json!({"type": 4}), json!({})] {
            let decoded: Question = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(decoded, Question::Raw(raw.clone()), "{raw}");
            assert_eq!(wire(&decoded), raw);
        }

        // A known type decodes into its typed variant, which drops the explicit nulls Python omits too.
        assert_eq!(
            serde_json::from_value::<Question>(json!({"type": "noul", "instructions": null}))
                .unwrap(),
            Question::from(Noul::new())
        );
    }

    #[test]
    fn typed_deserialization_rejects_other_tags() {
        assert!(serde_json::from_value::<Noul>(json!({"type": "choice", "criteria": {}})).is_err());
        assert!(serde_json::from_value::<Choice>(json!({"type": "noul"})).is_err());
        assert!(serde_json::from_value::<Score>(json!({"criteria": "not-an-array"})).is_err());
        assert!(serde_json::from_value::<Choice>(json!({"type": "choice"})).is_err());
        let noul: Noul =
            serde_json::from_value(json!({"type": "noul", "criteria": {"false": "no"}})).unwrap();
        assert_eq!(
            noul.criteria,
            Some(NoulCriteria {
                yes: None,
                no: Some(JsonContent::text("no"))
            })
        );
    }

    #[test]
    fn validation_rejects_empty_and_malformed_questions() {
        let cases = [
            json!({}),
            json!({"instructions": "Missing type"}),
            json!({"type": ""}),
            json!({"type": null}),
            json!({"type": 1}),
            json!({"type": ["future"]}),
            json!("noul"),
            json!(null),
        ];
        for invalid in cases {
            let error = normalize(&[question("invalid", invalid.clone())]).unwrap_err();
            assert_eq!(
                error.message(),
                "Question \"invalid\" must be a question object or a dictionary with a nonempty string \"type\".",
                "{invalid}"
            );
        }
    }

    #[test]
    fn validation_requires_nonempty_score_criteria() {
        assert_eq!(
            normalize(&[]).unwrap_err().message(),
            "At least one question is required."
        );
        assert_eq!(
            normalize(&[question("rating", Score::new(Vec::<String>::new()))])
                .unwrap_err()
                .message(),
            "Score question \"rating\" has no criteria; at least one score is required."
        );
        for empty in [json!([]), json!(null), json!({}), json!(""), json!(0)] {
            let raw = json!({"type": "score", "instructions": "Quality?", "criteria": empty});
            assert_eq!(
                normalize(&[question("rating", raw)]).unwrap_err().message(),
                "Score question \"rating\" has no criteria; at least one score is required.",
                "{empty}"
            );
        }
        assert!(
            normalize(&[question(
                "rating",
                json!({"type": "score", "criteria": ["good"]})
            )])
            .is_ok()
        );
    }

    #[test]
    fn validation_accepts_typed_and_raw_choices() {
        assert!(normalize(&[question("q", Noul::new())]).is_ok());
        assert!(normalize(&[question("q", Choice::new(Vec::<String>::new()))]).is_ok());
        assert!(normalize(&[question("q", json!({"type": "choice", "criteria": {}}))]).is_ok());
        assert!(
            normalize(&[question(
                "q",
                json!({"type": "future", "nested": {"k": null}})
            )])
            .is_ok()
        );
        assert!(normalize(&[question("q", json!({"type": "noul", "weight": 3}))]).is_ok());
        for missing in [json!({"type": "choice"}), json!({"type": "score"})] {
            assert_eq!(
                normalize(&[question("q", missing.clone())])
                    .unwrap_err()
                    .message(),
                "Question \"q\" requires \"criteria\".",
                "{missing}"
            );
        }
    }
}
