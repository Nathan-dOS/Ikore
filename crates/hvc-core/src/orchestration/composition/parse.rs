//! Strict parsing of a worker response (SC-008).
//!
//! Exactly one JSON object, optionally inside exactly one outer Markdown fence.
//! Duplicate keys, trailing content, truncation and schema violations are
//! rejected without salvage: no quote repair, no brace completion, no picking
//! a convenient object. Diagnostics carry the error class and a position,
//! never an excerpt of the model text.

use std::collections::HashMap;
use std::fmt;
use std::sync::{LazyLock, Mutex};

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::schema::{SchemaFlavor, compile, executor_response_schema};
use super::{CompositionError, ErrorCode, ExecutorKind, Invariant, UnitModelResponseV1, Validate};

fn shape(message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc008, ErrorCode::InvalidShape, message)
}

/// Removes one outer ```` ``` ```` fence when the whole text is that fence and
/// nothing else. Any other fence layout is left for the JSON parser to reject.
pub fn strip_single_outer_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let Some(body) = after_open.strip_suffix("```") else {
        return trimmed;
    };
    // The info string (e.g. `json`) ends at the first newline.
    let body = match body.find('\n') {
        Some(newline)
            if body[..newline]
                .trim()
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric()) =>
        {
            &body[newline + 1..]
        }
        _ => return trimmed,
    };
    body.trim()
}

/// A JSON structure whose only purpose is to fail on repeated object keys.
struct NoDuplicateKeys;

impl<'de> de::Deserialize<'de> for NoDuplicateKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = NoDuplicateKeys;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("any JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(NoDuplicateKeys)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<NoDuplicateKeys>()?.is_some() {}
                Ok(NoDuplicateKeys)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut seen = std::collections::HashSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!("duplicate object key `{key}`")));
                    }
                    map.next_value::<NoDuplicateKeys>()?;
                }
                Ok(NoDuplicateKeys)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Parses exactly one JSON object from `text`, rejecting duplicate keys and
/// trailing content. Truncation is reported as `output_truncated` because its
/// recovery differs from a malformed answer.
pub fn parse_single_object(text: &str) -> Result<Value, CompositionError> {
    if let Err(error) = serde_json::from_str::<NoDuplicateKeys>(text) {
        return Err(classify(&error));
    }
    let value: Value = serde_json::from_str(text).map_err(|error| classify(&error))?;
    if !value.is_object() {
        return Err(shape("The response must be a single JSON object."));
    }
    Ok(value)
}

fn classify(error: &serde_json::Error) -> CompositionError {
    let position = format!("line {}, column {}", error.line(), error.column());
    if error.is_eof() {
        CompositionError::new(
            Invariant::Sc008,
            ErrorCode::OutputTruncated,
            format!("The JSON document ended before it was closed ({position})."),
        )
    } else if error.to_string().starts_with("trailing characters") {
        shape(format!(
            "More than one JSON value was returned; exactly one object is required ({position})."
        ))
    } else if error.to_string().contains("duplicate object key") {
        shape(format!("Duplicate object key ({position})."))
    } else {
        shape(format!("Malformed JSON ({position})."))
    }
}

static VALIDATORS: LazyLock<Mutex<HashMap<(ExecutorKind, SchemaFlavor), jsonschema::Validator>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn schema_check(
    kind: ExecutorKind,
    flavor: SchemaFlavor,
    value: &Value,
) -> Result<(), CompositionError> {
    let mut validators = VALIDATORS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let validator = match validators.entry((kind, flavor)) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
            compile(&executor_response_schema(kind, flavor)).map_err(|error| {
                CompositionError::new(
                    Invariant::Sc005,
                    ErrorCode::HostContractError,
                    format!("selected schema does not compile: {error}"),
                )
            })?,
        ),
    };
    validator.validate(value).map_err(|error| {
        shape(format!(
            "The response violates the `{}` contract at `{}`: {}.",
            kind.as_str(),
            error.instance_path,
            error.kind_description()
        ))
    })
}

trait KindDescription {
    fn kind_description(&self) -> String;
}

impl KindDescription for jsonschema::ValidationError<'_> {
    /// The validation keyword that failed, without the offending instance value.
    fn kind_description(&self) -> String {
        let text = format!("{:?}", self.kind);
        text.split(['{', '(', ' '])
            .next()
            .unwrap_or("schema")
            .to_owned()
    }
}

/// The full strict path: fence → duplicates/trailing → selected schema →
/// typed contract → shape constraints → specialty match.
pub fn parse_unit_response(
    raw: &str,
    kind: ExecutorKind,
    flavor: SchemaFlavor,
) -> Result<UnitModelResponseV1, CompositionError> {
    let text = strip_single_outer_fence(raw);
    if text.is_empty() {
        return Err(shape("The response is empty."));
    }
    let value = parse_single_object(text)?;
    if value["outcome"] == "completed"
        && let Some(claimed) = value["executorKind"].as_str()
        && claimed != kind.as_str()
    {
        return Err(CompositionError::new(
            Invariant::Sc005,
            ErrorCode::InvalidShape,
            format!(
                "The response claims specialty `{claimed}` (executorKind) but the unit is `{}`.",
                kind.as_str()
            ),
        ));
    }
    schema_check(kind, flavor, &value)?;
    let response: UnitModelResponseV1 = serde_json::from_value(value).map_err(|error| {
        shape(format!(
            "The response does not match the typed contract: {error}."
        ))
    })?;
    response.validate()?;
    if let UnitModelResponseV1::Completed { executor_kind, .. } = &response
        && *executor_kind != kind
    {
        return Err(CompositionError::new(
            Invariant::Sc005,
            ErrorCode::InvalidShape,
            format!(
                "The response claims specialty `{}` but the unit is `{}`.",
                executor_kind.as_str(),
                kind.as_str()
            ),
        ));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    fn failed_json() -> String {
        serde_json::json!({
            "schemaVersion": 1, "outcome": "failed", "code": "scope_insufficient",
            "message": "The delivered context lacks the caller.", "relatedRefs": ["src_1"]
        })
        .to_string()
    }

    #[test]
    fn one_outer_fence_is_accepted_and_other_layouts_are_not_repaired() {
        let json = failed_json();
        let fenced = format!("```json\n{json}\n```");
        assert_eq!(strip_single_outer_fence(&fenced), json);
        assert_eq!(
            strip_single_outer_fence(&format!("```\n{json}\n```\n")),
            json
        );
        assert_eq!(strip_single_outer_fence(&json), json);
        // Prose around a fence, two fences, or an unterminated fence stay as-is
        // and fail at the JSON layer.
        let prose = format!("Here you go:\n```json\n{json}\n```");
        assert!(
            parse_unit_response(
                &prose,
                ExecutorKind::CodeImplementer,
                SchemaFlavor::Canonical
            )
            .is_err()
        );
        let double = format!("```json\n{json}\n```\n```json\n{json}\n```");
        assert!(
            parse_unit_response(
                &double,
                ExecutorKind::CodeImplementer,
                SchemaFlavor::Canonical
            )
            .is_err()
        );
        let open = format!("```json\n{json}");
        assert!(
            parse_unit_response(
                &open,
                ExecutorKind::CodeImplementer,
                SchemaFlavor::Canonical
            )
            .is_err()
        );
        assert!(
            parse_unit_response(
                &fenced,
                ExecutorKind::CodeImplementer,
                SchemaFlavor::Canonical
            )
            .is_ok()
        );
    }

    #[test]
    fn truncation_duplicates_and_multiple_objects_are_distinct_rejections() {
        let json = failed_json();
        let truncated = &json[..json.len() - 8];
        let error = parse_single_object(truncated).unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputTruncated);
        assert!(
            !error.message.contains("scope_insufficient"),
            "no excerpt: {}",
            error.message
        );

        let two = format!("{json}\n{json}");
        let error = parse_single_object(&two).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidShape);
        assert!(error.message.contains("More than one JSON value"));

        let duplicate = r#"{"schemaVersion":1,"outcome":"failed","outcome":"completed","code":"scope_insufficient","message":"x","relatedRefs":[]}"#;
        let error = parse_single_object(duplicate).unwrap_err();
        assert!(
            error.message.contains("Duplicate object key"),
            "{}",
            error.message
        );

        assert!(parse_single_object("[1,2]").is_err());
        assert!(parse_single_object("\"text\"").is_err());
        // A guessed repair is never attempted: a bare missing quote stays malformed.
        let broken = json.replace("\"scope_insufficient\"", "scope_insufficient");
        assert_eq!(
            parse_single_object(&broken).unwrap_err().code,
            ErrorCode::InvalidShape
        );
    }

    #[test]
    fn every_documented_response_example_parses_through_both_flavors() {
        for example in fixtures::examples() {
            if example.definition != "UnitModelResponseV1" {
                continue;
            }
            let kind = example.instance["executorKind"]
                .as_str()
                .and_then(ExecutorKind::parse)
                .unwrap_or(ExecutorKind::CodeImplementer);
            let canonical = example.instance.to_string();
            let result = parse_unit_response(&canonical, kind, SchemaFlavor::Canonical);
            assert_eq!(
                result.is_ok(),
                example.schema_valid,
                "canonical `{}`: {result:?}",
                example.name
            );
            let strict = super::super::schema::lift_to_strict(&example.instance).to_string();
            let result = parse_unit_response(&strict, kind, SchemaFlavor::Strict);
            assert_eq!(
                result.is_ok(),
                example.schema_valid,
                "strict `{}`: {result:?}",
                example.name
            );
        }
    }

    #[test]
    fn a_completed_payload_for_another_specialty_is_rejected_even_when_well_formed() {
        let example = fixtures::examples()
            .into_iter()
            .find(|example| example.name == "code_anchor_response")
            .unwrap();
        let text = example.instance.to_string();
        assert!(
            parse_unit_response(
                &text,
                ExecutorKind::CodeImplementer,
                SchemaFlavor::Canonical
            )
            .is_ok()
        );
        // The selected schema pins `executorKind`, so the mismatch is caught at
        // the shape layer before any typed or policy check runs.
        let error = parse_unit_response(&text, ExecutorKind::CodeRepairer, SchemaFlavor::Canonical)
            .unwrap_err();
        assert_eq!(error.sc, Invariant::Sc005);
        assert!(error.message.contains("executorKind"), "{}", error.message);
        assert!(
            parse_unit_response(&text, ExecutorKind::TestAuthor, SchemaFlavor::Canonical).is_err()
        );
    }

    #[test]
    fn schema_rejections_name_the_path_and_keyword_without_quoting_content() {
        let text = serde_json::json!({
            "schemaVersion": 1, "outcome": "failed", "code": "scope_insufficient",
            "message": "SECRET-LOOKING-TEXT", "relatedRefs": ["Not A Handle"]
        })
        .to_string();
        let error = parse_unit_response(
            &text,
            ExecutorKind::CodeImplementer,
            SchemaFlavor::Canonical,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidShape);
        assert!(
            !error.message.contains("SECRET-LOOKING-TEXT"),
            "{}",
            error.message
        );
        assert!(!error.message.contains("Not A Handle"), "{}", error.message);
    }
}
