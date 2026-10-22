//! The model-facing prompt for one work unit (SC-007).
//!
//! A unit sees only what it needs to perform its bounded operation: its
//! objective, its tasks and criteria with the approved requirement text, the
//! decisions and exact interfaces that apply, the reference handles it may
//! cite, the exact delivered content with anchor markers, validated dependency
//! outputs, explicit unknowns and a short field guide for its response. It
//! never sees execution fences, publication bases, persistent identifiers,
//! hashes, other units' work or the full contract registry. When the stage is
//! split, it does see which paths its sibling units deliver (paths only), so
//! batch-wide context naming those files is not mistaken for missing scope.
//! The JSON Schema is attached by the transport layer, once, only where the
//! provider needs text.

use std::fmt::Write as _;

use super::policy::{ExecutorPolicy, common_rules};
use super::references::ReferenceTable;
use super::schema::SchemaFlavor;
use super::validate::exclusive_criteria;
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
    pub redaction: Option<&'a crate::secret_redactor::ContextRedaction>,
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
    /// This unit's criteria the approved specification marked `not_applicable`.
    pub not_applicable: Vec<&'a Handle>,
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
    /// Byte ranges originating in host-authorized synthetic fixture exceptions.
    pub authorized_user_ranges: Vec<std::ops::Range<usize>>,
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
    let Ok(view) = crate::secret_redactor::redact_context(content) else {
        return "[content unavailable after sanitization]".to_owned();
    };
    render_redacted_source(content, anchors, &view).0
}

fn render_redacted_source(
    content: &str,
    anchors: &[&AnchorBindingV1],
    view: &crate::secret_redactor::ContextRedaction,
) -> (String, Vec<std::ops::Range<usize>>) {
    let mut sorted = anchors.to_vec();
    sorted.sort_by_key(|anchor| (anchor.start_byte, anchor.end_byte));
    let mut out = String::with_capacity(content.len() + 32 * sorted.len());
    let mut cursor = 0usize;
    let mut authorized = Vec::new();
    let mut append = |out: &mut String, start: usize, end: usize| -> Result<(), String> {
        let prefix = crate::secret_redactor::render_redacted_range(content, &view.spans, 0, start)?;
        let segment =
            crate::secret_redactor::render_redacted_range(content, &view.spans, start, end)?;
        let base = out.len();
        let view_start = prefix.len();
        let view_end = view_start + segment.len();
        for span in &view.authorized_spans {
            let start = span.start_byte.max(view_start);
            let end = span.end_byte.min(view_end);
            if start < end {
                authorized.push(base + start - view_start..base + end - view_start);
            }
        }
        out.push_str(&segment);
        Ok(())
    };
    for anchor in sorted {
        let (start, end) = (anchor.start_byte as usize, anchor.end_byte as usize);
        if start < cursor
            || end > content.len()
            || !content.is_char_boundary(start)
            || !content.is_char_boundary(end)
            || view
                .spans
                .iter()
                .any(|span| start < span.end_byte && span.start_byte < end)
        {
            continue;
        }
        if append(&mut out, cursor, start).is_err() {
            return (
                "[content unavailable after sanitization]".into(),
                Vec::new(),
            );
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        let _ = writeln!(out, "<<{}>>", anchor.anchor_ref);
        if append(&mut out, start, end).is_err() {
            return (
                "[content unavailable after sanitization]".into(),
                Vec::new(),
            );
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        let _ = writeln!(out, "<</{}>>", anchor.anchor_ref);
        cursor = end;
    }
    if append(&mut out, cursor, content.len()).is_err() {
        return (
            "[content unavailable after sanitization]".into(),
            Vec::new(),
        );
    }
    (out, authorized)
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
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"specification_author\",\"payload\":{\"objective\",\"exclusions\":[...],\"decisions\":[...],\"requirements\":[{\"taskRef\",\"criterionRef\",\"requirement\",\"verificationIntent\",\"verificationMode\":\"executed_check|document_check|not_applicable\",\"requiredCheckRefs\":[...],\"notApplicableReason\":null|\"...\"}],\"existingFileRefs\":[...],\"plannedFileRefs\":[...],\"interfaceIntent\":[{\"intent\":\"preserve|change|new|none\",\"sourceRefs\",\"criterionRefs\",\"description\"}],\"unitProposals\":[{\"unitKey\",\"kind\":\"code|test\",\"taskRefs\",\"criterionRefs\",\"inputRefs\",\"destinationRefs\",\"dependsOn\",\"objective\"}]}}`. Exactly one requirement per criterion handle; `requiredCheckRefs` may name only handles listed under \"Host checks\". When a human decision is missing: `{\"schemaVersion\":1,\"outcome\":\"needs_input\",\"missingRefs\":[...],\"questions\":[{\"key\",\"taskRefs\",\"question\",\"rationale\"}]}`."
        }
        ExecutorKind::ContractAuthor => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"contract_author\",\"payload\":{\"groups\":[{\"interfaceKey\",\"intent\":\"preserve|change|new\",\"sourceRefs\",\"criterionRefs\",\"signature\",\"invariants\":[...]}],\"unitBindings\":[{\"unitKey\",\"interfaceKeys\",\"dependsOn\"}],\"usedRefs\":[...]}}` or a `failed` outcome with `code` in contract_conflict|scope_insufficient|implementation_mismatch|unsupported_operation."
        }
        ExecutorKind::CodeImplementer | ExecutorKind::CodeRepairer => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"<your specialty>\",\"payload\":{\"edits\":[{\"artifactKey\",\"edit\":{\"operation\":\"create_file\",\"destinationRef\",\"content\"}|{\"operation\":\"replace_file\",\"sourceRef\",\"content\"}|{\"operation\":\"replace_anchor\",\"anchorRef\",\"content\"}}],\"evidence\":[{\"taskRef\",\"criterionRefs\",\"artifactKeys\",\"inputRefs\",\"explanation\"}],\"usedRefs\":[...]}}`. `content` for an anchor replaces exactly the bytes between its markers. Or a `failed` outcome (`contract_conflict` when an approved interface would have to change)."
        }
        ExecutorKind::TestAuthor | ExecutorKind::TestRepairer => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"<your specialty>\",\"payload\":{\"edits\":[...same edit shape as Code...],\"coverage\":[{\"taskRef\",\"criterionRef\",\"coverage\":\"authored|existing|not_applicable\",\"testArtifactKeys\",\"testInputRefs\",\"checkRefs\",\"explanation\"}],\"evidence\":[...],\"usedRefs\":[...]}}`. One `coverage` entry per criterion handle; `checkRefs` may name only handles listed under \"Host checks\". A criterion approved as not_applicable takes `coverage: not_applicable` and needs no evidence item. Or a `failed` outcome (`implementation_mismatch` for a production defect)."
        }
        ExecutorKind::IntegrationReviewer => {
            "Return `{\"schemaVersion\":1,\"outcome\":\"completed\",\"executorKind\":\"integration_reviewer\",\"payload\":{\"assessments\":[{\"taskRef\",\"criterionRef\",\"verdict\":\"supported|contradicted|unverified\",\"inputRefs\",\"checkRefs\",\"explanation\"}],\"blockers\":[...],\"documentationEdits\":[...],\"usedRefs\":[...]}}`. One assessment per criterion handle; `checkRefs` may name only handles listed under \"Host checks\", never a file handle."
        }
    }
}

/// Exact failure envelope for the transport's selected schema lowering. The
/// parser remains deliberately fail-closed; this is model guidance, never a
/// normalizer for invalid fields. The example names no concrete code: a fixed
/// `scope_insufficient` example primed workers to return exactly that.
pub fn failure_envelope_guide(flavor: SchemaFlavor) -> &'static str {
    match flavor {
        SchemaFlavor::Canonical => {
            r#"For any `failed` outcome, return exactly `{"schemaVersion":1,"outcome":"failed","code":"<code>","message":"short reason","relatedRefs":[...]}`, where `<code>` is exactly one of `contract_conflict`, `scope_insufficient`, `implementation_mismatch` or `unsupported_operation` and names the actual blocker. Do not include `executorKind`, `payload`, `missingRefs`, or `questions`."#
        }
        // The StructuredOutput tool's only argument is already `response`.
        // Showing the envelope wrapped in `{"response":…}` made Haiku nest a
        // second one; Claude Code rejected each call and the model rewrote
        // 20 KB of tests four times ("Haiku CLI teste 1", 2026-09-26).
        SchemaFlavor::Claude => {
            r#"For any `failed` outcome, return exactly `{"schemaVersion":1,"outcome":"failed","code":"<code>","message":"short reason","relatedRefs":[...]}`, where `<code>` is exactly one of `contract_conflict`, `scope_insufficient`, `implementation_mismatch` or `unsupported_operation` and names the actual blocker. Do not include `executorKind`, `payload`, `missingRefs`, or `questions`.

Deliver the response only by calling the StructuredOutput tool with its `response` argument set to that JSON object. Never wrap it in a second `response`, and keep file contents properly escaped JSON strings."#
        }
        // `agy`'s structured answer is its `finish` tool, same `response` wrapper.
        SchemaFlavor::Antigravity => {
            r#"For any `failed` outcome, return exactly `{"schemaVersion":1,"outcome":"failed","code":"<code>","message":"short reason","relatedRefs":[...]}`, where `<code>` is exactly one of `contract_conflict`, `scope_insufficient`, `implementation_mismatch` or `unsupported_operation` and names the actual blocker. Do not include `executorKind`, `payload`, `missingRefs`, or `questions`.

Deliver the response only by calling the finish tool with its `response` argument set to that JSON object. Put no other key next to the fields of that object, never wrap it in a second `response`, and keep file contents properly escaped JSON strings."#
        }
        SchemaFlavor::Strict => {
            r#"For any `failed` outcome, return exactly `{"schemaVersion":1,"outcome":"failed","executorKind":null,"payload":null,"missingRefs":null,"questions":null,"code":"<code>","message":"short reason","relatedRefs":[...]}`, where `<code>` is exactly one of `contract_conflict`, `scope_insufficient`, `implementation_mismatch` or `unsupported_operation` and names the actual blocker. Do not put a non-null value in any inactive field."#
        }
    }
}

fn path_list(table: &ReferenceTable, handles: &[Handle]) -> Vec<String> {
    handles
        .iter()
        .filter_map(|handle| table.entry(handle)?.relative_path())
        .map(|path| format!("`{path}`"))
        .collect()
}

fn handle_list(handles: &[&Handle]) -> String {
    handles
        .iter()
        .map(|handle| format!("`{handle}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Boundary between this unit's obligation and the batch-wide context, only
/// for a split stage. Task titles, decisions, interfaces and the stage
/// guidance describe the whole batch; without this section a worker owning
/// one file read the files of later siblings as missing destinations and
/// failed with `scope_insufficient` (ToDoListWebView, 2026-09-22).
fn render_unit_scope(
    user: &mut String,
    plan: &StageExecutionPlanV1,
    unit: &WorkUnitSpecV1,
    table: &ReferenceTable,
) {
    if plan.units.len() < 2 {
        return;
    }
    let owned = unit
        .owned_destination_refs
        .iter()
        .filter_map(|handle| {
            let path = table.entry(handle)?.relative_path()?;
            Some(format!("`{path}` (`{handle}`)"))
        })
        .collect::<Vec<_>>();
    let _ = writeln!(
        user,
        "## Scope of this unit\n\nThis stage is split into {} units that run separately. This unit delivers only: {}.",
        plan.units.len(),
        if owned.is_empty() {
            "no file (it only reads its inputs)".to_owned()
        } else {
            owned.join(", ")
        }
    );
    let siblings = plan
        .units
        .iter()
        .filter(|other| other.unit_id != unit.unit_id)
        .map(|other| {
            (
                other.logical_unit_key.as_str(),
                path_list(table, &other.owned_destination_refs),
            )
        })
        .filter(|(_, paths)| !paths.is_empty())
        .collect::<Vec<_>>();
    if !siblings.is_empty() {
        user.push_str("The other units of this stage deliver these files, so they are intentionally absent from your References table:\n");
        for (key, paths) in siblings {
            let _ = writeln!(user, "- unit {key}: {}", paths.join(", "));
        }
    }
    user.push_str("Task titles, decisions, interfaces, clarifications and the stage guidance describe the whole batch and are shared by every unit. A file they mention that another unit delivers is not missing scope: do not create, edit or request it, and do not fail with `scope_insufficient` because of it. Use `scope_insufficient` only when this unit's own objective and criteria cannot be met with the files it owns and the readable inputs listed.\n\n");
}

/// Renders the complete prompt for `unit`. Only handles the unit may cite are
/// listed: its inputs, its owned destinations and the plan's check handles.
pub fn render_unit_prompt(
    plan: &StageExecutionPlanV1,
    unit: &WorkUnitSpecV1,
    table: &ReferenceTable,
    policy: &ExecutorPolicy,
    inputs: &UnitPromptInputs<'_>,
    flavor: SchemaFlavor,
) -> RenderedPrompt {
    let mut user = String::new();
    let mut authorized_user_ranges = Vec::new();
    let tests = matches!(
        policy.kind,
        ExecutorKind::TestAuthor | ExecutorKind::TestRepairer
    );
    let _ = writeln!(user, "## Objective\n\n{}\n", unit.objective);
    render_unit_scope(&mut user, plan, unit, table);

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
            if tests && inputs.not_applicable.contains(&criterion) {
                user.push_str("    - approved as not_applicable: cover it `not_applicable`; it needs no evidence item.\n");
            }
        }
    }
    let exclusive = exclusive_criteria(plan, unit);
    let shared = unit
        .criterion_refs
        .iter()
        .filter(|criterion| !exclusive.contains(criterion))
        .collect::<Vec<_>>();
    // Test settles an approved `not_applicable` criterion by its coverage entry.
    let required = exclusive
        .into_iter()
        .filter(|criterion| !(tests && inputs.not_applicable.contains(criterion)))
        .collect::<Vec<_>>();
    if policy.requires_evidence && !shared.is_empty() {
        let _ = writeln!(
            user,
            "\nEvidence required from this unit: {}. Shared with other units of this stage (evidence them only where your own edits contribute): {}.",
            if required.is_empty() {
                "none of its own, but cite at least one shared criterion your edits contribute to"
                    .to_owned()
            } else {
                handle_list(&required)
            },
            handle_list(&shared)
        );
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

    user.push_str("## References\n\nYou may cite only the handles in this table. For `usedRefs`, list ONLY handles whose availability column shows \"readable\" or \"delivered\". If the table is empty or has no readable rows, set `usedRefs` to `[]`.\n\n| ref | role | path | availability |\n|---|---|---|---|\n");
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
    } else if plan.check_refs.is_empty()
        && matches!(
            policy.kind,
            ExecutorKind::SpecificationAuthor
                | ExecutorKind::TestAuthor
                | ExecutorKind::TestRepairer
                | ExecutorKind::IntegrationReviewer
        )
    {
        // Without this line a model filled `checkRefs` with the test file
        // it relied on ("Teste Haiku Only 1", 2026-09-24).
        user.push_str("\nNo host check is configured for this stage: every `checkRefs` / `requiredCheckRefs` must be `[]`.\n");
    }
    user.push('\n');

    let mut render_sources = |user: &mut String, title: &str, sources: &[DeliveredSource<'_>]| {
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
            let fallback;
            let view = match source.redaction {
                Some(view) => view,
                None => {
                    fallback = crate::secret_redactor::redact_context(source.content).ok();
                    let Some(view) = fallback.as_ref() else {
                        user.push_str("[content unavailable after sanitization]\n");
                        continue;
                    };
                    view
                }
            };
            if source.replaceable && view.spans.is_empty() {
                user.push_str(
                    "Small file: `replace_file` with the complete content is permitted.\n",
                );
            }
            if !anchors.is_empty() {
                user.push_str("Editable intervals are marked `<<anchor_N>>` … `<</anchor_N>>`; a `replace_anchor` replaces exactly the bytes between the markers.\n");
            }
            if !view.spans.is_empty() {
                user.push_str("Redacted regions are protected: their anchors are withheld and whole-file replacement is unavailable. Edit only disjoint marked intervals; never copy redaction markers into generated files.\n");
            }
            let (body, ranges) = render_redacted_source(source.content, &anchors, view);
            let fence = fence_for(&body);
            let body_start = user.len() + fence.len() + 1;
            authorized_user_ranges.extend(
                ranges
                    .into_iter()
                    .map(|range| body_start + range.start..body_start + range.end),
            );
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
        // The rejected answer is discarded whole and never shown: asking for
        // "only the fix" produced a partial answer the same validation
        // rejected again ("Teste Haiku Only 1", 2026-09-24).
        user.push_str("## Correction\n\nYour previous response was rejected for the reasons below. It is not shown to you and nothing from it is kept: return the COMPLETE response again, not only the corrected part.\n");
        // Code and Test deliver files; the Integrator's documentation edits
        // stay optional.
        if policy.requires_evidence && !unit.owned_destination_refs.is_empty() {
            let _ = writeln!(
                user,
                "- Deliver the complete edits for: {}.",
                path_list(table, &unit.owned_destination_refs).join(", ")
            );
        }
        if policy.requires_evidence && !required.is_empty() {
            let _ = writeln!(
                user,
                "- Evidence every criterion this unit must evidence: {}.",
                handle_list(&required)
            );
        }
        user.push_str("\nRejection reasons:\n");
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
        if plan.units.len() > 1 {
            let _ = writeln!(
                user,
                "## Stage guidance (whole batch, shared by every unit)\n\n{extra}\n"
            );
        } else {
            let _ = writeln!(user, "## Stage guidance\n\n{extra}\n");
        }
    }

    let _ = writeln!(
        user,
        "## Response contract\n\n{}\n\n{}\n\nYour entire reply must be one complete JSON object. If it cannot be closed it is discarded whole; keep prose short and structure complete.",
        response_field_guide(policy.kind),
        failure_envelope_guide(flavor)
    );

    let system = system_prompt(policy);
    let chars = system.chars().count() + user.chars().count();
    RenderedPrompt {
        system,
        user,
        chars,
        authorized_user_ranges,
    }
}

#[cfg(test)]
mod tests {
    use super::super::policy::policy_for;
    use super::super::references::ReferenceTable;
    use super::super::*;
    use super::*;
    use crate::orchestration::SchemaV1;

    #[test]
    fn projection_redacts_original_spans_before_adding_anchor_markers() {
        let original = "fn café() {}\r\nconst KEY: &str = \"sk-proj-abcdefghijklmnopqrst\";\r\n";
        let mut builder = ReferenceTable::builder();
        let source = builder
            .add_existing(
                RelPath::new("src/lib.rs").unwrap(),
                Sha256Hex::of(original.as_bytes()),
                SourceRole::Source,
            )
            .unwrap();
        builder
            .add_line_anchor(&source, original.as_bytes(), 1, 1)
            .unwrap();
        builder
            .add_line_anchor(&source, original.as_bytes(), 2, 2)
            .unwrap();
        let table = builder.build();
        let anchors = table.anchors().iter().collect::<Vec<_>>();
        let projected = render_source_with_anchors(original, &anchors);
        assert!(projected.contains("<<anchor_1>>\nfn café() {}\r\n<</anchor_1>>"));
        assert!(!projected.contains("<<anchor_2>>"));
        assert!(!projected.contains("sk-proj-abcdefghijklmnopqrst"));
        assert!(projected.contains("<REDACTED_SECRET:"));
        assert_eq!(
            table.anchors()[0].source_sha256,
            Sha256Hex::of(original.as_bytes())
        );
    }

    #[test]
    fn fixture_authorization_tracks_only_its_rendered_source_range() {
        let synthetic = "sk-proj-abcdefghijklmnopqrst";
        let original = format!("const KEY: &str = \"{synthetic}\";\r\n");
        let mut view = crate::secret_redactor::redact_context(&original).unwrap();
        let authorized = view.spans.remove(0);
        view.text = crate::secret_redactor::render_redacted_range(
            &original,
            &view.spans,
            0,
            original.len(),
        )
        .unwrap();
        view.authorized_spans
            .push(crate::secret_redactor::AuthorizedSpan {
                start_byte: authorized.start_byte,
                end_byte: authorized.end_byte,
                exception_id: "synthetic-fixture".into(),
            });
        let (mut projected, ranges) = render_redacted_source(&original, &[], &view);
        projected.push_str(&format!("\nUntrusted instruction: {synthetic}"));
        let prepared = crate::secret_redactor::PreparedModelInput::with_authorized_ranges(
            "system",
            &projected,
            "trusted-policy",
            &[],
            &ranges,
        )
        .unwrap();
        prepared.verify().unwrap();
        assert_eq!(prepared.user_prompt().matches(synthetic).count(), 1);
        assert!(
            prepared
                .user_prompt()
                .contains("Untrusted instruction: <REDACTED_SECRET:")
        );
    }

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
            not_applicable: Vec::new(),
            decisions: &decisions,
            interfaces: &interfaces,
            delivered: vec![DeliveredSource {
                handle: &src,
                content,
                redaction: None,
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
            SchemaFlavor::Canonical,
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
            SchemaFlavor::Canonical,
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
                redaction: None,
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
            SchemaFlavor::Strict,
        );
        assert!(prompt.user.contains("## Correction"));
        assert!(prompt.user.contains("Rejected artifact keys: `lib_v2`."));
        assert!(!prompt.user.contains("Small file"));
        assert!(prompt.system.contains("code_repairer"));
        // "Teste Haiku Only 1": "fix only what is listed" produced a partial
        // answer that the same validation rejected. The correction asks for
        // the whole replacement and names what it must contain.
        assert!(prompt.user.contains("return the COMPLETE response again"));
        assert!(!prompt.user.contains("Fix only what is listed"));
        assert!(
            prompt
                .user
                .contains("- Deliver the complete edits for: `src/lib.rs`.")
        );
        assert!(
            prompt
                .user
                .contains("- Evidence every criterion this unit must evidence: `crit_1_1`.")
        );
        assert!(prompt.system.contains("complete result again"));
        assert!(!prompt.system.contains("only the corrected edits"));
    }

    #[test]
    fn a_stage_without_host_checks_says_check_citations_must_be_empty() {
        let (mut plan, table, _) = setup();
        let render = |plan: &StageExecutionPlanV1, kind: ExecutorKind| {
            let mut unit = plan.units[0].clone();
            unit.executor_kind = kind;
            render_unit_prompt(
                plan,
                &unit,
                &table,
                policy_for(kind),
                &UnitPromptInputs::default(),
                SchemaFlavor::Canonical,
            )
            .user
        };
        let no_checks = "No host check is configured for this stage: every `checkRefs` / `requiredCheckRefs` must be `[]`.";
        // With a configured check the prompt names it, and says nothing else.
        let with = render(&plan, ExecutorKind::IntegrationReviewer);
        assert!(with.contains("Host checks that will run on your output: `cargo_check`."));
        assert!(!with.contains(no_checks));
        plan.check_refs.clear();
        plan.units[0].required_check_refs.clear();
        for kind in [
            ExecutorKind::SpecificationAuthor,
            ExecutorKind::TestAuthor,
            ExecutorKind::IntegrationReviewer,
        ] {
            let text = render(&plan, kind);
            assert!(text.contains(no_checks), "{kind:?}");
            assert!(
                text.contains("may name only handles listed under \"Host checks\""),
                "{kind:?}"
            );
        }
        // Code has no check field to fill.
        assert!(!render(&plan, ExecutorKind::CodeImplementer).contains(no_checks));
    }

    #[test]
    fn a_split_stage_names_required_and_shared_criteria() {
        let (mut plan, table, _) = setup();
        plan.units[0].task_refs = vec![h("task_1"), h("task_2")];
        plan.units[0].criterion_refs = vec![h("crit_1_1"), h("crit_2_1")];
        let mut sibling = plan.units[0].clone();
        sibling.unit_id = uuid(9);
        sibling.logical_unit_key = h("sibling");
        sibling.task_refs = vec![h("task_2")];
        sibling.criterion_refs = vec![h("crit_2_1")];
        sibling.owned_destination_refs = vec![];
        plan.units.push(sibling);
        let correction = CorrectionContext {
            diagnostics: vec!["No evidence covers criterion `crit_1_1`.".into()],
            rejected_artifact_keys: vec![],
        };
        let inputs = UnitPromptInputs {
            correction: Some(&correction),
            ..Default::default()
        };
        let prompt = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeImplementer),
            &inputs,
            SchemaFlavor::Canonical,
        );
        assert!(prompt.user.contains("Evidence required from this unit: `crit_1_1`. Shared with other units of this stage (evidence them only where your own edits contribute): `crit_2_1`."), "{}", prompt.user);
        assert!(
            prompt
                .user
                .contains("- Evidence every criterion this unit must evidence: `crit_1_1`.")
        );
        // A single-unit stage has nothing shared to explain.
        let (plan, table, _) = setup();
        let single = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeImplementer),
            &UnitPromptInputs::default(),
            SchemaFlavor::Canonical,
        );
        assert!(!single.user.contains("Evidence required from this unit"));
    }

    /// Test is told which criteria the Specification exempted and is not
    /// asked to evidence them; Code keeps owing them ("Haiku CLI teste 1").
    #[test]
    fn test_prompts_mark_approved_not_applicable_criteria_as_needing_no_evidence() {
        let (mut plan, table, _) = setup();
        plan.units[0].task_refs = vec![h("task_1"), h("task_2")];
        plan.units[0].criterion_refs = vec![h("crit_1_1"), h("crit_2_1")];
        let correction = CorrectionContext {
            diagnostics: vec!["No evidence covers criterion `crit_1_1`.".into()],
            rejected_artifact_keys: vec![],
        };
        let exempt = h("crit_2_1");
        let inputs = UnitPromptInputs {
            not_applicable: vec![&exempt],
            correction: Some(&correction),
            ..Default::default()
        };
        let render = |kind| {
            render_unit_prompt(
                &plan,
                &plan.units[0],
                &table,
                policy_for(kind),
                &inputs,
                SchemaFlavor::Canonical,
            )
            .user
        };
        let line =
            "approved as not_applicable: cover it `not_applicable`; it needs no evidence item.";
        let test = render(ExecutorKind::TestAuthor);
        assert!(test.contains(line), "{test}");
        assert!(test.contains("- Evidence every criterion this unit must evidence: `crit_1_1`."));
        assert!(test.contains("needs no evidence item. Or a `failed` outcome"));
        let code = render(ExecutorKind::CodeImplementer);
        assert!(!code.contains(line));
        assert!(code.contains(
            "- Evidence every criterion this unit must evidence: `crit_1_1`, `crit_2_1`."
        ));
    }

    #[test]
    fn failure_guidance_matches_the_selected_provider_contract() {
        let canonical = failure_envelope_guide(SchemaFlavor::Canonical);
        assert!(canonical.contains("Do not include `executorKind`"));
        assert!(!canonical.contains("\"executorKind\":null"));
        // Claude answers through a tool whose only argument is `response`:
        // the example is unwrapped and a second wrapper is forbidden.
        let claude = failure_envelope_guide(SchemaFlavor::Claude);
        assert!(claude.contains("`{\"schemaVersion\":1,\"outcome\":\"failed\""));
        assert!(!claude.contains("{\"response\":"));
        assert!(claude.contains("StructuredOutput tool with its `response` argument"));
        assert!(claude.contains("Never wrap it in a second `response`"));
        assert!(!canonical.contains("StructuredOutput"));
        assert!(claude.contains("Do not include `executorKind`"));
        assert!(!claude.contains("\"executorKind\":null"));
        let antigravity = failure_envelope_guide(SchemaFlavor::Antigravity);
        assert!(antigravity.contains("finish tool with its `response` argument"));
        assert!(antigravity.contains("never wrap it in a second `response`"));
        assert!(!antigravity.contains("StructuredOutput"));
        assert!(!antigravity.contains("{\"response\":"));
        let strict = failure_envelope_guide(SchemaFlavor::Strict);
        assert!(strict.contains("\"executorKind\":null"));
        assert!(strict.contains("\"payload\":null"));
        // No concrete code is modelled for the worker to copy.
        for guide in [canonical, claude, antigravity, strict] {
            assert!(guide.contains("\"code\":\"<code>\""));
            assert!(!guide.contains("\"code\":\"scope_insufficient\""));
        }
    }

    #[test]
    fn a_split_stage_names_sibling_destinations_as_context_not_missing_scope() {
        let (mut plan, table, bytes) = setup();
        let content = std::str::from_utf8(&bytes).unwrap();
        let mut sibling = plan.units[0].clone();
        sibling.unit_id = uuid(7);
        sibling.logical_unit_key = h("author_value_test");
        sibling.input_refs = vec![h("plan_1")];
        sibling.owned_destination_refs = vec![h("plan_1")];
        plan.units.push(sibling);
        let src = h("src_1");
        let guidance = "task_1: Return two (src/lib.rs, tests/value_test.rs)";
        let inputs = UnitPromptInputs {
            delivered: vec![DeliveredSource {
                handle: &src,
                content,
                redaction: None,
                replaceable: false,
            }],
            extra_instructions: Some(guidance),
            ..Default::default()
        };
        let prompt = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeImplementer),
            &inputs,
            SchemaFlavor::Strict,
        );
        let user = &prompt.user;
        assert!(user.contains("## Scope of this unit"));
        assert!(user.contains("split into 2 units"));
        assert!(user.contains("This unit delivers only: `src/lib.rs` (`src_1`)."));
        assert!(user.contains("- unit author_value_test: `tests/value_test.rs`"));
        assert!(user.contains("do not fail with `scope_insufficient` because of it"));
        assert!(user.contains("## Stage guidance (whole batch, shared by every unit)"));
        // The boundary comes before any batch-wide context, and the sibling's
        // handle never reaches this unit's References table.
        assert!(user.find("## Scope of this unit") < user.find("## Tasks and criteria"));
        assert!(!user.contains("`plan_1`"));
        // A single-unit stage keeps the original prompt shape.
        plan.units.truncate(1);
        let single = render_unit_prompt(
            &plan,
            &plan.units[0],
            &table,
            policy_for(ExecutorKind::CodeImplementer),
            &inputs,
            SchemaFlavor::Strict,
        );
        assert!(!single.user.contains("## Scope of this unit"));
        assert!(single.user.contains("## Stage guidance\n"));
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
