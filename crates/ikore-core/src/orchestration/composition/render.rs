//! Host-rendered documents (frontmatter, notes and reports).
//!
//! Workers return typed payloads; the developer reviews readable Markdown. The
//! host renders that Markdown deterministically from validated data, marks it
//! host-generated, and never lets a model write frontmatter, coverage states
//! or execution results. Rendering is pure: same inputs, same bytes.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::references::ReferenceTable;
use super::{
    CheckSummaryV1, CoverageKind, CriterionAssessmentV1, Handle, IntakePayloadV1,
    InterfaceContractV1, RelPath, SpecificationContractV1, TestCoverageV1, UnitProposalV1,
    VerificationMode,
};

pub const REFERENCE_CONTRACT: &str = "stage-composition-v1";
pub const HOST_GENERATED_MARKER: &str = "<!-- host-generated: stage-composition-v1 -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Complete,
    Partial,
    Unavailable,
}

impl CoverageState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "complete" => Some(Self::Complete),
            "partial" => Some(Self::Partial),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// The v4 generated-note frontmatter. Physical arrays list only files that
/// exist in the fenced baseline; planned arrays list approved absent
/// destinations. `coverage_state` describes reference delivery, never
/// correctness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    pub source_files: Vec<RelPath>,
    pub test_files: Vec<RelPath>,
    pub planned_source_files: Vec<RelPath>,
    pub planned_test_files: Vec<RelPath>,
    pub coverage_state: CoverageState,
}

impl Frontmatter {
    /// Derives the four arrays from the reference handles a document cites,
    /// classifying by entry kind and role. Unknown or unavailable handles make
    /// coverage `partial`.
    pub fn from_references(table: &ReferenceTable, cited: &[Handle]) -> Self {
        use super::{ReferenceEntryV1, SourceRole};
        let mut frontmatter = Self {
            source_files: Vec::new(),
            test_files: Vec::new(),
            planned_source_files: Vec::new(),
            planned_test_files: Vec::new(),
            coverage_state: CoverageState::Complete,
        };
        for handle in cited {
            match table.entry(handle) {
                Some(ReferenceEntryV1::ExistingSource {
                    relative_path,
                    source_role,
                    ..
                })
                | Some(ReferenceEntryV1::CandidateSource {
                    relative_path,
                    source_role,
                    ..
                }) => {
                    if *source_role == SourceRole::Test {
                        frontmatter.test_files.push(relative_path.clone());
                    } else {
                        frontmatter.source_files.push(relative_path.clone());
                    }
                }
                Some(ReferenceEntryV1::PlannedDestination {
                    relative_path,
                    source_role,
                    ..
                }) => {
                    if *source_role == SourceRole::Test {
                        frontmatter.planned_test_files.push(relative_path.clone());
                    } else {
                        frontmatter.planned_source_files.push(relative_path.clone());
                    }
                }
                Some(ReferenceEntryV1::Unavailable { .. }) | None => {
                    frontmatter.coverage_state = CoverageState::Partial;
                }
            }
        }
        for list in [
            &mut frontmatter.source_files,
            &mut frontmatter.test_files,
            &mut frontmatter.planned_source_files,
            &mut frontmatter.planned_test_files,
        ] {
            list.sort();
            list.dedup();
        }
        frontmatter
    }

    pub fn render(&self) -> String {
        fn list(out: &mut String, key: &str, items: &[RelPath]) {
            if items.is_empty() {
                let _ = writeln!(out, "{key}: []");
            } else {
                let _ = writeln!(out, "{key}:");
                for item in items {
                    let _ = writeln!(out, "  - {}", yaml_string(item.as_str()));
                }
            }
        }
        let mut out = String::from("---\n");
        let _ = writeln!(out, "reference-contract: {REFERENCE_CONTRACT}");
        list(&mut out, "source-files", &self.source_files);
        list(&mut out, "test-files", &self.test_files);
        list(&mut out, "planned-source-files", &self.planned_source_files);
        list(&mut out, "planned-test-files", &self.planned_test_files);
        let _ = writeln!(out, "coverage-state: {}", self.coverage_state.as_str());
        out.push_str("---\n");
        out
    }
}

/// Double-quoted YAML scalar with the escapes a path or title can need.
fn yaml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn task_label(table: &ReferenceTable, handle: &Handle) -> String {
    match table.task(handle) {
        Some(task) => format!("`{handle}` — {}", task.title),
        None => format!("`{handle}`"),
    }
}

fn criterion_label(table: &ReferenceTable, handle: &Handle) -> String {
    match table.criterion(handle) {
        Some(criterion) => format!(
            "`{handle}` ({}) — {}",
            criterion.criterion_id, criterion.description
        ),
        None => format!("`{handle}`"),
    }
}

fn path_label(table: &ReferenceTable, handle: &Handle) -> String {
    match table.entry(handle).and_then(|entry| entry.relative_path()) {
        Some(path) => format!("`{handle}` → `{path}`"),
        None => format!("`{handle}`"),
    }
}

fn heading(out: &mut String, title: &str) {
    let _ = writeln!(out, "{HOST_GENERATED_MARKER}\n# {title}\n");
}

pub fn render_intake_note(
    title: &str,
    payload: &IntakePayloadV1,
    table: &ReferenceTable,
) -> String {
    let mut out = String::new();
    heading(&mut out, &format!("Intake — {title}"));
    out.push_str("## Tasks\n\n| Task | Classification | Risk | Summary |\n|---|---|---|---|\n");
    for task in &payload.tasks {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            task_label(table, &task.task_ref),
            enum_name(&task.classification),
            enum_name(&task.risk),
            cell(&task.summary)
        );
    }
    if !payload.issues.is_empty() {
        out.push_str("\n## Issues for Specification\n\n");
        for issue in &payload.issues {
            let _ = writeln!(out, "- {issue}");
        }
    }
    references_section(&mut out, table, &payload.used_refs);
    out
}

pub fn render_specification_note(
    title: &str,
    payload: &SpecificationContractV1,
    table: &ReferenceTable,
    frontmatter: &Frontmatter,
) -> String {
    let mut out = frontmatter.render();
    heading(&mut out, &format!("Specification — {title}"));
    let _ = writeln!(out, "## Objective\n\n{}\n", payload.objective);
    if !payload.exclusions.is_empty() {
        out.push_str("## Exclusions\n\n");
        payload.exclusions.iter().for_each(|item| {
            let _ = writeln!(out, "- {item}");
        });
        out.push('\n');
    }
    if !payload.decisions.is_empty() {
        out.push_str("## Decisions\n\n");
        payload.decisions.iter().for_each(|item| {
            let _ = writeln!(out, "- {item}");
        });
        out.push('\n');
    }
    out.push_str("## Requirements\n\n| Criterion | Requirement | Verification | Checks |\n|---|---|---|---|\n");
    for requirement in &payload.requirements {
        let verification = match requirement.verification_mode {
            VerificationMode::ExecutedCheck => "executed check".to_owned(),
            VerificationMode::DocumentCheck => "document check".to_owned(),
            VerificationMode::NotApplicable => format!(
                "not applicable — {}",
                requirement.not_applicable_reason.as_deref().unwrap_or("")
            ),
        };
        let _ = writeln!(
            out,
            "| {} | {} — {} | {} | {} |",
            criterion_label(table, &requirement.criterion_ref),
            cell(&requirement.requirement),
            cell(&requirement.verification_intent),
            cell(&verification),
            handles(&requirement.required_check_refs)
        );
    }
    out.push_str("\n## Interface intent\n\n");
    for intent in &payload.interface_intent {
        let _ = writeln!(
            out,
            "- **{}** — {} (sources: {}; criteria: {})",
            enum_name(&intent.intent),
            intent.description,
            intent
                .source_refs
                .iter()
                .map(|h| path_label(table, h))
                .collect::<Vec<_>>()
                .join(", "),
            handles(&intent.criterion_refs)
        );
    }
    out.push_str("\n## Files\n\n");
    for handle in &payload.existing_file_refs {
        let _ = writeln!(out, "- existing: {}", path_label(table, handle));
    }
    for handle in &payload.planned_file_refs {
        let _ = writeln!(out, "- planned: {}", path_label(table, handle));
    }
    if !payload.unit_proposals.is_empty() {
        out.push('\n');
        render_proposals(&mut out, &payload.unit_proposals, table);
    }
    out
}

fn render_proposals(out: &mut String, proposals: &[UnitProposalV1], table: &ReferenceTable) {
    out.push_str("## Proposed work units\n\n| Unit | Kind | Criteria | Destinations | Depends on | Objective |\n|---|---|---|---|---|---|\n");
    for proposal in proposals {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} |",
            proposal.unit_key,
            enum_name(&proposal.kind),
            handles(&proposal.criterion_refs),
            proposal
                .destination_refs
                .iter()
                .map(|h| path_label(table, h))
                .collect::<Vec<_>>()
                .join(", "),
            handles(&proposal.depends_on),
            cell(&proposal.objective)
        );
    }
}

/// One approved unit as the developer reviews it at the Contract checkpoint:
/// what it writes and what it waits for, with paths already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedUnitRow {
    pub key: Handle,
    pub kind: String,
    pub task_refs: Vec<Handle>,
    pub criterion_refs: Vec<Handle>,
    pub destinations: Vec<String>,
    pub depends_on: Vec<Handle>,
    pub objective: String,
}

pub fn render_contract_document(
    title: &str,
    payload: &InterfaceContractV1,
    table: &ReferenceTable,
    preserved_note: Option<&str>,
    planned_units: &[PlannedUnitRow],
) -> String {
    let mut out = String::new();
    heading(&mut out, &format!("Contract — {title}"));
    if let Some(note) = preserved_note {
        let _ = writeln!(out, "{note}\n");
    }
    if payload.groups.is_empty() {
        out.push_str("No new or changed interface is required by the approved specification.\n\n");
    }
    for group in &payload.groups {
        let _ = writeln!(
            out,
            "## `{}` ({})\n",
            group.interface_key,
            enum_name(&group.intent)
        );
        let _ = writeln!(out, "```\n{}\n```\n", group.signature);
        out.push_str("Invariants:\n\n");
        group.invariants.iter().for_each(|item| {
            let _ = writeln!(out, "- {item}");
        });
        let _ = writeln!(
            out,
            "\nSources: {}\n\nCriteria: {}\n",
            group
                .source_refs
                .iter()
                .map(|h| path_label(table, h))
                .collect::<Vec<_>>()
                .join(", "),
            handles(&group.criterion_refs)
        );
    }
    if !payload.unit_bindings.is_empty() {
        out.push_str("## Unit bindings\n\n| Unit | Interfaces | Depends on |\n|---|---|---|\n");
        for binding in &payload.unit_bindings {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} |",
                binding.unit_key,
                handles(&binding.interface_keys),
                handles(&binding.depends_on)
            );
        }
    }
    if !planned_units.is_empty() {
        out.push_str("\n## Planned units\n\nApproving this contract freezes the units below; Code and Test group them by file ownership and dependency before dispatch.\n\n| Unit | Kind | Tasks | Criteria | Writes | Depends on | Objective |\n|---|---|---|---|---|---|---|\n");
        for unit in planned_units {
            let writes = if unit.destinations.is_empty() {
                "—".to_owned()
            } else {
                unit.destinations
                    .iter()
                    .map(|path| format!("`{path}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let depends = if unit.depends_on.is_empty() {
                "—".to_owned()
            } else {
                handles(&unit.depends_on)
            };
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {writes} | {depends} | {} |",
                unit.key,
                unit.kind,
                handles(&unit.task_refs),
                handles(&unit.criterion_refs),
                cell(&unit.objective)
            );
        }
    }
    references_section(&mut out, table, &payload.used_refs);
    out
}

pub fn render_test_report(
    title: &str,
    coverage: &[TestCoverageV1],
    checks: &[CheckSummaryV1],
    table: &ReferenceTable,
    frontmatter: &Frontmatter,
) -> String {
    let mut out = frontmatter.render();
    heading(&mut out, &format!("Test report — {title}"));
    out.push_str("Authored coverage and executed checks are reported separately; a declaration below never means a test ran.\n\n");
    out.push_str(
        "## Coverage\n\n| Criterion | Coverage | Tests | Checks | Notes |\n|---|---|---|---|---|\n",
    );
    for item in coverage {
        let tests = match item.coverage {
            CoverageKind::Authored => handles(&item.test_artifact_keys),
            CoverageKind::Existing => item
                .test_input_refs
                .iter()
                .map(|h| path_label(table, h))
                .collect::<Vec<_>>()
                .join(", "),
            CoverageKind::NotApplicable => "—".to_owned(),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            criterion_label(table, &item.criterion_ref),
            enum_name(&item.coverage),
            tests,
            handles(&item.check_refs),
            cell(&item.explanation)
        );
    }
    checks_section(&mut out, checks);
    out
}

pub fn render_integration_report(
    title: &str,
    assessments: &[CriterionAssessmentV1],
    blockers: &[String],
    checks: &[CheckSummaryV1],
    table: &ReferenceTable,
) -> String {
    let mut out = String::new();
    heading(&mut out, &format!("Integration report — {title}"));
    out.push_str("## Criterion assessment\n\n| Criterion | Verdict | Inputs | Checks | Explanation |\n|---|---|---|---|---|\n");
    for item in assessments {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            criterion_label(table, &item.criterion_ref),
            enum_name(&item.verdict),
            handles(&item.input_refs),
            handles(&item.check_refs),
            cell(&item.explanation)
        );
    }
    if !blockers.is_empty() {
        out.push_str("\n## Blockers\n\n");
        blockers.iter().for_each(|item| {
            let _ = writeln!(out, "- {item}");
        });
    }
    checks_section(&mut out, checks);
    out
}

fn checks_section(out: &mut String, checks: &[CheckSummaryV1]) {
    out.push_str("\n## Host checks\n\n");
    if checks.is_empty() {
        out.push_str("No host check was configured for this stage; nothing was executed.\n");
        return;
    }
    out.push_str("| Check | Required | Status | Receipt |\n|---|---|---|---|\n");
    for check in checks {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} |",
            check.check_ref,
            if check.required { "yes" } else { "no" },
            enum_name(&check.status),
            check
                .receipt_id
                .as_ref()
                .map(|id| format!("`{id}`"))
                .unwrap_or_else(|| "—".to_owned())
        );
    }
}

fn references_section(out: &mut String, table: &ReferenceTable, used: &[Handle]) {
    if used.is_empty() {
        return;
    }
    out.push_str("\n## References used\n\n");
    for handle in used {
        let _ = writeln!(out, "- {}", path_label(table, handle));
    }
}

fn handles(items: &[Handle]) -> String {
    if items.is_empty() {
        "—".to_owned()
    } else {
        items
            .iter()
            .map(|h| format!("`{h}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn enum_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::references::ReferenceTable;
    use super::super::*;
    use super::*;

    fn h(text: &str) -> Handle {
        Handle::new(text).unwrap()
    }

    fn table() -> ReferenceTable {
        let mut builder = ReferenceTable::builder();
        builder
            .add_task(
                "t-1",
                1,
                "User login",
                &[("AC-1".into(), "Rejects an expired token.".into())],
            )
            .unwrap();
        builder
            .add_existing(
                RelPath::new("src/auth.rs").unwrap(),
                Sha256Hex::of(b"a"),
                SourceRole::Source,
            )
            .unwrap();
        builder
            .add_planned(
                RelPath::new("tests/auth_test.rs").unwrap(),
                SourceRole::Test,
            )
            .unwrap();
        builder.add_unavailable(UnavailableReason::Missing, "No fixture.");
        builder.add_check(h("cargo_test")).unwrap();
        builder.build()
    }

    #[test]
    fn frontmatter_distinguishes_physical_from_planned_files_and_reports_delivery_coverage() {
        let table = table();
        let frontmatter = Frontmatter::from_references(&table, &[h("src_1"), h("plan_1")]);
        assert_eq!(frontmatter.coverage_state, CoverageState::Complete);
        let rendered = frontmatter.render();
        assert_eq!(
            rendered,
            "---\nreference-contract: stage-composition-v1\nsource-files:\n  - \"src/auth.rs\"\ntest-files: []\nplanned-source-files: []\nplanned-test-files:\n  - \"tests/auth_test.rs\"\ncoverage-state: complete\n---\n"
        );
        let partial = Frontmatter::from_references(&table, &[h("unav_1")]);
        assert_eq!(partial.coverage_state, CoverageState::Partial);
        assert!(partial.render().contains("source-files: []"));
        // The legacy validator accepts the versioned note with empty physical arrays.
        let note = format!("{}# Greenfield\n", partial.render());
        crate::workspace_service::validate_generated_vault_note(
            std::path::Path::new("."),
            "knowledge/obsidian/features/greenfield.md",
            &note,
        )
        .unwrap();
        // ...but a legacy note with empty arrays still fails, and a versioned
        // note missing one of the four arrays fails too.
        let legacy = "---\nsource-files: []\ntest-files: []\n---\n# x\n";
        assert!(
            crate::workspace_service::validate_generated_vault_note(
                std::path::Path::new("."),
                "knowledge/obsidian/features/x.md",
                legacy
            )
            .is_err()
        );
        let truncated = "---\nreference-contract: stage-composition-v1\nsource-files: []\ntest-files: []\ncoverage-state: complete\n---\n# x\n";
        assert!(
            crate::workspace_service::validate_generated_vault_note(
                std::path::Path::new("."),
                "knowledge/obsidian/features/x.md",
                truncated
            )
            .is_err()
        );
        let bad_state = "---\nreference-contract: stage-composition-v1\nsource-files: []\ntest-files: []\nplanned-source-files: []\nplanned-test-files: []\ncoverage-state: perfect\n---\n# x\n";
        assert!(
            crate::workspace_service::validate_generated_vault_note(
                std::path::Path::new("."),
                "knowledge/obsidian/tests/x.md",
                bad_state
            )
            .is_err()
        );
    }

    #[test]
    fn rendered_documents_are_marked_host_generated_and_pass_the_language_guard() {
        let table = table();
        let intake = render_intake_note(
            "User login",
            &IntakePayloadV1 {
                tasks: vec![IntakeTaskV1 {
                    task_ref: h("task_1"),
                    classification: TaskClassification::Feature,
                    risk: Risk::Unknown,
                    summary: "Add | token expiry handling.".into(),
                }],
                issues: vec!["No test file exists yet.".into()],
                used_refs: vec![h("src_1")],
            },
            &table,
        );
        assert!(intake.starts_with(HOST_GENERATED_MARKER));
        assert!(intake.contains(
            "| `task_1` — User login | feature | unknown | Add \\| token expiry handling. |"
        ));
        assert!(intake.contains("`src_1` → `src/auth.rs`"));
        crate::agent_output_language::validate_agent_artifact("Intake", None, &intake).unwrap();

        let frontmatter = Frontmatter::from_references(&table, &[h("src_1"), h("plan_1")]);
        let spec = render_specification_note(
            "User login",
            &SpecificationContractV1 {
                objective: "Reject expired tokens.".into(),
                exclusions: vec![],
                decisions: vec!["Keep the public signature.".into()],
                requirements: vec![RequirementV1 {
                    task_ref: h("task_1"),
                    criterion_ref: h("crit_1_1"),
                    requirement: "Return an error for expired tokens.".into(),
                    verification_intent: "Run the configured test.".into(),
                    verification_mode: VerificationMode::ExecutedCheck,
                    required_check_refs: vec![h("cargo_test")],
                    not_applicable_reason: None,
                }],
                existing_file_refs: vec![h("src_1")],
                planned_file_refs: vec![h("plan_1")],
                interface_intent: vec![InterfaceIntentV1 {
                    intent: InterfaceIntent::Preserve,
                    source_refs: vec![h("src_1")],
                    criterion_refs: vec![h("crit_1_1")],
                    description: "Keep the validator signature.".into(),
                }],
                unit_proposals: vec![UnitProposalV1 {
                    unit_key: h("implement_a"),
                    kind: ProposalKind::Code,
                    task_refs: vec![h("task_1")],
                    criterion_refs: vec![h("crit_1_1")],
                    input_refs: vec![h("src_1")],
                    destination_refs: vec![h("src_1")],
                    depends_on: vec![],
                    objective: "Implement the expiry check.".into(),
                }],
            },
            &table,
            &frontmatter,
        );
        assert!(spec.starts_with("---\nreference-contract: stage-composition-v1\n"));
        assert!(spec.contains("## Proposed work units"));
        assert!(spec.contains("`crit_1_1` (AC-1) — Rejects an expired token."));
        crate::workspace_service::validate_generated_vault_note(
            std::path::Path::new("."),
            "knowledge/obsidian/features/user-login.md",
            &spec,
        )
        .unwrap();
        crate::agent_output_language::validate_agent_artifact("Specification", None, &spec)
            .unwrap();

        let contract = render_contract_document(
            "User login",
            &InterfaceContractV1 {
                groups: vec![],
                unit_bindings: vec![],
                used_refs: vec![h("src_1")],
            },
            &table,
            Some("Preserved: the approved specification declares no interface change."),
            &[PlannedUnitRow {
                key: h("implement_login"),
                kind: "code".into(),
                task_refs: vec![h("task_1")],
                criterion_refs: vec![h("crit_1_1")],
                destinations: vec!["src/auth.rs".into()],
                depends_on: vec![],
                objective: "Reject expired tokens.".into(),
            }],
        );
        assert!(contract.contains("No new or changed interface is required"));
        assert!(contract.contains("| `implement_login` | code | `task_1` | `crit_1_1` | `src/auth.rs` | — | Reject expired tokens. |"));
        assert!(contract.contains("Preserved:"));

        let report = render_test_report(
            "User login",
            &[TestCoverageV1 {
                task_ref: h("task_1"),
                criterion_ref: h("crit_1_1"),
                coverage: CoverageKind::Authored,
                test_artifact_keys: vec![h("auth_test")],
                test_input_refs: vec![],
                check_refs: vec![h("cargo_test")],
                explanation: "Covers the expiry branch.".into(),
            }],
            &[CheckSummaryV1 {
                check_ref: h("cargo_test"),
                required: true,
                receipt_id: None,
                status: CheckStatus::NotRun,
            }],
            &table,
            &frontmatter,
        );
        assert!(report.contains("| `cargo_test` | yes | not run | — |"));
        assert!(report.contains("never means a test ran"));

        let integration = render_integration_report(
            "User login",
            &[CriterionAssessmentV1 {
                task_ref: h("task_1"),
                criterion_ref: h("crit_1_1"),
                verdict: Verdict::Unverified,
                input_refs: vec![h("src_1")],
                check_refs: vec![],
                explanation: "The required receipt is unavailable.".into(),
            }],
            &["Required execution evidence is unavailable.".into()],
            &[],
            &table,
        );
        assert!(integration.contains("| unverified |"));
        assert!(integration.contains("## Blockers"));
        assert!(integration.contains("nothing was executed"));
        // Determinism: identical inputs, identical bytes.
        assert_eq!(
            integration,
            render_integration_report(
                "User login",
                &[CriterionAssessmentV1 {
                    task_ref: h("task_1"),
                    criterion_ref: h("crit_1_1"),
                    verdict: Verdict::Unverified,
                    input_refs: vec![h("src_1")],
                    check_refs: vec![],
                    explanation: "The required receipt is unavailable.".into(),
                }],
                &["Required execution evidence is unavailable.".into()],
                &[],
                &table,
            )
        );
    }
}
