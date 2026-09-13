//! The model-facing prompt for one work unit (SC-007).
//!
//! A unit sees only what it needs to perform its bounded operation: its
//! objective, its tasks and criteria with the approved requirement text, the
//! decisions and exact interfaces that apply, the reference handles it may
//! cite, the exact delivered content with anchor markers, validated dependency
//! outputs, explicit unknowns and a short field guide for its response. It
//! never sees execution fences, publication bases, persistent identifiers,
//! hashes, other units' work or the full contract registry. The JSON Schema is
//! attached by the transport layer, once, only where the provider needs text.

use std::fmt::Write as _;

use super::policy::{ExecutorPolicy, common_rules};
use super::references::ReferenceTable;
use super::{
    AnchorBindingV1, ExecutorKind, Handle, InterfaceGroupV1, ReferenceEntryV1,
    StageExecutionPlanV1, UnavailableReason, WorkUnitSpecV1,
};
use crate::agent_output_language::ENGLISH_ONLY_INSTRUCTION;

/// A delivered source with its exact content and the anchors issued on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredSource<'a> {
    pub handle: &'a Handle,
    pub content: &'a str,
    /// True when `replace_file` is permitted (source and replacement ≤ 8 KiB).
    pub replaceable: bool,
}

/// Bounded diagnostics for a repair attempt. Never carries the rejected raw
/// output: the worker gets the reason and the owned regions, not its previous
/// text to reproduce.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorrectionContext {
    pub diagnostics: Vec<String>,
    pub rejected_artifact_keys: Vec<Handle>,
}

#[derive(Debug, Clone, Default)]
pub struct UnitPromptInputs<'a> {
    /// Approved requirement text per criterion handle.
    pub requirements: Vec<(&'a Handle, &'a str)>,
    pub decisions: &'a [String],
    pub interfaces: &'a [InterfaceGroupV1],
    pub delivered: Vec<DeliveredSource<'a>>,
    pub dependency_outputs: Vec<DeliveredSource<'a>>,
    pub correction: Option<&'a CorrectionContext>,
    /// Recipe-specific guidance placed before the response contract.
    pub extra_instructions: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPrompt {
    pub system: String,
    pub user: String,
    /// Unicode scalar values across both messages; the transport adds the
    /// schema when it embeds one textually.
    pub chars: usize,
}

pub fn system_prompt(policy: &ExecutorPolicy) -> String {
    format!(
        "You are the `{}` worker of the `{}` stage in Hard Vibe Code.\n\n{}\n\n{}\n\n{}",
        policy.kind.as_str(),
        policy.stage.as_str(),
        policy.role_brief,
        common_rules(),
        ENGLISH_ONLY_INSTRUCTION,
    )
}

/// Renders the exact content of a source with `<<anchor_N>>`/`<</anchor_N>>`
/// markers around each issued interval. Markers are on their own lines so the
/// content bytes between them are untouched.
pub fn render_source_with_anchors(content: &str, anchors: &[&AnchorBindingV1]) -> String {
    let mut sorted = anchors.to_vec();
    sorted.sort_by_key(|anchor| (anchor.start_byte, anchor.end_byte));
    let mut out = String::with_capacity(content.len() + 32 * sorted.len());
    let mut cursor = 0usize;
    for anchor in sorted {
        let (start, end) = (anchor.start_byte as usize, anchor.end_byte as usize);
        if start < cursor
            || end > content.len()
            || !content.is_char_boundary(start)
            || !content.is_char_boundary(end)
        {
            continue;
        }
        out.push_str(&content[cursor..start]);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        let _ = writeln!(out, "<<{}>>", anchor.anchor_ref);
        out.push_str(&content[start..end]);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        let _ = writeln!(out, "<</{}>>", anchor.anchor_ref);
        cursor = end;
    }
    out.push_str(&content[cursor..]);
    out
}

fn fence_for(content: &str) -> &'static str {
    if content.contains("````") {
        "`````"
    } else if content.contains("```") {
        "````"
    } else {
        "```"
    }
}

fn availability(entry: &ReferenceEntryV1) -> String {
    match entry {
        ReferenceEntryV1::ExistingSource { .. } => "existing; content delivered below".into(),
        ReferenceEntryV1::PlannedDestination { .. } => {
            "absent; may be created with `create_file`".into()
        }
        ReferenceEntryV1::CandidateSource { .. } => {
            "validated output of a predecessor unit; content delivered below".into()
        }
        ReferenceEntryV1::Unavailable { reason, .. } => match reason {
            UnavailableReason::Missing => "unavailable (missing)".into(),
            UnavailableReason::Ambiguous => "unavailable (ambiguous)".into(),
            UnavailableReason::Unsupported => "unavailable (unsupported)".into(),
        },
    }
}

fn role_name(entry: &ReferenceEntryV1) -> &'static str {
    match entry.source_role() {
        Some(super::SourceRole::Source) => "source",
        Some(super::SourceRole::Test) => "test",
        Some(super::SourceRole::Document) => "document",
        Some(super::SourceRole::Configuration) => "configuration",
        None => "—",
    }
}

/// Response field guide per payload, deliberately shorter than the schema.
pub fn response_field_guide(kind: ExecutorKind) -> &'static str {
    match kind {
        ExecutorKind::IntakeClassifier => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"intake_classifier\",\"payload\":{\"tasks\":[{\"taskRef\",\"classification\",\"risk\",\"summary\"}],\"issues\":[...],\"usedRefs\":[...]}}`. One `tasks` entry per task handle listed above. Or `{\"schemaVersion\":1,\"outcome\":\"failed\",\"code\",\"message\",\"relatedRefs\"}`."
        }
        ExecutorKind::SpecificationAuthor => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"specification_author\",\"payload\":{\"objective\",\"exclusions\":[...],\"decisions\":[...],\"requirements\":[{\"taskRef\",\"criterionRef\",\"requirement\",\"verificationIntent\",\"verificationMode\":\"executed_check|document_check|not_applicable\",\"requiredCheckRefs\":[...],\"notApplicableReason\":null|\"...\"}],\"existingFileRefs\":[...],\"plannedFileRefs\":[...],\"interfaceIntent\":[{\"intent\":\"preserve|change|new|none\",\"sourceRefs\",\"criterionRefs\",\"description\"}],\"unitProposals\":[{\"unitKey\",\"kind\":\"code|test\",\"taskRefs\",\"criterionRefs\",\"inputRefs\",\"destinationRefs\",\"dependsOn\",\"objective\"}]}}`. Exactly one requirement per criterion handle. When a human decision is missing: `{\"schemaVersion\":1,\"outcome\":\"needs_input\",\"missingRefs\":[...],\"questions\":[{\"key\",\"taskRefs\",\"question\",\"rationale\"}]}`."
        }
        ExecutorKind::ContractAuthor => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"contract_author\",\"payload\":{\"groups\":[{\"interfaceKey\",\"intent\":\"preserve|change|new\",\"sourceRefs\",\"criterionRefs\",\"signature\",\"invariants\":[...]}],\"unitBindings\":[{\"unitKey\",\"interfaceKeys\",\"dependsOn\"}],\"usedRefs\":[...]}}` or a `failed` outcome with `code` in contract_conflict|scope_insufficient|implementation_mismatch|unsupported_operation."
        }
        ExecutorKind::CodeImplementer | ExecutorKind::CodeRepairer => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"<your specialty>\",\"payload\":{\"edits\":[{\"artifactKey\",\"edit\":{\"operation\":\"create_file\",\"destinationRef\",\"content\"}|{\"operation\":\"replace_file\",\"sourceRef\",\"content\"}|{\"operation\":\"replace_anchor\",\"anchorRef\",\"content\"}}],\"evidence\":[{\"taskRef\",\"criterionRefs\",\"artifactKeys\",\"inputRefs\",\"explanation\"}],\"usedRefs\":[...]}}`. `content` for an anchor replaces exactly the bytes between its markers. Or a `failed` outcome (`contract_conflict` when an approved interface would have to change)."
        }
        ExecutorKind::TestAuthor | ExecutorKind::TestRepairer => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"<your specialty>\",\"payload\":{\"edits\":[...same edit shape as Code...],\"coverage\":[{\"taskRef\",\"criterionRef\",\"coverage\":\"authored|existing|not_applicable\",\"testArtifactKeys\",\"testInputRefs\",\"checkRefs\",\"explanation\"}],\"evidence\":[...],\"usedRefs\":[...]}}`. One `coverage` entry per criterion handle. Or a `failed` outcome (`implementation_mismatch` for a production defect)."
        }
        ExecutorKind::IntegrationReviewer => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"integration_reviewer\",\"payload\":{\"assessments\":[{\"taskRef\",\"criterionRef\",\"verdict\":\"supported|contradicted|unverified\",\"inputRefs\",\"checkRefs\",\"explanation\"}],\"blockers\":[...],\"documentationEdits\":[...],\"usedRefs\":[...]}}`. One assessment per criterion handle."
        }
    }
}

/// Renders the complete prompt for `unit`. Only handles the unit may cite are
/// listed: its inputs, its owned destinations and the plan's check handles.
pub fn render_unit_prompt(
    plan: &StageExecutionPlanV1,
    unit: &WorkUnitSpecV1,
    table: &ReferenceTable,
    policy: &ExecutorPolicy,
    inputs: &UnitPromptInputs<'_>,
) -> RenderedPrompt {
    let mut user = String::new();
    let _ = writeln!(user, "## Objective\n\n{}\n", unit.objective);

    user.push_str("## Tasks and criteria\n\n");
    for task in &unit.task_refs {
        let title = table.task(task).map(|t| t.title.as_str()).unwrap_or("");
        let _ = writeln!(user, "- `{task}` — {title}");
        for criterion in unit.criterion_refs.iter().filter(|c| {
            table
                .criterion(c)
                .is_some_and(|binding| &binding.task == task)
        }) {
            let description = table
                .criterion(criterion)
                .map(|c| c.description.as_str())
                .unwrap_or("");
            let _ = writeln!(user, "  - `{criterion}`: {description}");
            if let Some((_, requirement)) =
                inputs.requirements.iter().find(|(h, _)| *h == criterion)
            {
                let _ = writeln!(user, "    - requirement: {requirement}");
            }
        }
    }
    user.push('\n');

    if !inputs.decisions.is_empty() {
        user.push_str("## Decisions\n\n");
        for decision in inputs.decisions {
            let _ = writeln!(user, "- {decision}");
        }
        user.push('\n');
    }

    if !inputs.interfaces.is_empty() {
        user.push_str("## Interfaces\n\n");
        for group in inputs.interfaces {
            let intent = serde_json::to_value(group.intent)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let _ = writeln!(user, "### `{}` ({intent})\n", group.interface_key);
            let fence = fence_for(&group.signature);
            let _ = writeln!(user, "{fence}\n{}\n{fence}", group.signature);
            for invariant in &group.invariants {
                let _ = writeln!(user, "- invariant: {invariant}");
            }
            user.push('\n');
        }
    }

    user.push_str("## References\n\nYou may cite only the handles in this table.\n\n| ref | role | path | availability |\n|---|---|---|---|\n");
    let mut listed: Vec<&Handle> = unit.input_refs.iter().collect();
    for owned in &unit.owned_destination_refs {
        if !listed.contains(&owned) {
            listed.push(owned);
        }
    }
    for handle in listed {
        if let Some(entry) = table.entry(handle) {
            let path = entry
                .relative_path()
                .map(|p| format!("`{p}`"))
                .unwrap_or_else(|| "—".into());
            let owned = if unit.owned_destination_refs.contains(handle) {
                " (owned: you may edit it)"
            } else {
                ""
            };
            let _ = writeln!(
                user,
                "| `{handle}` | {} | {path} | {}{owned} |",
                role_name(entry),
                availability(entry)
            );
        }
    }
    if !unit.required_check_refs.is_empty() {
        let _ = writeln!(
            user,
            "\nHost checks that will run on your output: {}.",
            unit.required_check_refs
                .iter()
                .map(|h| format!("`{h}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    user.push('\n');

    let render_sources = |user: &mut String, title: &str, sources: &[DeliveredSource<'_>]| {
        if sources.is_empty() {
            return;
        }
        let _ = writeln!(user, "## {title}\n");
        for source in sources {
            let path = table
                .entry(source.handle)
                .and_then(ReferenceEntryV1::relative_path)
                .map(|p| p.as_str().to_owned())
                .unwrap_or_default();
            let anchors = plan
                .anchors
                .iter()
                .filter(|anchor| &anchor.source_ref == source.handle)
                .collect::<Vec<_>>();
            let _ = writeln!(user, "### `{}` — `{path}`", source.handle);
            if source.replaceable {
                user.push_str(
                    "Small file: `replace_file` with the complete content is permitted.\n",
                );
            }
            if !anchors.is_empty() {
                user.push_str("Editable intervals are marked `<<anchor_N>>` … `<</anchor_N>>`; a `replace_anchor` replaces exactly the bytes between the markers.\n");
            }
            let body = render_source_with_anchors(source.content, &anchors);
            let fence = fence_for(&body);
            let _ = writeln!(
                user,
                "{fence}\n{}{fence}\n",
                if body.ends_with('\n') {
                    body
                } else {
                    format!("{body}\n")
                }
            );
        }
    };
    render_sources(&mut user, "Delivered content", &inputs.delivered);
    render_sources(&mut user, "Dependency outputs", &inputs.dependency_outputs);

    let unknowns = unit
        .input_refs
        .iter()
        .filter_map(|handle| match table.entry(handle) {
            Some(ReferenceEntryV1::Unavailable { reason, hint, .. }) => {
                Some((handle, *reason, hint))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if !unknowns.is_empty() {
        user.push_str("## Unknowns\n\n");
        for (handle, reason, hint) in unknowns {
            let reason = serde_json::to_value(reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let _ = writeln!(user, "- `{handle}` — {reason}: {hint}");
        }
        user.push('\n');
    }

    if let Some(correction) = inputs.correction {
        user.push_str("## Correction\n\nYour unit's previous output was rejected. Fix only what is listed; do not re-author anything else.\n\n");
        for diagnostic in &correction.diagnostics {
            let _ = writeln!(user, "- {diagnostic}");
        }
        if !correction.rejected_artifact_keys.is_empty() {
            let _ = writeln!(
                user,
                "\nRejected artifact keys: {}.",
                correction
                    .rejected_artifact_keys
                    .iter()
                    .map(|h| format!("`{h}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        user.push('\n');
    }

    if let Some(extra) = inputs.extra_instructions {
        let _ = writeln!(user, "## Stage guidance\n\n{extra}\n");
    }

    let _ = writeln!(
        user,
        "## Response contract\n\n{}\n\nYour entire reply must be one complete JSON object. If it cannot be closed it is discarded whole; keep prose short and structure complete.",
        response_field_guide(policy.kind)
    );

    let system = system_prompt(policy);
    let chars = system.chars().count() + user.chars().count();
    RenderedPrompt {
        system,
        user,
        chars,
    }
}

#[cfg(test)]
mod tests {
    use super::super::policy::policy_for;
    use super::super::references::ReferenceTable;
    use super::super::*;
    use super::*;
    use crate::orchestration::SchemaV1;

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn uuid(n: u8) -> Uuid {
        Uuid::new(format!("10000000-0000-4000-8000-0000000000{n:02}")).unwrap()
    }

    fn setup() -> (StageExecutionPlanV1, ReferenceTable, Vec<u8>) {
        let bytes =
            b"use std::fmt;\n\npub fn value() -> u8 {\n    1\n}\n\npub fn other() {}\n".to_vec();
        let mut builder = ReferenceTable::builder();
        builder
            .add_task(
                "t-1",
                1,
                "Return two",
                &[("AC-1".into(), "`value()` returns 2.".into())],
            )
            .unwrap();
        builder
            .add_task(
                "t-2",
                1,
                "Backlog",
                &[("AC-1".into(), "Not this unit.".into())],
            )
            .unwrap();
        let src = builder
            .add_existing(
                RelPath::new("src/lib.rs").unwrap(),
                Sha256Hex::of(&bytes),
                SourceRole::Source,
            )
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/secret_config.rs").unwrap(),
                Sha256Hex::of(b"s"),
                SourceRole::Configuration,
            )
            .unwrap();
        builder
            .add_planned(
                RelPath::new("tests/value_test.rs").unwrap(),
                SourceRole::Test,
            )
            .unwrap();
        builder.add_unavailable(UnavailableReason::Ambiguous, "Two candidate callers.");
        builder.add_check(h("cargo_check")).unwrap();
        builder.add_line_anchor(&src, &bytes, 3, 5).unwrap();
        let table = builder.build();
        let unit = WorkUnitSpecV1 {
            schema_version: SchemaV1,
            unit_id: uuid(1),
            logical_unit_key: h("implement_value"),
            stage: Stage::Code,
            executor_kind: ExecutorKind::CodeImplementer,
            task_refs: vec![h("task_1")],
            criterion_refs: vec![h("crit_1_1")],
            dependencies: vec![],
            input_refs: vec![h("src_1"), h("unav_1")],
            owned_destination_refs: vec![h("src_1")],
            required_check_refs: vec![h("cargo_check")],
            objective: "Make `value()` return 2 without changing its signature.".into(),
            max_output_tokens: 800,
        };
        let plan = StageExecutionPlanV1 {
            schema_version: SchemaV1,
            stage_execution_id: uuid(2),
            budget_account_id: uuid(3),
            parent_fence: ParentFenceV1 {
                project_id: uuid(4),
                pulsation_id: uuid(5),
                input_revision: 1,
                job_id: uuid(6),
                generation: 1,
                state_version: 1,
            },
            stage: Stage::Code,
            recipe_version: 1,
            policy_sha256: Sha256Hex::of(b"policy"),
            batch_sha256: Sha256Hex::of(b"batch"),
            approved_contract_sha256: Some(Sha256Hex::of(b"contract")),
            plan_sha256: Sha256Hex::of(b"plan"),
            scope: table.scope().to_vec(),
            references: table.entries().to_vec(),
            anchors: table.anchors().to_vec(),
            check_refs: table.checks().to_vec(),
            units: vec![unit],
            profile_bindings: vec![],
            budget: StageBudgetV1 {
                initial_invocation_limit: 1,
                extra_invocation_limit: 2,
                per_unit_extra_invocation_limit: 1,
                initial_output_token_limit: 800,
                extra_output_token_limit: 800,
                initial_prompt_character_limit: 64_000,
                extra_prompt_character_limit: 64_000,
                max_in_flight_units: 1,
            },
        };
        (plan, table, bytes)
    }

    #[test]
    fn the_prompt_carries_exact_scoped_content_and_no_host_bookkeeping() {
        let (plan, table, bytes) = setup();
        let content = std::str::from_utf8(&bytes).unwrap();
        let src = h("src_1");
        let decisions = vec!["Preserve the public signature.".to_string()];
        let interfaces = vec![InterfaceGroupV1 {
            interface_key: h("public_api"),
            intent: GroupIntent::Preserve,
            source_refs: vec![h("src_1")],
            criterion_refs: vec![h("crit_1_1")],
            signature: "pub fn value() -> u8".into(),
            invariants: vec!["Never panics.".into()],
        }];
        let inputs = UnitPromptInputs {
            requirements: vec![(&plan.units[0].criterion_refs[0], "Return 2 from `value()`.")],
            decisions: &decisions,
            interfaces: &interfaces,
            delivered: vec![DeliveredSource {
                handle: &src,
                content,
                replaceable: true,
            }],
            dependency_outputs: vec![],
            correction: None,
            extra_instructions: None,
        };
        let prompt = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeImplementer),
            &inputs,
        );
        let text = format!("{}\n{}", prompt.system, prompt.user);
        // Exact approved text and exact source bytes are present.
        assert!(text.contains("`crit_1_1`: `value()` returns 2."));
        assert!(text.contains("requirement: Return 2 from `value()`."));
        assert!(text.contains("- Preserve the public signature."));
        assert!(text.contains("pub fn value() -> u8"));
        assert!(text.contains("<<anchor_1>>\npub fn value() -> u8 {\n    1\n}\n<</anchor_1>>\n"));
        assert!(text.contains("use std::fmt;"));
        assert!(text.contains("| `src_1` | source | `src/lib.rs` | existing; content delivered below (owned: you may edit it) |"));
        assert!(text.contains("| `unav_1` | — | — | unavailable (ambiguous) |"));
        assert!(text.contains("- `unav_1` — ambiguous: Two candidate callers."));
        assert!(text.contains("Host checks that will run on your output: `cargo_check`."));
        assert!(text.contains("## Response contract"));
        // Scoped: the other task, the non-input configuration file and the
        // planned test are absent; so is every host identifier.
        assert!(!text.contains("task_2"));
        assert!(!text.contains("Backlog"));
        assert!(!text.contains("secret_config"));
        assert!(!text.contains("plan_1"));
        for forbidden in [
            "10000000-0000-4000-8000",
            "publicationBases",
            "jobId",
            "stateVersion",
            "inputRevision",
            "statusUpdate",
            "relativePath",
            "$defs",
            "\"type\":\"object\"",
        ] {
            assert!(!text.contains(forbidden), "prompt leaks `{forbidden}`");
        }
        assert!(!text.contains(&Sha256Hex::of(&bytes).to_string()));
        assert!(!text.contains(&Sha256Hex::of(b"plan").to_string()));
        assert_eq!(
            prompt.chars,
            prompt.system.chars().count() + prompt.user.chars().count()
        );
        assert!(prompt.system.contains("ROLE:"));
        assert!(prompt.system.contains("Never invent handles"));
        assert!(prompt.system.contains("MUST be written in English"));
        // Determinism.
        let again = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeImplementer),
            &inputs,
        );
        assert_eq!(again, prompt);
    }

    #[test]
    fn corrections_carry_diagnostics_but_never_the_rejected_output() {
        let (plan, table, bytes) = setup();
        let content = std::str::from_utf8(&bytes).unwrap();
        let src = h("src_1");
        let correction = CorrectionContext {
            diagnostics: vec![
                "Evidence cites artifact key `lib_v2` that this payload did not produce.".into(),
            ],
            rejected_artifact_keys: vec![h("lib_v2")],
        };
        let inputs = UnitPromptInputs {
            delivered: vec![DeliveredSource {
                handle: &src,
                content,
                replaceable: false,
            }],
            correction: Some(&correction),
            ..Default::default()
        };
        let prompt = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeRepairer),
            &inputs,
        );
        assert!(prompt.user.contains("## Correction"));
        assert!(prompt.user.contains("Rejected artifact keys: `lib_v2`."));
        assert!(!prompt.user.contains("Small file"));
        assert!(prompt.system.contains("code_repairer"));
    }

    #[test]
    fn anchor_markers_never_alter_the_bytes_between_them() {
        let content = "a\nbb\nccc\n";
        let anchor = AnchorBindingV1 {
            anchor_ref: h("anchor_1"),
            source_ref: h("src_1"),
            source_sha256: Sha256Hex::of(content.as_bytes()),
            start_byte: 2,
            end_byte: 5,
            region_sha256: Sha256Hex::of(b"bb\n"),
        };
        let rendered = render_source_with_anchors(content, &[&anchor]);
        assert_eq!(rendered, "a\n<<anchor_1>>\nbb\n<</anchor_1>>\nccc\n");
        // Removing the marker lines restores the original exactly.
        let restored = rendered
            .lines()
            .filter(|line| !line.starts_with("<<"))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert_eq!(restored, content);
        // Content with triple backticks gets a longer fence.
        assert_eq!(fence_for("```"), "````");
        assert_eq!(fence_for("````"), "`````");
    }
}
