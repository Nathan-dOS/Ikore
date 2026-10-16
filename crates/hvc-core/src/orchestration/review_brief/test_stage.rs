//! The Test brief: each criterion's declared coverage next to what the host
//! executed. A declaration never reads as a test that ran.

use std::collections::BTreeMap;

use super::{
    Approved, BriefCoverage, BriefFile, JudgementItem, JudgementKind, ReviewBrief, StageVisual,
    TestVisual, batch_criteria, brief_checks, checks_count, checks_verified, file_groups, path_of,
    plural, split_count, split_root, unexecuted_checks,
};
use crate::orchestration::composition::recipes::StagePayloadRecord;
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::{
    CheckSummaryV1, CompletedPayloadV1, CoverageKind, EditOperationV1, Handle, TestCoverageV1,
    UnitModelResponseV1,
};
use crate::persistence_service::PulsationStage;

pub(super) fn build(
    record: &StagePayloadRecord,
    table: &ReferenceTable,
    approved: Approved<'_>,
    checks: &[CheckSummaryV1],
) -> Option<ReviewBrief> {
    // Every unit's coverage, with the paths its artifact keys wrote.
    let mut declared: BTreeMap<Handle, (&TestCoverageV1, Vec<String>)> = BTreeMap::new();
    let mut written: BTreeMap<String, bool> = BTreeMap::new();
    let mut any = false;
    for response in record.responses() {
        let UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Tests(tests),
            ..
        } = response
        else {
            continue;
        };
        any = true;
        let target_of = |key: &Handle| {
            tests
                .edits
                .iter()
                .find(|entry| &entry.artifact_key == key)
                .map(|entry| path_of(record, entry.edit.target()))
        };
        for entry in &tests.edits {
            let created = matches!(entry.edit, EditOperationV1::CreateFile { .. });
            written
                .entry(path_of(record, entry.edit.target()))
                .and_modify(|planned| *planned |= created)
                .or_insert(created);
        }
        for coverage in &tests.coverage {
            let files = match coverage.coverage {
                CoverageKind::Authored => coverage
                    .test_artifact_keys
                    .iter()
                    .filter_map(target_of)
                    .collect(),
                CoverageKind::Existing => coverage
                    .test_input_refs
                    .iter()
                    .map(|handle| path_of(record, handle))
                    .collect(),
                CoverageKind::NotApplicable => Vec::new(),
            };
            declared.insert(coverage.criterion_ref.clone(), (coverage, files));
        }
    }
    if !any {
        return None;
    }

    let coverage: Vec<BriefCoverage> = batch_criteria(table)
        .into_iter()
        .filter_map(|(handle, task_title, criterion_id, description)| {
            let (item, tests) = declared.get(&handle)?;
            Some(BriefCoverage {
                criterion_id,
                description,
                task_title,
                coverage: item.coverage,
                tests: tests.clone(),
                declared_checks: item.check_refs.iter().map(ToString::to_string).collect(),
                explanation: item.explanation.clone(),
            })
        })
        .collect();
    let checks = brief_checks(checks);
    let files = file_groups(written.into_iter().map(|(path, planned)| {
        let (root, relative) = split_root(&path);
        (
            root,
            BriefFile {
                path: relative,
                planned,
                interfaces: Vec::new(),
            },
        )
    }));

    let mut judgement: Vec<JudgementItem> = coverage
        .iter()
        .filter_map(|row| {
            let kind = match row.coverage {
                CoverageKind::Authored => return None,
                CoverageKind::Existing => JudgementKind::ExistingTestsOnly,
                CoverageKind::NotApplicable => JudgementKind::NotApplicable,
            };
            Some(JudgementItem {
                kind,
                label: row.criterion_id.clone(),
                text: row.description.clone(),
                detail: Some(row.explanation.clone()),
                refs: row.tests.clone(),
            })
        })
        .collect();
    judgement.extend(unexecuted_checks(&checks));

    let count = |kind: CoverageKind| coverage.iter().filter(|row| row.coverage == kind).count();
    let file_count: usize = files.iter().map(|group| group.files.len()).sum();
    let created = files
        .iter()
        .flat_map(|group| &group.files)
        .filter(|file| file.planned)
        .count();
    let counts = vec![
        split_count(
            plural(coverage.len(), "criterion", "criteria"),
            &[
                (count(CoverageKind::Authored), "authored"),
                (count(CoverageKind::Existing), "existing"),
                (count(CoverageKind::NotApplicable), "not applicable"),
            ],
        ),
        checks_count(&checks),
        format!(
            "{} · {created} new",
            plural(file_count, "test file", "test files")
        ),
    ];

    let mut verified = vec!["Each criterion in scope has exactly one coverage entry.".to_owned()];
    if count(CoverageKind::Authored) > 0 {
        verified.push("Authored coverage names test files this stage wrote.".to_owned());
    }
    if count(CoverageKind::Existing) > 0 {
        verified
            .push("Existing coverage names test files that were delivered and read.".to_owned());
    }
    if count(CoverageKind::NotApplicable) > 0 {
        verified.push(
            "Not-applicable coverage exempts only criteria the specification approved as not applicable."
                .to_owned(),
        );
    }
    if coverage.iter().any(|row| !row.declared_checks.is_empty()) {
        verified.push("Every check it declares is configured for this project.".to_owned());
    }
    checks_verified(&checks, &mut verified);

    Some(ReviewBrief {
        stage: PulsationStage::Test,
        next_stage: Some(PulsationStage::Integrator),
        objective: approved.map(|(spec, _)| spec.objective.clone()),
        counts,
        revision: None,
        judgement,
        verified,
        visual: StageVisual::Test(TestVisual {
            coverage,
            checks,
            files,
        }),
    })
}
