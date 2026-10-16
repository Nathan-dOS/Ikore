use super::*;
use crate::orchestration::SchemaV1;
use crate::orchestration::composition::recipes::UnitPayloadRecord;
use crate::orchestration::composition::{
    CompletedPayloadV1, CriterionAssessmentV1, EditEntryV1, EditOperationV1, ExecutorKind,
    IntegrationPayloadV1, InterfaceContractV1, InterfaceGroupV1, InterfaceIntentV1,
    ReferenceEntryV1, RelPath, RequirementV1, SourceRole, TestCoverageV1, TestPayloadV1,
    UnitInterfaceBindingV1, UnitModelResponseV1, UnitProposalV1,
};

fn h(value: &str) -> Handle {
    Handle::new(value).unwrap()
}

fn visual(brief: &ReviewBrief) -> &SpecificationVisual {
    match &brief.visual {
        StageVisual::Specification(visual) => visual,
        other => panic!("not a Specification brief: {other:?}"),
    }
}

fn table() -> ReferenceTable {
    let mut builder = ReferenceTable::builder();
    builder
        .add_task(
            "t-core",
            1,
            "Export usage summary to CSV",
            &[
                ("AC-1".into(), "One row per pulsation and model.".into()),
                ("AC-2".into(), "Empty range writes only the header.".into()),
            ],
        )
        .unwrap();
    builder
        .add_task(
            "t-ui",
            1,
            "Expose CSV export",
            &[("AC-3".into(), "Export is disabled while loading.".into())],
        )
        .unwrap();
    builder.build()
}

fn requirement(
    task: &str,
    criterion: &str,
    mode: VerificationMode,
    reason: Option<&str>,
) -> RequirementV1 {
    RequirementV1 {
        task_ref: h(task),
        criterion_ref: h(criterion),
        requirement: format!("Requirement for {criterion}."),
        verification_intent: format!("Verify {criterion}."),
        verification_mode: mode,
        required_check_refs: Vec::new(),
        not_applicable_reason: reason.map(str::to_owned),
    }
}

fn record(spec: SpecificationContractV1) -> StagePayloadRecord {
    let sha = "0".repeat(64);
    StagePayloadRecord {
        schema_version: SchemaV1,
        stage: crate::orchestration::composition::Stage::Specification,
        plan_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
        references: vec![
            ReferenceEntryV1::ExistingSource {
                handle: h("src_1"),
                relative_path: RelPath::new("crates/hvc-core/src/usage.rs").unwrap(),
                content_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
                source_role: SourceRole::Source,
            },
            ReferenceEntryV1::ExistingSource {
                handle: h("src_2"),
                relative_path: RelPath::new("crates/hvc-ui/src/usage.rs").unwrap(),
                content_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
                source_role: SourceRole::Source,
            },
            ReferenceEntryV1::PlannedDestination {
                handle: h("dst_1"),
                relative_path: RelPath::new("crates/hvc-core/src/usage/export.rs").unwrap(),
                source_role: SourceRole::Source,
            },
            ReferenceEntryV1::ExistingSource {
                handle: h("src_3"),
                relative_path: RelPath::new("README.md").unwrap(),
                content_sha256: serde_json::from_value(serde_json::json!(sha)).unwrap(),
                source_role: SourceRole::Document,
            },
        ],
        units: vec![UnitPayloadRecord {
            logical_unit_key: h("specification"),
            response: UnitModelResponseV1::Completed {
                executor_kind: ExecutorKind::SpecificationAuthor,
                payload: CompletedPayloadV1::Specification(spec),
            },
        }],
    }
}

fn spec() -> SpecificationContractV1 {
    SpecificationContractV1 {
        objective: "Export token usage to CSV.".into(),
        exclusions: vec!["No scheduled export.".into(), "No per-call rows.".into()],
        decisions: vec!["Aggregate per pulsation and model.".into()],
        requirements: vec![
            requirement("task_1", "crit_1_1", VerificationMode::ExecutedCheck, None),
            requirement("task_1", "crit_1_2", VerificationMode::DocumentCheck, None),
            requirement(
                "task_2",
                "crit_2_1",
                VerificationMode::NotApplicable,
                Some("Covered by the existing layout suite."),
            ),
        ],
        existing_file_refs: vec![h("src_1"), h("src_2"), h("src_3")],
        planned_file_refs: vec![h("dst_1")],
        interface_intent: vec![
            InterfaceIntentV1 {
                intent: InterfaceIntent::Change,
                source_refs: vec![h("src_2")],
                criterion_refs: vec![h("crit_2_1")],
                description: "The Usage view gains an Export button.".into(),
            },
            InterfaceIntentV1 {
                intent: InterfaceIntent::Preserve,
                source_refs: vec![h("src_1")],
                criterion_refs: vec![h("crit_1_1")],
                description: "query_usage keeps its signature.".into(),
            },
            InterfaceIntentV1 {
                intent: InterfaceIntent::New,
                source_refs: Vec::new(),
                criterion_refs: vec![h("crit_1_1")],
                description: "write_usage_csv is new.".into(),
            },
        ],
        unit_proposals: vec![
            UnitProposalV1 {
                unit_key: h("core_export"),
                kind: ProposalKind::Code,
                task_refs: vec![h("task_1")],
                criterion_refs: vec![h("crit_1_1"), h("crit_1_2")],
                input_refs: vec![h("src_1")],
                destination_refs: vec![h("src_1"), h("dst_1")],
                depends_on: Vec::new(),
                objective: "Write the CSV export.".into(),
            },
            UnitProposalV1 {
                unit_key: h("ui_export"),
                kind: ProposalKind::Code,
                task_refs: vec![h("task_2")],
                criterion_refs: vec![h("crit_2_1")],
                input_refs: vec![h("src_2")],
                destination_refs: vec![h("src_2")],
                depends_on: vec![h("core_export")],
                objective: "Add the Export button.".into(),
            },
        ],
    }
}

#[test]
fn specification_brief_labels_handles_and_orders_judgement_by_rule() {
    let brief = specification::build(&spec(), &record(spec()), &table());
    assert_eq!(brief.next_stage, Some(PulsationStage::Contract));
    assert_eq!(
        visual(&brief).facts,
        BriefFacts {
            tasks: 2,
            criteria: 3,
            executed: 1,
            by_reading: 1,
            not_applicable: 1,
            existing_files: 3,
            planned_files: 1,
            changed_interfaces: 1,
            units: 2,
        }
    );
    let kinds: Vec<(JudgementKind, &str)> = brief
        .judgement
        .iter()
        .map(|item| (item.kind, item.label.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (JudgementKind::Decision, "D1"),
            (JudgementKind::ReadingOnlyProof, "AC-2"),
            (JudgementKind::NotApplicable, "AC-3"),
            (JudgementKind::InterfaceChange, "crates/hvc-ui/src/usage.rs"),
            (JudgementKind::Exclusions, "2"),
        ]
    );
    // The developer's own criterion text leads; the agent's intent follows.
    assert_eq!(
        brief.judgement[1].text,
        "Empty range writes only the header."
    );
    assert_eq!(
        brief.judgement[1].detail.as_deref(),
        Some("Verify crit_1_2.")
    );
    assert_eq!(
        brief.judgement[2].detail.as_deref(),
        Some("Covered by the existing layout suite.")
    );
    assert_eq!(
        brief.judgement[3].refs,
        ["crates/hvc-ui/src/usage.rs", "AC-3"]
    );
    assert_eq!(
        brief.judgement[4].text,
        "No scheduled export. · No per-call rows."
    );
}

#[test]
fn specification_brief_groups_files_and_keeps_unattached_interfaces() {
    let brief = specification::build(&spec(), &record(spec()), &table());
    let groups: Vec<(&str, Vec<(&str, bool)>)> = visual(&brief)
        .files
        .iter()
        .map(|group| {
            (
                group.root.as_str(),
                group
                    .files
                    .iter()
                    .map(|file| (file.path.as_str(), file.planned))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        groups,
        [
            ("", vec![("README.md", false)]),
            (
                "crates/hvc-core",
                vec![("src/usage.rs", false), ("src/usage/export.rs", true)]
            ),
            ("crates/hvc-ui", vec![("src/usage.rs", false)]),
        ]
    );
    assert_eq!(
        visual(&brief).files[1].files[0].interfaces[0].intent,
        InterfaceIntent::Preserve
    );
    assert_eq!(visual(&brief).unattached_interfaces.len(), 1);
    assert_eq!(
        visual(&brief).unattached_interfaces[0].intent,
        InterfaceIntent::New
    );
}

#[test]
fn specification_brief_lists_criteria_per_task_and_units_by_path() {
    let brief = specification::build(&spec(), &record(spec()), &table());
    assert_eq!(visual(&brief).tasks.len(), 2);
    assert_eq!(visual(&brief).tasks[0].title, "Export usage summary to CSV");
    let ids: Vec<&str> = visual(&brief).tasks[0]
        .criteria
        .iter()
        .map(|criterion| criterion.criterion_id.as_str())
        .collect();
    assert_eq!(ids, ["AC-1", "AC-2"]);
    assert_eq!(
        visual(&brief).units[0].destinations,
        [
            "crates/hvc-core/src/usage.rs",
            "crates/hvc-core/src/usage/export.rs"
        ]
    );
    assert_eq!(visual(&brief).units[0].criteria, ["AC-1", "AC-2"]);
    assert_eq!(visual(&brief).units[1].depends_on, ["core_export"]);
}

#[test]
fn verified_lines_only_claim_what_validation_guarantees() {
    let mut minimal = spec();
    minimal.existing_file_refs.clear();
    minimal.planned_file_refs.clear();
    minimal.unit_proposals.clear();
    minimal.requirements.truncate(1);
    let brief = specification::build(&minimal, &record(minimal.clone()), &table());
    assert_eq!(
        brief.verified,
        ["The acceptance criterion has exactly one requirement."]
    );
    let full = specification::build(&spec(), &record(spec()), &table());
    assert_eq!(full.verified.len(), 5, "{:?}", full.verified);
}

#[test]
fn root_split_is_stable() {
    assert_eq!(
        split_root("crates/hvc-ui/src/a.rs"),
        ("crates/hvc-ui".into(), "src/a.rs".into())
    );
    assert_eq!(split_root("docs/a.md"), ("docs".into(), "a.md".into()));
    assert_eq!(split_root("README.md"), (String::new(), "README.md".into()));
    assert_eq!(split_root("crates/x"), ("crates".into(), "x".into()));
}

fn brief_of(spec: SpecificationContractV1) -> ReviewBrief {
    specification::build(&spec, &record(spec.clone()), &table())
}

fn line(change: &BriefChange) -> String {
    format!(
        "{:?} {:?} {} {} | {} -> {}",
        change.kind,
        change.subject,
        change.label,
        change.field.unwrap_or("-"),
        change.before.as_deref().unwrap_or("-"),
        change.after.as_deref().unwrap_or("-"),
    )
}

#[test]
fn an_unchanged_revision_has_no_changes() {
    assert!(compare(&brief_of(spec()), &brief_of(spec())).is_empty());
}

#[test]
fn revision_changes_are_keyed_by_criterion_path_and_unit_in_brief_order() {
    let mut revised = spec();
    revised.objective = "Export token usage to CSV, per day.".into();
    revised.decisions = vec![
        "Aggregate per pulsation, model and day.".into(),
        "Round costs to cents.".into(),
    ];
    revised.requirements[1].verification_mode = VerificationMode::ExecutedCheck;
    revised.requirements[1].verification_intent = "Run the empty-range test.".into();
    revised.interface_intent[0].intent = InterfaceIntent::Preserve;
    revised.interface_intent.truncate(2);
    revised.exclusions.truncate(1);
    revised
        .existing_file_refs
        .retain(|handle| handle != &h("src_3"));
    revised.unit_proposals[1].unit_key = h("ui_button");

    let changes: Vec<String> = compare(&brief_of(spec()), &brief_of(revised))
        .iter()
        .map(line)
        .collect();
    assert_eq!(
        changes,
        [
            "Changed Objective  - | Export token usage to CSV. -> Export token usage to CSV, per day.",
            "Removed Decision D1 - | Aggregate per pulsation and model. -> -",
            "Added Decision D1 - | - -> Aggregate per pulsation, model and day.",
            "Added Decision D2 - | - -> Round costs to cents.",
            "Changed Criterion AC-2 proof | by reading -> executed check",
            "Changed Criterion AC-2 proof plan | Verify crit_1_2. -> Run the empty-range test.",
            "Moved Criterion AC-3 work units | ui_export -> ui_button",
            "Removed Interface  - | new: write_usage_csv is new. -> -",
            "Changed Interface crates/hvc-ui/src/usage.rs intent | change -> preserve",
            "Removed Exclusion  - | No per-call rows. -> -",
            "Removed File README.md - | existing file -> -",
            "Added Unit ui_button - | - -> Add the Export button.",
            "Removed Unit ui_export - | Add the Export button. -> -",
        ]
    );
}

#[test]
fn a_criterion_the_previous_revision_lacked_reads_as_added_only() {
    let mut previous = spec();
    previous.requirements.truncate(2);
    let changes = compare(&brief_of(previous), &brief_of(spec()));
    let about_ac3: Vec<String> = changes
        .iter()
        .filter(|change| change.label == "AC-3")
        .map(line)
        .collect();
    assert_eq!(
        about_ac3,
        ["Added Criterion AC-3 - | - -> Requirement for crit_2_1."]
    );
}

// ---------------------------------------------------------------------------
// Contract, Test and Integrator briefs (M4)
// ---------------------------------------------------------------------------

/// A record of `stage` over the Specification fixture's references, plus an
/// existing test file (`tst_1`) and a planned one (`dst_2`).
fn stage_record(
    stage: Stage,
    units: Vec<(ExecutorKind, CompletedPayloadV1)>,
) -> StagePayloadRecord {
    let mut record = record(spec());
    record.stage = stage;
    record.references.push(ReferenceEntryV1::ExistingSource {
        handle: h("tst_1"),
        relative_path: RelPath::new("crates/hvc-core/tests/usage_test.rs").unwrap(),
        content_sha256: serde_json::from_value(serde_json::json!("0".repeat(64))).unwrap(),
        source_role: SourceRole::Test,
    });
    record
        .references
        .push(ReferenceEntryV1::PlannedDestination {
            handle: h("dst_2"),
            relative_path: RelPath::new("crates/hvc-core/tests/export_test.rs").unwrap(),
            source_role: SourceRole::Test,
        });
    record.units = units
        .into_iter()
        .enumerate()
        .map(|(index, (executor_kind, payload))| UnitPayloadRecord {
            logical_unit_key: h(&format!("unit_{}", index + 1)),
            response: UnitModelResponseV1::Completed {
                executor_kind,
                payload,
            },
        })
        .collect();
    record
}

fn contract() -> InterfaceContractV1 {
    let group =
        |key: &str, intent, sources: &[&str], criterion: &str, signature: &str| InterfaceGroupV1 {
            interface_key: h(key),
            intent,
            source_refs: sources.iter().map(|source| h(source)).collect(),
            criterion_refs: vec![h(criterion)],
            signature: signature.into(),
            invariants: vec![format!("{key} never panics.")],
        };
    InterfaceContractV1 {
        groups: vec![
            group(
                "query_usage",
                GroupIntent::Preserve,
                &["src_1"],
                "crit_1_1",
                "pub fn query_usage(range: Range) -> Vec<Usage>",
            ),
            group(
                "csv_writer",
                GroupIntent::New,
                &[],
                "crit_1_1",
                "pub fn write_usage_csv(rows: &[Usage]) -> String",
            ),
            group(
                "usage_view",
                GroupIntent::Change,
                &["src_2"],
                "crit_2_1",
                "fn render_export_button(&self, loading: bool) -> Button",
            ),
        ],
        unit_bindings: vec![
            UnitInterfaceBindingV1 {
                unit_key: h("core_export"),
                interface_keys: vec![h("query_usage"), h("csv_writer")],
                depends_on: Vec::new(),
            },
            UnitInterfaceBindingV1 {
                unit_key: h("ui_export"),
                interface_keys: vec![h("usage_view")],
                depends_on: vec![h("core_export")],
            },
        ],
        used_refs: Vec::new(),
    }
}

fn contract_brief(contract: InterfaceContractV1) -> ReviewBrief {
    let specification = record(spec());
    let record = stage_record(
        Stage::Contract,
        vec![(
            ExecutorKind::ContractAuthor,
            CompletedPayloadV1::Contract(contract),
        )],
    );
    contract::build(&record, &table(), Some((&spec(), &specification))).unwrap()
}

#[test]
fn contract_brief_judges_changed_and_new_interfaces_and_binds_the_approved_units() {
    let brief = contract_brief(contract());
    assert_eq!(brief.next_stage, Some(PulsationStage::Code));
    assert_eq!(
        brief.objective.as_deref(),
        Some("Export token usage to CSV.")
    );
    assert_eq!(
        brief.counts,
        [
            "3 interface groups · 1 changed, 1 new, 1 preserved",
            "2 work units · 2 bound to an interface"
        ]
    );
    let judgement: Vec<(JudgementKind, &str, &str)> = brief
        .judgement
        .iter()
        .map(|item| (item.kind, item.label.as_str(), item.text.as_str()))
        .collect();
    assert_eq!(
        judgement,
        [
            (
                JudgementKind::NewInterface,
                "csv_writer",
                "pub fn write_usage_csv(rows: &[Usage]) -> String"
            ),
            (
                JudgementKind::InterfaceChange,
                "usage_view",
                "fn render_export_button(&self, loading: bool) -> Button"
            ),
        ]
    );
    assert_eq!(
        brief.judgement[1].detail.as_deref(),
        Some("Invariants: usage_view never panics.")
    );
    assert_eq!(
        brief.judgement[1].refs,
        ["crates/hvc-ui/src/usage.rs", "AC-3"]
    );
    let StageVisual::Contract(visual) = &brief.visual else {
        panic!("not a Contract brief");
    };
    assert_eq!(
        visual.interfaces[0].sources,
        ["crates/hvc-core/src/usage.rs"]
    );
    assert_eq!(visual.interfaces[0].units, ["core_export"]);
    assert_eq!(visual.units[0].interfaces, ["query_usage", "csv_writer"]);
    assert_eq!(visual.units[1].depends_on, ["core_export"]);
    assert_eq!(
        visual.units[0].destinations,
        [
            "crates/hvc-core/src/usage.rs",
            "crates/hvc-core/src/usage/export.rs"
        ]
    );
    assert_eq!(brief.verified.len(), 4, "{:?}", brief.verified);
}

#[test]
fn a_contract_without_an_approved_specification_still_shows_its_interfaces() {
    let record = stage_record(
        Stage::Contract,
        vec![(
            ExecutorKind::ContractAuthor,
            CompletedPayloadV1::Contract(contract()),
        )],
    );
    let brief = contract::build(&record, &table(), None).unwrap();
    assert_eq!(brief.objective, None);
    let StageVisual::Contract(visual) = &brief.visual else {
        panic!("not a Contract brief");
    };
    assert_eq!(visual.interfaces.len(), 3);
    assert!(visual.units.is_empty());
    assert_eq!(brief.counts.len(), 1);
}

fn coverage(criterion: &str, task: &str, kind: CoverageKind) -> TestCoverageV1 {
    TestCoverageV1 {
        task_ref: h(task),
        criterion_ref: h(criterion),
        coverage: kind,
        test_artifact_keys: if kind == CoverageKind::Authored {
            vec![h("export_test")]
        } else {
            Vec::new()
        },
        test_input_refs: if kind == CoverageKind::Existing {
            vec![h("tst_1")]
        } else {
            Vec::new()
        },
        check_refs: vec![h("core_tests")],
        explanation: format!("Coverage of {criterion}."),
    }
}

fn tests_payload(kinds: [CoverageKind; 3]) -> TestPayloadV1 {
    TestPayloadV1 {
        edits: vec![EditEntryV1 {
            artifact_key: h("export_test"),
            edit: EditOperationV1::CreateFile {
                destination_ref: h("dst_2"),
                content: "#[test]\nfn header_only() {}\n".into(),
            },
        }],
        coverage: vec![
            coverage("crit_1_1", "task_1", kinds[0]),
            coverage("crit_1_2", "task_1", kinds[1]),
            coverage("crit_2_1", "task_2", kinds[2]),
        ],
        evidence: Vec::new(),
        used_refs: Vec::new(),
    }
}

fn check(handle: &str, status: CheckStatus, receipt: Option<&str>) -> CheckSummaryV1 {
    CheckSummaryV1 {
        check_ref: h(handle),
        required: true,
        receipt_id: receipt.map(|id| crate::orchestration::composition::Uuid::new(id).unwrap()),
        status,
    }
}

const RECEIPT: &str = "0b6f1c2e-8a4d-4c3b-9e2f-1a2b3c4d5e6f";

fn test_brief(payload: TestPayloadV1, checks: &[CheckSummaryV1]) -> ReviewBrief {
    let specification = record(spec());
    let record = stage_record(
        Stage::Test,
        vec![(ExecutorKind::TestAuthor, CompletedPayloadV1::Tests(payload))],
    );
    test_stage::build(&record, &table(), Some((&spec(), &specification)), checks).unwrap()
}

#[test]
fn test_brief_keeps_declared_coverage_apart_from_executed_checks() {
    let checks = [
        check("core_tests", CheckStatus::Passed, Some(RECEIPT)),
        check("ui_tests", CheckStatus::NotRun, None),
    ];
    let brief = test_brief(
        tests_payload([
            CoverageKind::Authored,
            CoverageKind::Existing,
            CoverageKind::NotApplicable,
        ]),
        &checks,
    );
    assert_eq!(brief.next_stage, Some(PulsationStage::Integrator));
    assert_eq!(
        brief.counts,
        [
            "3 criteria · 1 authored, 1 existing, 1 not applicable",
            "2 host checks · 1 passed, 1 not run",
            "1 test file · 1 new"
        ]
    );
    let judgement: Vec<(JudgementKind, &str)> = brief
        .judgement
        .iter()
        .map(|item| (item.kind, item.label.as_str()))
        .collect();
    assert_eq!(
        judgement,
        [
            (JudgementKind::ExistingTestsOnly, "AC-2"),
            (JudgementKind::NotApplicable, "AC-3"),
            (JudgementKind::UnexecutedCheck, "ui_tests"),
        ]
    );
    assert_eq!(
        brief.judgement[2].text,
        "The host has no passing receipt for this required check: not run."
    );
    let StageVisual::Test(visual) = &brief.visual else {
        panic!("not a Test brief");
    };
    let rows: Vec<(&str, &str, Vec<&str>)> = visual
        .coverage
        .iter()
        .map(|row| {
            (
                row.criterion_id.as_str(),
                row.task_title.as_str(),
                row.tests.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "AC-1",
                "Export usage summary to CSV",
                vec!["crates/hvc-core/tests/export_test.rs"]
            ),
            (
                "AC-2",
                "Export usage summary to CSV",
                vec!["crates/hvc-core/tests/usage_test.rs"]
            ),
            ("AC-3", "Expose CSV export", vec![]),
        ]
    );
    // Declared is not executed: every row declares `core_tests`; only the
    // host receipt says it passed.
    assert!(
        visual
            .coverage
            .iter()
            .all(|row| row.declared_checks == ["core_tests"])
    );
    assert_eq!(visual.checks[0].receipt.as_deref(), Some(RECEIPT));
    assert_eq!(visual.checks[1].receipt, None);
    assert_eq!(visual.files[0].root, "crates/hvc-core");
    assert_eq!(visual.files[0].files[0].path, "tests/export_test.rs");
    assert!(visual.files[0].files[0].planned);
    assert!(
        brief
            .verified
            .iter()
            .any(|line| line.starts_with("Check statuses come from host receipts"))
    );
}

fn integration(verdicts: [Verdict; 3], blockers: &[&str]) -> IntegrationPayloadV1 {
    let assessment = |criterion: &str, task: &str, verdict| CriterionAssessmentV1 {
        task_ref: h(task),
        criterion_ref: h(criterion),
        verdict,
        input_refs: vec![h("src_1")],
        check_refs: Vec::new(),
        explanation: format!("Assessment of {criterion}."),
    };
    IntegrationPayloadV1 {
        assessments: vec![
            assessment("crit_1_1", "task_1", verdicts[0]),
            assessment("crit_1_2", "task_1", verdicts[1]),
            assessment("crit_2_1", "task_2", verdicts[2]),
        ],
        blockers: blockers.iter().map(|text| (*text).to_owned()).collect(),
        documentation_edits: Vec::new(),
        used_refs: Vec::new(),
    }
}

fn integrator_brief(payload: IntegrationPayloadV1, checks: &[CheckSummaryV1]) -> ReviewBrief {
    let publication = |path: &str, stage, state| BriefPublication {
        path: path.into(),
        stage,
        state,
    };
    let publications = [
        publication(
            "crates/hvc-core/src/usage.rs",
            PulsationStage::Code,
            PublicationState::Pending,
        ),
        publication(
            "crates/hvc-core/tests/export_test.rs",
            PulsationStage::Test,
            PublicationState::Pending,
        ),
        publication(
            "knowledge/obsidian/features/usage-export.md",
            PulsationStage::Specification,
            PublicationState::Applied,
        ),
    ];
    let specification = record(spec());
    let record = stage_record(
        Stage::Integrator,
        vec![(
            ExecutorKind::IntegrationReviewer,
            CompletedPayloadV1::Integration(payload),
        )],
    );
    integrator::build(
        &record,
        &table(),
        Some((&spec(), &specification)),
        checks,
        &publications,
    )
    .unwrap()
}

#[test]
fn integrator_brief_judges_every_verdict_short_of_supported_and_every_blocker() {
    let brief = integrator_brief(
        integration(
            [
                Verdict::Supported,
                Verdict::Contradicted,
                Verdict::Unverified,
            ],
            &["The CSV header order differs from the Usage view."],
        ),
        &[],
    );
    assert_eq!(brief.next_stage, None, "approval publishes");
    assert_eq!(
        brief.counts,
        [
            "3 criteria · 1 supported, 1 contradicted, 1 unverified",
            "1 blocker",
            "No host check for this stage",
            "3 files · 1 published, 2 to publish"
        ]
    );
    let judgement: Vec<(JudgementKind, &str)> = brief
        .judgement
        .iter()
        .map(|item| (item.kind, item.label.as_str()))
        .collect();
    assert_eq!(
        judgement,
        [
            (JudgementKind::Contradicted, "AC-2"),
            (JudgementKind::Unverified, "AC-3"),
            (JudgementKind::Blocker, "B1"),
        ]
    );
    assert_eq!(
        brief.judgement[0].detail.as_deref(),
        Some("Assessment of crit_1_2.")
    );
    let StageVisual::Integrator(visual) = &brief.visual else {
        panic!("not an Integrator brief");
    };
    assert_eq!(visual.verdicts.len(), 3);
    assert_eq!(visual.publications.len(), 3);
    assert_eq!(
        brief.verified,
        ["Each criterion in scope has exactly one verdict."]
    );
}

#[test]
fn later_stage_revisions_are_keyed_by_interface_criterion_check_and_blocker_text() {
    let mut revised = contract();
    revised.groups[2].intent = GroupIntent::Preserve;
    revised.groups[1].signature = "pub fn write_usage_csv(rows: &[Usage], out: &mut String)".into();
    revised.groups.remove(0);
    revised.unit_bindings[0].interface_keys.remove(0);
    let changes: Vec<String> = compare(&contract_brief(contract()), &contract_brief(revised))
        .iter()
        .map(line)
        .collect();
    assert_eq!(
        changes,
        [
            "Changed Interface csv_writer signature | pub fn write_usage_csv(rows: &[Usage]) -> String -> pub fn write_usage_csv(rows: &[Usage], out: &mut String)",
            "Changed Interface usage_view intent | change -> preserve",
            "Removed Interface query_usage - | preserve: pub fn query_usage(range: Range) -> Vec<Usage> -> -",
            "Changed Unit core_export interfaces | csv_writer, query_usage -> csv_writer",
        ]
    );

    let before = test_brief(
        tests_payload([
            CoverageKind::Authored,
            CoverageKind::Existing,
            CoverageKind::NotApplicable,
        ]),
        &[check("core_tests", CheckStatus::NotRun, None)],
    );
    let after = test_brief(
        tests_payload([
            CoverageKind::Authored,
            CoverageKind::Authored,
            CoverageKind::NotApplicable,
        ]),
        &[check("core_tests", CheckStatus::Passed, Some(RECEIPT))],
    );
    let changes: Vec<String> = compare(&before, &after).iter().map(line).collect();
    assert_eq!(
        changes,
        [
            "Changed Criterion AC-2 coverage | existing -> authored",
            "Changed Criterion AC-2 tests | crates/hvc-core/tests/usage_test.rs -> crates/hvc-core/tests/export_test.rs",
            "Changed Check core_tests status | not run -> passed",
        ]
    );

    let before = integrator_brief(
        integration(
            [
                Verdict::Supported,
                Verdict::Contradicted,
                Verdict::Unverified,
            ],
            &["The CSV header order differs from the Usage view."],
        ),
        &[],
    );
    let after = integrator_brief(
        integration(
            [Verdict::Supported, Verdict::Supported, Verdict::Unverified],
            &[],
        ),
        &[],
    );
    let changes: Vec<String> = compare(&before, &after).iter().map(line).collect();
    assert_eq!(
        changes,
        [
            "Changed Criterion AC-2 verdict | contradicted -> supported",
            "Removed Blocker B1 - | The CSV header order differs from the Usage view. -> -",
        ]
    );
}

fn failure(
    allow_retry: bool,
    targets: &[PulsationStage],
    permits: Option<u32>,
) -> crate::persistence_service::StageFailureCheckpoint {
    crate::persistence_service::StageFailureCheckpoint {
        checkpoint_id: "failure".into(),
        pulsation_id: "pulse".into(),
        project_id: "project".into(),
        feature_name: "Usage export".into(),
        stage: PulsationStage::Test,
        failure_class: "validation_failed".into(),
        error_code: "check_failed".into(),
        diagnostic: "core_tests failed".into(),
        allow_retry,
        allow_revision: !targets.is_empty(),
        revision_stage: targets.first().copied(),
        attempts: 3,
        state_version: 7,
        created_at: "2026-10-16T00:00:00Z".into(),
        allowed_revision_targets: if permits.is_some() {
            targets.to_vec()
        } else {
            Vec::new()
        },
        remaining_extra_invocations: permits,
        stage_execution_id: permits.map(|_| "execution".into()),
    }
}

#[test]
fn failure_choices_follow_the_permitted_decisions_and_say_what_each_does() {
    let choices = failure_choices(&failure(
        true,
        &[PulsationStage::Code, PulsationStage::Specification],
        Some(2),
    ));
    let kinds: Vec<(FailureDecisionKind, Option<PulsationStage>)> = choices
        .iter()
        .map(|choice| (choice.decision, choice.target))
        .collect();
    assert_eq!(
        kinds,
        [
            (FailureDecisionKind::Retry, None),
            (FailureDecisionKind::Revision, Some(PulsationStage::Code)),
            (
                FailureDecisionKind::Revision,
                Some(PulsationStage::Specification)
            ),
            (FailureDecisionKind::Abort, None),
        ]
    );
    assert!(
        choices[0]
            .effect
            .ends_with("It spends one of the 2 extra permits the stage has left.")
    );
    assert!(
        choices[1]
            .effect
            .starts_with("Supersedes the artifacts from Code onward and runs Code again.")
    );
    assert!(choices[1].effect.contains("`check_failed`"));

    // A legacy failure without retry offers its single revision target and abort.
    let legacy = failure_choices(&failure(false, &[PulsationStage::Code], None));
    let kinds: Vec<FailureDecisionKind> = legacy.iter().map(|choice| choice.decision).collect();
    assert_eq!(
        kinds,
        [FailureDecisionKind::Revision, FailureDecisionKind::Abort]
    );
}

fn visual_mut(brief: &mut ReviewBrief) -> &mut SpecificationVisual {
    match &mut brief.visual {
        StageVisual::Specification(visual) => visual,
        other => panic!("not a Specification brief: {other:?}"),
    }
}

fn context(manifest: &BatchManifest) -> BriefContext<'_> {
    BriefContext {
        manifest,
        specification: None,
        checks: &[],
        publications: &[],
    }
}

// ---------------------------------------------------------------------------
// Task-scoped criterion ids (ADR-027 M3 integration)
// ---------------------------------------------------------------------------

fn task_local_criterion_manifest() -> BatchManifest {
    use crate::feature_forge::{AcceptanceCriterion, TaskDraft, TaskKind, WorkTask};

    BatchManifest {
        schema_version: 1,
        title: "Task-scoped criteria".into(),
        tasks: [("t-core", "Task A"), ("t-ui", "Task B")]
            .into_iter()
            .map(|(id, title)| WorkTask {
                id: id.into(),
                project_id: "project".into(),
                revision: 1,
                draft: TaskDraft {
                    title: title.into(),
                    kind: TaskKind::Feature,
                    description: format!("Implement {title}."),
                    criteria: vec![AcceptanceCriterion {
                        id: "ac-1".into(),
                        description: format!("Acceptance for {title}."),
                    }],
                    dependencies: Vec::new(),
                    feature_id: None,
                },
                feature_revision: None,
                state: "ready".into(),
                active_pulsation_id: None,
                completed_pulsation_id: None,
            })
            .collect(),
        features: Vec::new(),
        feature_dependencies: Vec::new(),
        path_confirmation_source: Default::default(),
    }
}

fn task_local_criterion_spec() -> SpecificationContractV1 {
    let mut specification = spec();
    specification.requirements = vec![
        requirement("task_1", "crit_1_1", VerificationMode::ExecutedCheck, None),
        requirement("task_2", "crit_2_1", VerificationMode::DocumentCheck, None),
    ];
    specification.unit_proposals[0].criterion_refs = vec![h("crit_1_1")];
    specification
}

#[test]
fn unchanged_task_local_criterion_ids_do_not_create_cross_task_changes() {
    let manifest = task_local_criterion_manifest();
    let record = record(task_local_criterion_spec());
    let brief = stage_brief(&record, &context(&manifest)).unwrap().unwrap();
    assert_eq!(visual(&brief).tasks[0].criteria[0].criterion_id, "ac-1");
    assert_eq!(visual(&brief).tasks[1].criteria[0].criterion_id, "ac-1");
    assert!(compare(&brief, &brief).is_empty());
    assert!(
        compare_records(&record, &context(&manifest), &record, &context(&manifest))
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_changed_or_removed_task_local_criterion_belongs_only_to_its_task() {
    let manifest = task_local_criterion_manifest();
    let specification = task_local_criterion_spec();
    let previous_record = record(specification.clone());
    let previous = stage_brief(&previous_record, &context(&manifest))
        .unwrap()
        .unwrap();
    let mut revised = specification;
    revised.requirements[1].requirement = "Only Task B requires the export button.".into();
    let current_record = record(revised);
    let current = stage_brief(&current_record, &context(&manifest))
        .unwrap()
        .unwrap();
    let expected = vec![
        BriefChange::new(
            ChangeKind::Changed,
            ChangeSubject::Criterion,
            "Task B · ac-1",
        )
        .field("requirement")
        .before("Requirement for crit_2_1.")
        .after("Only Task B requires the export button."),
    ];
    assert_eq!(compare(&previous, &current), expected);
    assert_eq!(
        compare_records(
            &previous_record,
            &context(&manifest),
            &current_record,
            &context(&manifest)
        )
        .unwrap()
        .unwrap(),
        expected
    );

    let mut removed = previous.clone();
    visual_mut(&mut removed).tasks[1].criteria.clear();
    assert_eq!(
        compare(&previous, &removed),
        [BriefChange::new(
            ChangeKind::Removed,
            ChangeSubject::Criterion,
            "Task B · ac-1"
        )
        .before("Requirement for crit_2_1.")]
    );
    assert_eq!(
        compare(&removed, &previous),
        [
            BriefChange::new(ChangeKind::Added, ChangeSubject::Criterion, "Task B · ac-1")
                .after("Requirement for crit_2_1.")
        ]
    );
}

#[test]
fn typed_criterion_handles_keep_duplicate_ids_in_their_own_work_units() {
    let manifest = task_local_criterion_manifest();
    let specification = task_local_criterion_spec();
    let previous_record = record(specification.clone());
    let mut revised = specification;
    revised.unit_proposals[1].unit_key = h("ui_button");
    let current_record = record(revised);
    let changes = compare_records(
        &previous_record,
        &context(&manifest),
        &current_record,
        &context(&manifest),
    )
    .unwrap()
    .unwrap();
    let criterion_changes: Vec<_> = changes
        .into_iter()
        .filter(|change| change.subject == ChangeSubject::Criterion)
        .collect();
    assert_eq!(
        criterion_changes,
        [
            BriefChange::new(ChangeKind::Moved, ChangeSubject::Criterion, "Task B · ac-1")
                .field("work units")
                .before("ui_export")
                .after("ui_button")
        ]
    );

    // The public display labels cannot identify which task owns `ac-1`.
    // Without the records, report the added/removed unit without guessing
    // that both tasks' identically named criteria moved with it.
    let previous = stage_brief(&previous_record, &context(&manifest))
        .unwrap()
        .unwrap();
    let current = stage_brief(&current_record, &context(&manifest))
        .unwrap()
        .unwrap();
    let view_changes = compare(&previous, &current);
    assert_eq!(view_changes.len(), 2);
    assert!(
        view_changes
            .iter()
            .all(|change| change.subject == ChangeSubject::Unit)
    );
}

#[test]
fn reordering_set_associations_does_not_change_a_specification_brief() {
    use crate::feature_forge::AcceptanceCriterion;
    use crate::orchestration::composition::Validate as _;

    let mut manifest = task_local_criterion_manifest();
    manifest.tasks[0].draft.criteria = ["AC-1", "AC-2"]
        .into_iter()
        .map(|id| AcceptanceCriterion {
            id: id.into(),
            description: format!("Acceptance for {id}."),
        })
        .collect();
    manifest.tasks[1].draft.criteria[0].id = "AC-3".into();
    let mut specification = spec();
    specification.requirements[0].required_check_refs = vec![h("check_a"), h("check_b")];
    specification.unit_proposals[0].task_refs = vec![h("task_1"), h("task_2")];
    specification.unit_proposals[0].criterion_refs =
        vec![h("crit_1_1"), h("crit_1_2"), h("crit_2_1")];
    let mut test_unit = specification.unit_proposals[1].clone();
    test_unit.unit_key = h("ui_checks");
    test_unit.kind = ProposalKind::Test;
    test_unit.depends_on = vec![h("core_export"), h("ui_export")];
    specification.unit_proposals.push(test_unit);
    specification.validate().unwrap();
    let previous_record = record(specification.clone());

    // Keep the same requirements, graph and unit coverage, changing only
    // the order in which their set-valued associations were returned.
    for requirement in &mut specification.requirements {
        requirement.required_check_refs.reverse();
    }
    for unit in &mut specification.unit_proposals {
        unit.destination_refs.reverse();
        unit.depends_on.reverse();
    }
    specification.unit_proposals.reverse();
    specification.validate().unwrap();
    let current_record = record(specification);

    assert!(
        compare_records(
            &previous_record,
            &context(&manifest),
            &current_record,
            &context(&manifest)
        )
        .unwrap()
        .unwrap()
        .is_empty()
    );
    let previous = stage_brief(&previous_record, &context(&manifest))
        .unwrap()
        .unwrap();
    let current = stage_brief(&current_record, &context(&manifest))
        .unwrap()
        .unwrap();
    assert!(compare(&previous, &current).is_empty());
}

#[test]
fn distinct_raw_sets_still_change_when_their_normal_display_collides() {
    use crate::orchestration::composition::Validate as _;

    let mut specification = spec();
    specification.unit_proposals[0].unit_key = h("none");
    specification.unit_proposals[1].depends_on.clear();
    specification.validate().unwrap();
    let previous = brief_of(specification.clone());
    specification.unit_proposals[1].depends_on = vec![h("none")];
    specification.validate().unwrap();
    let current = brief_of(specification);
    assert_eq!(
        compare(&previous, &current),
        [
            BriefChange::new(ChangeKind::Changed, ChangeSubject::Unit, "ui_export")
                .field("depends on")
                .before("[]")
                .after(r#"["none"]"#)
        ]
    );

    // A path may itself contain a comma. It is still one destination,
    // not the two paths that happen to have the same joined display.
    let mut previous = previous;
    visual_mut(&mut previous).units[1].destinations = vec!["a, b".into()];
    let mut current = previous.clone();
    visual_mut(&mut current).units[1].destinations = vec!["a".into(), "b".into()];
    assert_eq!(
        compare(&previous, &current),
        [
            BriefChange::new(ChangeKind::Changed, ChangeSubject::Unit, "ui_export")
                .field("writes")
                .before(r#"["a, b"]"#)
                .after(r#"["a","b"]"#)
        ]
    );
}

#[test]
fn coverage_and_verdicts_keep_duplicate_criterion_ids_with_their_task() {
    let coverage = |task: &str, kind| BriefCoverage {
        task_id: format!("t-{task}"),
        criterion_id: "ac-1".into(),
        description: format!("Acceptance for Task {task}."),
        task_title: format!("Task {task}"),
        coverage: kind,
        tests: Vec::new(),
        declared_checks: Vec::new(),
        explanation: format!("Coverage of Task {task}."),
    };
    let test_brief = |b_kind| ReviewBrief {
        stage: PulsationStage::Test,
        next_stage: Some(PulsationStage::Integrator),
        objective: None,
        counts: Vec::new(),
        revision: None,
        judgement: Vec::new(),
        verified: Vec::new(),
        visual: StageVisual::Test(TestVisual {
            coverage: vec![coverage("A", CoverageKind::Authored), coverage("B", b_kind)],
            checks: Vec::new(),
            files: Vec::new(),
        }),
    };
    assert!(
        compare(
            &test_brief(CoverageKind::Authored),
            &test_brief(CoverageKind::Authored)
        )
        .is_empty()
    );
    let changes: Vec<String> = compare(
        &test_brief(CoverageKind::Authored),
        &test_brief(CoverageKind::NotApplicable),
    )
    .iter()
    .map(line)
    .collect();
    assert_eq!(
        changes,
        ["Changed Criterion Task B · ac-1 coverage | authored -> not applicable"]
    );

    let verdict = |task: &str, verdict| BriefVerdict {
        task_id: format!("t-{task}"),
        criterion_id: "ac-1".into(),
        description: format!("Acceptance for Task {task}."),
        task_title: format!("Task {task}"),
        verdict,
        checks: Vec::new(),
        explanation: "Reviewed.".into(),
    };
    let integrator_brief = |verdicts| ReviewBrief {
        stage: PulsationStage::Integrator,
        next_stage: None,
        objective: None,
        counts: Vec::new(),
        revision: None,
        judgement: Vec::new(),
        verified: Vec::new(),
        visual: StageVisual::Integrator(IntegratorVisual {
            verdicts,
            blockers: Vec::new(),
            checks: Vec::new(),
            publications: Vec::new(),
        }),
    };
    let changes: Vec<String> = compare(
        &integrator_brief(vec![
            verdict("A", Verdict::Supported),
            verdict("B", Verdict::Supported),
        ]),
        &integrator_brief(vec![verdict("A", Verdict::Supported)]),
    )
    .iter()
    .map(line)
    .collect();
    assert_eq!(
        changes,
        ["Removed Criterion Task B · ac-1 - | supported: Reviewed. -> -"]
    );
}

// ---------------------------------------------------------------------------
// Visualizer contract (ADR-027 M5)
// ---------------------------------------------------------------------------

#[test]
fn the_visualizer_input_names_every_judgement_item_and_reference_once() {
    let brief = brief_of(spec());
    let input = visualizer_input(&brief);
    assert_eq!(input.stage, "Specification");
    assert_eq!(
        input.objective.as_deref(),
        Some("Export token usage to CSV.")
    );
    let items: Vec<(&str, &str)> = input
        .judgement
        .iter()
        .map(|item| (item.reference.as_str(), item.label.as_str()))
        .collect();
    assert_eq!(
        items,
        [
            ("J1", "D1"),
            ("J2", "AC-2"),
            ("J3", "AC-3"),
            ("J4", "crates/hvc-ui/src/usage.rs"),
            ("J5", "2"),
        ]
    );
    let references: Vec<(&str, &str, &str)> = input
        .references
        .iter()
        .map(|item| {
            (
                item.reference.as_str(),
                item.names.as_str(),
                item.label.as_str(),
            )
        })
        .collect();
    assert_eq!(
        references,
        [
            ("R1", "criterion", "AC-1"),
            ("R2", "criterion", "AC-2"),
            ("R3", "criterion", "AC-3"),
            ("R4", "file", "README.md"),
            ("R5", "file", "crates/hvc-core/src/usage.rs"),
            ("R6", "file", "crates/hvc-core/src/usage/export.rs"),
            ("R7", "file", "crates/hvc-ui/src/usage.rs"),
            ("R8", "unit", "core_export"),
            ("R9", "unit", "ui_export"),
        ]
    );
    assert!(input.changes.is_empty() && input.feedback.is_none());
}

fn visualizer_reply(value: serde_json::Value) -> String {
    value.to_string()
}

#[test]
fn a_valid_visualization_resolves_its_references_to_brief_labels() {
    let brief = brief_of(spec());
    let input = visualizer_input(&brief);
    let raw = visualizer_reply(serde_json::json!({
        "headline": "Two criteria rest on reading and an exemption; check AC-2 first.",
        "notes": [
            {"text": "AC-2 is proven only by reading the early return.", "refs": ["J2", "R6"]},
            {"text": "The Usage view changes shape for the export button.", "refs": ["J4"]}
        ],
        "emphasis": ["J2"]
    }));
    let validated = validate_visualization(&raw, &input).unwrap();
    let resolved = resolve_visualization(&validated, &input);
    assert_eq!(
        resolved.notes[0].refs,
        ["AC-2", "crates/hvc-core/src/usage/export.rs"]
    );
    assert_eq!(resolved.notes[1].refs, ["crates/hvc-ui/src/usage.rs"]);
    assert_eq!(resolved.emphasis, [1]);
    // A fenced answer is accepted; the fence is not content.
    let fenced = format!("```json\n{raw}\n```");
    assert_eq!(validate_visualization(&fenced, &input).unwrap(), validated);
}

#[test]
fn the_host_rejects_a_visualization_that_breaks_the_contract() {
    let input = visualizer_input(&brief_of(spec()));
    let reject =
        |value: serde_json::Value| validate_visualization(&value.to_string(), &input).unwrap_err();
    let note = |text: &str, refs: &[&str]| serde_json::json!({"text": text, "refs": refs});
    assert!(
        reject(serde_json::json!({"headline": "x".repeat(141), "notes": [], "emphasis": []}))
            .contains("limit is 140")
    );
    assert!(
        reject(serde_json::json!({"headline": "Fine.", "notes": [note(&"y".repeat(221), &["J1"])], "emphasis": []}))
            .contains("limit is 220")
    );
    assert!(
        reject(serde_json::json!({"headline": "Fine.", "notes": [note("A.", &["J1"]), note("B.", &["J1"]), note("C.", &["J1"]), note("D.", &["J1"])], "emphasis": []}))
            .contains("limit is 3")
    );
    assert!(
        reject(serde_json::json!({"headline": "Fine.", "notes": [note("Cites nothing.", &[])], "emphasis": []}))
            .contains("cites no reference")
    );
    assert!(
        reject(serde_json::json!({"headline": "Fine.", "notes": [note("Unknown.", &["R99"])], "emphasis": []}))
            .contains("`R99`, which is not in the input")
    );
    assert!(
        reject(serde_json::json!({"headline": "Fine.", "notes": [], "emphasis": ["R1"]}))
            .contains("not a judgement item")
    );
    assert!(
        reject(serde_json::json!({"headline": "**Bold** headline", "notes": [], "emphasis": []}))
            .contains("without markup")
    );
    assert!(
        reject(
            serde_json::json!({"headline": "Revise a especificação.", "notes": [], "emphasis": []})
        )
        .contains("headline")
    );
    assert!(
        reject(serde_json::json!({"headline": "Fine.", "notes": [], "emphasis": [], "verdict": "approve"}))
            .contains("unknown field")
    );
}
