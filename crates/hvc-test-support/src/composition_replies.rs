//! Scripted worker replies for pipeline v4 (ADR-022): valid
//! `UnitModelResponseV1` documents built from the handles the host rendered
//! in a unit prompt, never invented. Shared by the integration suites and
//! the composition benchmark (fixture mode).
use crate::RecordedRequest;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const ORIGINAL_SOURCE: &str = "pub fn validate_token(expires_at: u64, now: u64) -> bool {\n    let _ = (expires_at, now);\n    true\n}\n";
pub const IMPLEMENTED_SOURCE: &str =
    "pub fn validate_token(expires_at: u64, now: u64) -> bool {\n    expires_at > now\n}\n";
pub const AUTHORED_TEST: &str =
    "#[test]\nfn expired_token_is_rejected() {\n    assert!(!auth::validate_token(1, 2));\n}\n";

/// One row of the rendered reference table.
#[derive(Debug, Clone)]
pub struct ReferenceRow {
    pub role: String,
    pub path: String,
    pub owned: bool,
    pub planned: bool,
    pub candidate: bool,
}

/// What the host told the worker: task/criterion handles and the reference
/// table rows, parsed from the rendered prompt.
pub struct PromptFacts {
    pub executor: String,
    pub tasks: Vec<String>,
    pub criteria: Vec<(String, String)>,
    pub references: BTreeMap<String, ReferenceRow>,
    pub user_prompt: String,
}

impl PromptFacts {
    pub fn handle_for_path(&self, path: &str) -> &str {
        self.references
            .iter()
            .find(|(_, row)| row.path == path && !row.candidate)
            .map(|(handle, _)| handle.as_str())
            .unwrap_or_else(|| panic!("reference for `{path}` in {:?}", self.references))
    }

    pub fn owned(&self) -> Vec<(&str, &ReferenceRow)> {
        self.references
            .iter()
            .filter(|(_, row)| row.owned)
            .map(|(handle, row)| (handle.as_str(), row))
            .collect()
    }

    pub fn candidates(&self) -> Vec<(&str, &ReferenceRow)> {
        self.references
            .iter()
            .filter(|(_, row)| row.candidate)
            .map(|(handle, row)| (handle.as_str(), row))
            .collect()
    }

    /// A readable non-owned, non-candidate source handle, if any.
    pub fn a_source(&self) -> Option<&str> {
        self.references
            .iter()
            .find(|(_, row)| !row.owned && !row.candidate && !row.planned)
            .map(|(handle, _)| handle.as_str())
    }

    /// The criteria of one task, in prompt order.
    pub fn criteria_of(&self, task: &str) -> Vec<&str> {
        self.criteria
            .iter()
            .filter(|(t, _)| t == task)
            .map(|(_, c)| c.as_str())
            .collect()
    }
}

pub fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

pub fn parse_prompt(request: &RecordedRequest) -> PromptFacts {
    let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
    let messages = body["messages"].as_array().expect("messages");
    let system = messages[0]["content"].as_str().unwrap_or_default();
    let user = messages[1]["content"].as_str().unwrap_or_default();
    let executor = between(system, "You are the `", "` worker")
        .expect("executor kind in system prompt")
        .to_owned();
    let mut tasks = Vec::new();
    let mut criteria = Vec::new();
    let mut references = BTreeMap::new();
    let mut in_references = false;
    for line in user.lines() {
        if line.starts_with("## ") {
            in_references = line == "## References";
        }
        if let Some(rest) = line.strip_prefix("- `") {
            if let Some(handle) = rest.split('`').next()
                && handle.starts_with("task_")
            {
                tasks.push(handle.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("  - `")
            && let Some(handle) = rest.split('`').next()
            && handle.starts_with("crit_")
        {
            let task = tasks.last().cloned().expect("criterion under a task");
            criteria.push((task, handle.to_owned()));
        }
        if in_references && line.starts_with("| `") {
            let cells = line
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>();
            if cells.len() >= 4 {
                let handle = cells[0].trim_matches('`').to_owned();
                references.insert(
                    handle,
                    ReferenceRow {
                        role: cells[1].to_owned(),
                        path: cells[2].trim_matches('`').to_owned(),
                        owned: cells[3].contains("(owned"),
                        planned: cells[3].starts_with("absent"),
                        candidate: cells[3].starts_with("validated output of a predecessor"),
                    },
                );
            }
        }
    }
    PromptFacts {
        executor,
        tasks,
        criteria,
        references,
        user_prompt: user.to_owned(),
    }
}

pub fn handle_for_path<'a>(facts: &'a PromptFacts, path: &str) -> &'a str {
    facts.handle_for_path(path)
}

/// How the scripted worker fills its reply: the file contents it "writes"
/// (by path; a default is used otherwise) and the unit proposals the
/// specification author declares.
#[derive(Debug, Clone)]
pub struct ReplyOptions {
    pub contents: BTreeMap<String, String>,
    /// `unitProposals` for the specification; `None` proposes one code unit
    /// per task writing the delivered production sources.
    pub proposals: Option<Value>,
    /// The specification's interface intent. `change` (the default) makes a
    /// real Contract call necessary; `preserve` takes the host's zero-call
    /// path and no `contract_author` reply is ever requested.
    pub interface_intent: &'static str,
    /// `unitBindings` for the contract; `None` binds every planned unit the
    /// prompt lists (or the default `implement_expiry` proposal).
    pub bindings: Option<Value>,
}

impl Default for ReplyOptions {
    fn default() -> Self {
        Self {
            contents: BTreeMap::new(),
            proposals: None,
            interface_intent: "change",
            bindings: None,
        }
    }
}

fn content_for(options: &ReplyOptions, path: &str) -> String {
    options.contents.get(path).cloned().unwrap_or_else(|| {
        if path.ends_with("_test.rs") || path.starts_with("tests/") {
            AUTHORED_TEST.to_owned()
        } else if path == "src/auth.rs" {
            IMPLEMENTED_SOURCE.to_owned()
        } else {
            format!("// implemented {path}\npub fn generated() {{}}\n")
        }
    })
}

/// One valid reply per specialty from the delivered handles only.
pub fn unit_response(facts: &PromptFacts) -> Value {
    unit_response_with(facts, &ReplyOptions::default())
}

pub fn unit_response_with(facts: &PromptFacts, options: &ReplyOptions) -> Value {
    let task = facts.tasks[0].as_str();
    let criterion = facts.criteria[0].1.as_str();
    let source_handle = facts
        .references
        .iter()
        .find(|(_, row)| row.path == "src/auth.rs" && !row.candidate)
        .map(|(handle, _)| handle.clone())
        .or_else(|| facts.a_source().map(str::to_owned))
        .unwrap_or_else(|| facts.references.keys().next().cloned().unwrap_or_default());
    let source = source_handle.as_str();
    // A greenfield workspace renders an empty References table for Intake
    // (there is no source to list): `usedRefs` must then be `[]`, per the
    // contract's own instruction, never a synthesized/empty handle.
    let used_refs: &[&str] = if source.is_empty() { &[] } else { &[source] };
    let payload = match facts.executor.as_str() {
        "intake_classifier" => json!({
            "tasks": facts.tasks.iter().map(|task| json!({"taskRef": task, "classification": "bugfix", "risk": "low", "summary": "Correct the token expiry check."})).collect::<Vec<_>>(),
            "issues": [],
            "usedRefs": used_refs
        }),
        "specification_author" => {
            let existing = facts
                .references
                .iter()
                .filter(|(_, row)| !row.planned && !row.candidate)
                .map(|(handle, _)| handle.clone())
                .collect::<Vec<_>>();
            let planned = facts
                .references
                .iter()
                .filter(|(_, row)| row.planned)
                .map(|(handle, _)| handle.clone())
                .collect::<Vec<_>>();
            let requirements = facts
                .criteria
                .iter()
                .map(|(task, criterion)| json!({
                    "taskRef": task, "criterionRef": criterion,
                    "requirement": "validate_token returns false when expires_at is not after now.",
                    "verificationIntent": "Author a unit test that calls the validator with an expired token.",
                    "verificationMode": "document_check", "requiredCheckRefs": [], "notApplicableReason": null
                }))
                .collect::<Vec<_>>();
            let proposals = options.proposals.clone().unwrap_or_else(|| {
                json!([{
                    "unitKey": "implement_expiry", "kind": "code", "taskRefs": [task], "criterionRefs": [criterion],
                    "inputRefs": [source], "destinationRefs": [source], "dependsOn": [], "objective": "Implement the expiry comparison."
                }])
            });
            json!({
                "objective": "Reject expired tokens in the validator.",
                "exclusions": ["No change to the token format."],
                "decisions": ["Keep the public signature of validate_token."],
                "requirements": requirements,
                "existingFileRefs": existing,
                "plannedFileRefs": planned,
                "interfaceIntent": [{"intent": options.interface_intent, "sourceRefs": [source], "criterionRefs": [criterion], "description": "The validator signature and its expiry contract."}],
                "unitProposals": proposals
            })
        }
        "contract_author" => {
            let keys = facts
                .user_prompt
                .lines()
                .filter_map(|line| line.strip_prefix("- `"))
                .filter(|rest| rest.contains("): "))
                .filter_map(|rest| rest.split('`').next())
                .filter(|key| !key.starts_with("task_") && !key.starts_with("crit_"))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let bindings = if let Some(bindings) = options.bindings.clone() {
                bindings
            } else if keys.is_empty() {
                json!([{"unitKey": "implement_expiry", "interfaceKeys": ["validator"], "dependsOn": []}])
            } else {
                Value::Array(
                    keys.iter()
                        .map(|key| json!({"unitKey": key, "interfaceKeys": ["validator"], "dependsOn": []}))
                        .collect(),
                )
            };
            json!({
                "groups": [{"interfaceKey": "validator", "intent": "change", "sourceRefs": [source], "criterionRefs": [criterion],
                            "signature": "pub fn validate_token(expires_at: u64, now: u64) -> bool", "invariants": ["Returns false for an expired token."]}],
                "unitBindings": bindings,
                "usedRefs": [source]
            })
        }
        "code_implementer" | "code_repairer" | "test_author" | "test_repairer" => {
            let mut edits = Vec::new();
            let mut keys = Vec::new();
            // A worker leaves owned documents alone unless the fixture
            // provides their content: a host-rendered note is not code.
            let owned = facts
                .owned()
                .into_iter()
                .filter(|(_, row)| {
                    row.role != "document" || options.contents.contains_key(&row.path)
                })
                .collect::<Vec<_>>();
            for (index, (handle, row)) in owned.into_iter().enumerate() {
                let key = format!("edit_{}", index + 1);
                let content = content_for(options, &row.path);
                let edit = if row.planned {
                    json!({"operation": "create_file", "destinationRef": handle, "content": content})
                } else {
                    json!({"operation": "replace_file", "sourceRef": handle, "content": content})
                };
                edits.push(json!({"artifactKey": key, "edit": edit}));
                keys.push(key);
            }
            let inputs = facts
                .candidates()
                .first()
                .map(|(handle, _)| handle.to_string())
                .or_else(|| facts.a_source().map(str::to_owned))
                .map(|handle| vec![handle])
                .unwrap_or_default();
            let evidence = facts
                .tasks
                .iter()
                .map(|task| json!({
                    "taskRef": task, "criterionRefs": facts.criteria_of(task), "artifactKeys": keys,
                    "inputRefs": inputs, "explanation": "The owned files implement the approved behavior."
                }))
                .collect::<Vec<_>>();
            if facts.executor.starts_with("code") {
                json!({"edits": edits, "evidence": evidence, "usedRefs": inputs})
            } else {
                let coverage = facts
                    .criteria
                    .iter()
                    .map(|(task, criterion)| json!({
                        "taskRef": task, "criterionRef": criterion, "coverage": "authored", "testArtifactKeys": keys,
                        "testInputRefs": [], "checkRefs": [], "explanation": "A unit test exercises the criterion."
                    }))
                    .collect::<Vec<_>>();
                json!({"edits": edits, "coverage": coverage, "evidence": evidence, "usedRefs": inputs})
            }
        }
        "integration_reviewer" => json!({
            "assessments": facts.criteria.iter().map(|(task, criterion)| json!({
                "taskRef": task, "criterionRef": criterion, "verdict": "supported", "inputRefs": [source], "checkRefs": [],
                "explanation": "The implementation and the authored test agree with the requirement."
            })).collect::<Vec<_>>(),
            "blockers": [],
            "documentationEdits": [],
            "usedRefs": [source]
        }),
        other => panic!("unexpected executor `{other}`"),
    };
    json!({"schemaVersion": 1, "outcome": "completed", "executorKind": facts.executor, "payload": payload})
}
