//! The Integrator brief: a verdict per criterion, the blockers, the host
//! checks with their receipts, and the files approval publishes.

use super::{
    Approved, BriefPublication, BriefVerdict, IntegratorVisual, JudgementItem, JudgementKind,
    ReviewBrief, StageVisual, batch_criteria, brief_checks, checks_count, checks_verified, plural,
    split_count, unexecuted_checks,
};
use crate::orchestration::PublicationState;
use crate::orchestration::composition::recipes::StagePayloadRecord;
use crate::orchestration::composition::references::ReferenceTable;
use crate::orchestration::composition::{
    CheckSummaryV1, CompletedPayloadV1, CriterionAssessmentV1, UnitModelResponseV1, Verdict,
};

pub(super) fn build(
    record: &StagePayloadRecord,
    table: &ReferenceTable,
    approved: Approved<'_>,
    checks: &[CheckSummaryV1],
    publications: &[BriefPublication],
) -> Option<ReviewBrief> {
    let mut assessments: Vec<&CriterionAssessmentV1> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let mut any = false;
    for response in record.responses() {
        let UnitModelResponseV1::Completed {
            payload: CompletedPayloadV1::Integration(integration),
            ..
        } = response
        else {
            continue;
        };
        any = true;
        assessments.extend(integration.assessments.iter());
        blockers.extend(integration.blockers.iter().cloned());
    }
    if !any {
        return None;
    }

    let verdicts: Vec<BriefVerdict> = batch_criteria(table)
        .into_iter()
        .filter_map(|criterion| {
            let assessment = assessments
                .iter()
                .find(|assessment| assessment.criterion_ref == criterion.handle)?;
            Some(BriefVerdict {
                task_id: criterion.task_id,
                criterion_id: criterion.criterion_id,
                description: criterion.description,
                task_title: criterion.task_title,
                verdict: assessment.verdict,
                checks: assessment
                    .check_refs
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                explanation: assessment.explanation.clone(),
            })
        })
        .collect();
    let checks = brief_checks(checks);

    let mut judgement: Vec<JudgementItem> = verdicts
        .iter()
        .filter_map(|verdict| {
            let kind = match verdict.verdict {
                Verdict::Supported => return None,
                Verdict::Contradicted => JudgementKind::Contradicted,
                Verdict::Unverified => JudgementKind::Unverified,
            };
            Some(JudgementItem {
                kind,
                label: verdict.criterion_id.clone(),
                text: verdict.description.clone(),
                detail: Some(verdict.explanation.clone()),
                refs: Vec::new(),
            })
        })
        .collect();
    judgement.extend(
        blockers
            .iter()
            .enumerate()
            .map(|(index, blocker)| JudgementItem {
                kind: JudgementKind::Blocker,
                label: format!("B{}", index + 1),
                text: blocker.clone(),
                detail: None,
                refs: Vec::new(),
            }),
    );
    judgement.extend(unexecuted_checks(&checks));

    let count = |kind: Verdict| verdicts.iter().filter(|row| row.verdict == kind).count();
    let state = |wanted: &[PublicationState]| {
        publications
            .iter()
            .filter(|file| wanted.contains(&file.state))
            .count()
    };
    let mut counts = vec![split_count(
        plural(verdicts.len(), "criterion", "criteria"),
        &[
            (count(Verdict::Supported), "supported"),
            (count(Verdict::Contradicted), "contradicted"),
            (count(Verdict::Unverified), "unverified"),
        ],
    )];
    if !blockers.is_empty() {
        counts.push(plural(blockers.len(), "blocker", "blockers"));
    }
    counts.push(checks_count(&checks));
    counts.push(split_count(
        plural(publications.len(), "file", "files"),
        &[
            (state(&[PublicationState::Applied]), "published"),
            (
                state(&[PublicationState::Pending, PublicationState::Applying]),
                "to publish",
            ),
            (state(&[PublicationState::Conflict]), "in conflict"),
        ],
    ));

    let mut verified = vec!["Each criterion in scope has exactly one verdict.".to_owned()];
    if verdicts.iter().any(|verdict| !verdict.checks.is_empty()) {
        verified.push("Every check a verdict cites is configured for this project.".to_owned());
    }
    checks_verified(&checks, &mut verified);

    Some(ReviewBrief {
        stage: crate::persistence_service::PulsationStage::Integrator,
        next_stage: None,
        objective: approved.map(|(spec, _)| spec.objective.clone()),
        counts,
        revision: None,
        judgement,
        verified,
        visual: StageVisual::Integrator(IntegratorVisual {
            verdicts,
            blockers,
            checks,
            publications: publications.to_vec(),
        }),
    })
}
