//! Selected response schemas per executor specialty.
//!
//! The documented registry (`fixtures/stage-composition-v1.schema.json`) is
//! the single source; nothing here is hand-written twice. A model only ever
//! receives the *selected* schema for its specialty: the `completed` branch
//! bound to its `executorKind` and payload type, plus `needs_input` and
//! `failed`, with the transitively referenced `$defs` and nothing else.
//!
//! Two flavors exist because providers lower JSON Schema differently:
//!
//! - [`SchemaFlavor::Canonical`] is the registry shape (`anyOf` root, absent
//!   keys for the other outcomes). Used where the schema is textual guidance
//!   or the provider accepts a union root.
//! - [`SchemaFlavor::Strict`] is the OpenAI Structured Outputs lowering: an
//!   object root, every property required, non-applicable outcome fields
//!   explicitly nullable, `const` as single-value `enum`, no `oneOf`, no
//!   `uniqueItems`/`format`. The typed layer maps `null` back to "absent", so
//!   both flavors produce the same [`super::UnitModelResponseV1`].
//!
//! Host-only rules (handle resolution, ownership, hashes) are never encoded in
//! these schemas; see `validate.rs`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::ExecutorKind;

const REGISTRY_JSON: &str = include_str!("fixtures/stage-composition-v1.schema.json");
pub const RESPONSE_SCHEMA_NAME: &str = "hvc_unit_response";

fn registry() -> &'static Value {
    static REGISTRY: OnceLock<Value> = OnceLock::new();
    REGISTRY.get_or_init(|| serde_json::from_str(REGISTRY_JSON).expect("bundled registry parses"))
}

fn registry_defs() -> &'static Map<String, Value> {
    registry()["$defs"]
        .as_object()
        .expect("registry carries $defs")
}

/// One named `$defs` entry of the bundled registry.
pub fn definition(name: &str) -> Option<&'static Value> {
    registry_defs().get(name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFlavor {
    Canonical,
    /// Claude Code turns `--json-schema` into an Anthropic tool (`StructuredOutput`),
    /// whose input schema strictly requires `type: object` and rejects `anyOf`/`oneOf`
    /// at the top level. We wrap the canonical union inside a `response` property so
    /// the tool schema is accepted while omitting the meta-schema declaration.
    Claude,
    Strict,
}

impl SchemaFlavor {
    /// Profiles that send the schema natively with `strict: true` receive the
    /// strict lowering; Claude Code receives its compatible canonical flavor;
    /// every other transport (Anthropic, and OpenAI-compatible endpoints that
    /// only offer `json_object`) answers the registry shape.
    pub fn for_profile(profile: &crate::llm::profiles::ProviderProfile) -> Self {
        if crate::llm::domain::enforces_strict_schema(profile) {
            Self::Strict
        } else {
            Self::Canonical
        }
    }

    pub fn for_cli_backend(backend: crate::terminals::CliBackend) -> Self {
        use crate::terminals::CliBackend;
        match backend {
            CliBackend::Codex => Self::Strict,
            CliBackend::Claude => Self::Claude,
            // Antigravity takes the canonical schema natively through
            // `--json-schema`, `$schema` key included.
            CliBackend::Gemini
            | CliBackend::Opencode
            | CliBackend::Antigravity
            | CliBackend::Custom => Self::Canonical,
        }
    }
}

/// The registry definition name of the payload each specialty returns.
pub const fn payload_definition(kind: ExecutorKind) -> &'static str {
    match kind {
        ExecutorKind::IntakeClassifier => "IntakePayloadV1",
        ExecutorKind::SpecificationAuthor => "SpecificationContractV1",
        ExecutorKind::ContractAuthor => "InterfaceContractV1",
        ExecutorKind::CodeImplementer | ExecutorKind::CodeRepairer => "EditSetV1",
        ExecutorKind::TestAuthor | ExecutorKind::TestRepairer => "TestPayloadV1",
        ExecutorKind::IntegrationReviewer => "IntegrationPayloadV1",
    }
}

fn union_branches() -> &'static [Value] {
    registry_defs()["UnitModelResponseV1"]["oneOf"]
        .as_array()
        .expect("response union")
}

fn completed_branch(kind: ExecutorKind) -> &'static Value {
    union_branches()
        .iter()
        .find(|branch| branch["properties"]["executorKind"]["const"] == kind.as_str())
        .expect("every specialty has a completed branch")
}

fn outcome_branch(outcome: &str) -> &'static Value {
    union_branches()
        .iter()
        .find(|branch| branch["properties"]["outcome"]["const"] == outcome)
        .expect("outcome branch exists")
}

/// Walks `$ref` pointers and returns the transitive closure of definitions the
/// given roots depend on, in name order.
fn collect_defs(roots: &[&Value]) -> BTreeMap<String, Value> {
    fn visit(value: &Value, out: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(fields) => {
                if let Some(Value::String(reference)) = fields.get("$ref")
                    && let Some(name) = reference.strip_prefix("#/$defs/")
                    && !out.contains_key(name)
                {
                    let definition = registry_defs()
                        .get(name)
                        .unwrap_or_else(|| panic!("registry lacks `{name}`"))
                        .clone();
                    out.insert(name.to_owned(), definition.clone());
                    visit(&definition, out);
                }
                fields.values().for_each(|child| visit(child, out));
            }
            Value::Array(items) => items.iter().for_each(|item| visit(item, out)),
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    roots.iter().for_each(|root| visit(root, &mut out));
    out
}

/// OpenAI Structured Outputs lowering, applied recursively.
///
/// Walks only schema positions (`properties` values, `$defs` values, `items`,
/// `anyOf`/`allOf` branches) so a *property named* `description` or an enum
/// literal is never mistaken for the annotation keyword.
fn lower_strict(schema: &mut Value) {
    let Value::Object(fields) = schema else {
        return;
    };
    if let Some(branches) = fields.remove("oneOf") {
        fields.insert("anyOf".into(), branches);
    }
    if let Some(constant) = fields.remove("const") {
        fields.insert("enum".into(), Value::Array(vec![constant]));
    }
    for annotation in [
        "uniqueItems",
        "format",
        "description",
        "title",
        "$schema",
        "$id",
    ] {
        fields.remove(annotation);
    }
    for (keyword, child) in fields.iter_mut() {
        match keyword.as_str() {
            "properties" | "$defs" => {
                if let Value::Object(map) = child {
                    map.values_mut().for_each(lower_strict);
                }
            }
            "anyOf" | "allOf" => {
                if let Value::Array(branches) = child {
                    branches.iter_mut().for_each(lower_strict);
                }
            }
            "items" | "additionalProperties" | "not" => lower_strict(child),
            _ => {}
        }
    }
}

fn nullable(schema: Value) -> Value {
    json!({ "anyOf": [schema, { "type": "null" }] })
}

/// The schema a worker of `kind` must satisfy, in the requested flavor.
pub fn executor_response_schema(kind: ExecutorKind, flavor: SchemaFlavor) -> Value {
    let completed = completed_branch(kind);
    let needs_input = outcome_branch("needs_input");
    let failed = outcome_branch("failed");
    let defs = collect_defs(&[completed, needs_input, failed]);
    match flavor {
        SchemaFlavor::Canonical => json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": RESPONSE_SCHEMA_NAME,
            "anyOf": [completed, needs_input, failed],
            "$defs": defs,
        }),
        SchemaFlavor::Claude => json!({
            "title": RESPONSE_SCHEMA_NAME,
            "type": "object",
            "properties": {
                "response": {
                    "anyOf": [completed, needs_input, failed],
                },
            },
            "required": ["response"],
            "additionalProperties": false,
            "$defs": defs,
        }),
        SchemaFlavor::Strict => {
            let payload = json!({ "$ref": format!("#/$defs/{}", payload_definition(kind)) });
            let mut schema = json!({
                "title": RESPONSE_SCHEMA_NAME,
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "schemaVersion", "outcome", "executorKind", "payload",
                    "missingRefs", "questions", "code", "message", "relatedRefs"
                ],
                "properties": {
                    "schemaVersion": { "enum": [1] },
                    "outcome": { "enum": ["completed", "needs_input", "failed"] },
                    "executorKind": nullable(json!({ "enum": [kind.as_str()] })),
                    "payload": nullable(payload),
                    "missingRefs": nullable(needs_input["properties"]["missingRefs"].clone()),
                    "questions": nullable(needs_input["properties"]["questions"].clone()),
                    "code": nullable(failed["properties"]["code"].clone()),
                    "message": nullable(failed["properties"]["message"].clone()),
                    "relatedRefs": nullable(failed["properties"]["relatedRefs"].clone()),
                },
                "$defs": defs,
            });
            lower_strict(&mut schema);
            schema
        }
    }
}

/// Compiles a selected schema. Formats are validated only in the canonical
/// flavor; the strict lowering removes them by construction.
pub fn compile(schema: &Value) -> Result<jsonschema::Validator, String> {
    jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .map_err(|error| error.to_string())
}

/// Fills the strict-flavor keys a canonical instance leaves absent with
/// `null`, without touching anything else. Used to compare provider-lowered
/// acceptance against the registry.
pub fn lift_to_strict(instance: &Value) -> Value {
    let mut lifted = instance.clone();
    if let Value::Object(fields) = &mut lifted {
        for key in [
            "executorKind",
            "payload",
            "missingRefs",
            "questions",
            "code",
            "message",
            "relatedRefs",
        ] {
            fields.entry(key).or_insert(Value::Null);
        }
    }
    lifted
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    fn assert_strict(schema: &Value) {
        if schema["type"] == "object" {
            assert_eq!(schema["additionalProperties"], false, "{schema}");
            let properties = schema["properties"].as_object().expect("object properties");
            let required = schema["required"].as_array().expect("object required");
            assert_eq!(properties.len(), required.len(), "{schema}");
            assert!(properties.keys().all(|key| required.contains(&json!(key))));
            properties.values().for_each(assert_strict);
        }
        assert!(schema.get("oneOf").is_none());
        assert!(schema.get("const").is_none());
        assert!(schema.get("uniqueItems").is_none());
        assert!(schema.get("format").is_none());
        assert!(schema.get("title").is_none());
        if let Some(branches) = schema["anyOf"].as_array() {
            branches.iter().for_each(assert_strict);
        }
        if let Some(items) = schema.get("items") {
            assert_strict(items);
        }
        if let Some(defs) = schema["$defs"].as_object() {
            defs.values().for_each(assert_strict);
        }
    }

    fn response_examples() -> Vec<fixtures::Example> {
        fixtures::examples()
            .into_iter()
            .filter(|example| example.definition == "UnitModelResponseV1")
            .collect()
    }

    fn example_kind(example: &fixtures::Example) -> Option<ExecutorKind> {
        example.instance["executorKind"]
            .as_str()
            .and_then(ExecutorKind::parse)
    }

    #[test]
    fn selected_schemas_accept_and_reject_exactly_like_the_registry() {
        let examples = response_examples();
        assert_eq!(examples.len(), 14);
        for flavor in [
            SchemaFlavor::Canonical,
            SchemaFlavor::Claude,
            SchemaFlavor::Strict,
        ] {
            for kind in ExecutorKind::ALL {
                let schema = executor_response_schema(kind, flavor);
                let validator = compile(&schema).expect("selected schema compiles");
                for example in &examples {
                    let instance = match flavor {
                        SchemaFlavor::Canonical => example.instance.clone(),
                        SchemaFlavor::Claude => json!({ "response": example.instance.clone() }),
                        SchemaFlavor::Strict => lift_to_strict(&example.instance),
                    };
                    let expected = example.schema_valid
                        && (example.instance["outcome"] != "completed"
                            || example_kind(example) == Some(kind));
                    assert_eq!(
                        validator.is_valid(&instance),
                        expected,
                        "{flavor:?} {kind:?} `{}`",
                        example.name
                    );
                }
            }
        }
    }

    #[test]
    fn strict_flavor_is_openai_compliant_and_canonical_keeps_the_registry_shape() {
        for kind in ExecutorKind::ALL {
            let strict = executor_response_schema(kind, SchemaFlavor::Strict);
            assert_eq!(strict["type"], "object");
            assert_strict(&strict);
            let canonical = executor_response_schema(kind, SchemaFlavor::Canonical);
            let claude = executor_response_schema(kind, SchemaFlavor::Claude);
            assert_eq!(canonical["anyOf"].as_array().unwrap().len(), 3);
            assert_eq!(
                canonical["$schema"],
                "https://json-schema.org/draft/2020-12/schema"
            );
            assert!(claude.get("$schema").is_none());
            assert_eq!(claude["type"], "object");
            assert_eq!(claude["required"], json!(["response"]));
            assert_eq!(claude["additionalProperties"], false);
            assert_eq!(claude["properties"]["response"]["anyOf"], canonical["anyOf"]);
            assert_eq!(claude["$defs"], canonical["$defs"]);
            assert_eq!(
                canonical["anyOf"][0]["properties"]["executorKind"]["const"],
                kind.as_str()
            );
            assert!(
                canonical["anyOf"][0]["properties"]["payload"]["$ref"]
                    .as_str()
                    .unwrap()
                    .ends_with(payload_definition(kind))
            );
        }
    }

    #[test]
    fn selected_schemas_carry_only_their_dependencies_and_stay_small() {
        let intake =
            executor_response_schema(ExecutorKind::IntakeClassifier, SchemaFlavor::Canonical);
        let defs = intake["$defs"].as_object().unwrap();
        assert!(defs.contains_key("IntakePayloadV1"));
        assert!(defs.contains_key("QuestionV1"));
        assert!(!defs.contains_key("EditSetV1"));
        assert!(!defs.contains_key("ValidationReceiptV1"));
        assert!(!defs.contains_key("StageExecutionPlanV1"));
        for kind in ExecutorKind::ALL {
            for flavor in [
                SchemaFlavor::Canonical,
                SchemaFlavor::Claude,
                SchemaFlavor::Strict,
            ] {
                let text = executor_response_schema(kind, flavor).to_string();
                assert!(
                    text.len() < 12_000,
                    "{kind:?} {flavor:?} schema is {} bytes",
                    text.len()
                );
            }
        }
    }

    #[test]
    fn transport_flavors_follow_provider_lowering_rules() {
        use crate::llm::profiles::profile_by_id;
        use crate::terminals::CliBackend;
        let flavor = |id: &str| SchemaFlavor::for_profile(&profile_by_id(id).unwrap());
        // Native structured outputs (`strict: true`) on both OpenAI protocols.
        assert_eq!(flavor("openai"), SchemaFlavor::Strict);
        assert_eq!(flavor("gemini"), SchemaFlavor::Strict);
        // Anthropic and json_object-only OpenAI-compatible endpoints answer the
        // registry shape: no grammar forces the null-padded strict form.
        assert_eq!(flavor("anthropic"), SchemaFlavor::Canonical);
        assert_eq!(flavor("ollama"), SchemaFlavor::Canonical);
        assert_eq!(flavor("groq"), SchemaFlavor::Canonical);
        assert_eq!(
            SchemaFlavor::for_cli_backend(CliBackend::Codex),
            SchemaFlavor::Strict
        );
        assert_eq!(
            SchemaFlavor::for_cli_backend(CliBackend::Claude),
            SchemaFlavor::Claude
        );
    }
}
