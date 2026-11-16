//! Developer review of the Feature Agent's proposed defaults.
use super::{
    FeatureBrief, FeatureForgeService,
    service::{
        check_project, editable_feature, feature, invalid, persist_brief_revision, record, replay,
        text,
    },
};
use crate::persistence_service::PersistenceServiceError as Error;
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(crate) const MAX_ASSUMPTIONS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionCategory {
    Scope,
    Security,
    Ux,
    Technical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureAssumption {
    pub id: String,
    pub text: String,
    pub category: AssumptionCategory,
    pub reason: String,
    /// Only the review command sets this field. The provider schema omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<AssumptionReviewAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssumptionReviewAction {
    Flag,
    Answer { text: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewAssumptionRequest {
    pub project_id: String,
    pub feature_id: String,
    pub expected_revision: u64,
    pub assumption_id: String,
    pub request_id: String,
    pub action: AssumptionReviewAction,
}

impl FeatureAssumption {
    fn pending_label(&self) -> String {
        format!("Review assumption {}: {}", self.id, self.text)
    }

    fn same_proposal(&self, other: &Self) -> bool {
        self.id == other.id
            && self.text == other.text
            && self.category == other.category
            && self.reason == other.reason
    }
}

pub(super) fn validate_assumptions(brief: &FeatureBrief) -> Result<(), Error> {
    if brief.assumptions.len() > MAX_ASSUMPTIONS {
        return Err(invalid("A brief may contain up to 32 assumptions"));
    }
    let mut seen = HashSet::new();
    for item in &brief.assumptions {
        if item.id.is_empty()
            || item.id.len() > 64
            || !item
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || !seen.insert(&item.id)
        {
            return Err(invalid(
                "Assumption IDs must be unique letters, digits, dashes or underscores (1–64 bytes)",
            ));
        }
        text(&item.text, 2_000)?;
        text(&item.reason, 2_000)?;
    }
    Ok(())
}

/// Restores only host decisions attached to the identical proposal. An open
/// flag must survive a model turn even if the model drops it from its reply.
pub(super) fn carry_reviews(previous: &FeatureBrief, next: &mut FeatureBrief) -> Result<(), Error> {
    validate_assumptions(next)?;
    for item in &mut next.assumptions {
        item.review = None;
    }
    for prior in &previous.assumptions {
        let Some(review) = &prior.review else {
            continue;
        };
        let matching = next
            .assumptions
            .iter_mut()
            .find(|item| prior.same_proposal(item));
        match (review, matching) {
            (AssumptionReviewAction::Flag, None) => {
                return Err(invalid(
                    "Answer the flagged assumption before replacing or removing it",
                ));
            }
            (_, Some(item)) => item.review = Some(review.clone()),
            (_, None) => {}
        }
        let label = prior.pending_label();
        next.pending_decisions.retain(|decision| decision != &label);
        if *review == AssumptionReviewAction::Flag {
            next.pending_decisions.push(label);
        }
    }
    Ok(())
}

impl FeatureForgeService {
    /// Flag or answer one proposed default under the revision displayed in Forge.
    pub fn review_assumption(&self, request: &ReviewAssumptionRequest) -> Result<u64, Error> {
        let mut db = self.persistence.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let kind = "review_assumption";
        check_project(&tx, &request.project_id)?;
        if let Some(revision) =
            replay(&tx, &request.project_id, &request.request_id, kind, request)?
        {
            return Ok(revision);
        }
        let f = feature(&tx, &request.project_id, &request.feature_id)?;
        editable_feature(&tx, &f, request.expected_revision)?;
        if f.approved_revision == Some(f.revision) {
            return Err(invalid(
                "Revise the approved brief before reviewing its assumptions",
            ));
        }
        if let AssumptionReviewAction::Answer { text: answer } = &request.action {
            text(answer, 2_000)?;
        }
        let mut brief = f.brief;
        let item = brief
            .assumptions
            .iter_mut()
            .find(|a| a.id == request.assumption_id)
            .ok_or_else(|| invalid("The assumption is absent from this revision"))?;
        let revision = if item.review.as_ref() == Some(&request.action) {
            f.revision
        } else {
            let label = item.pending_label();
            item.review = Some(request.action.clone());
            brief
                .pending_decisions
                .retain(|decision| decision != &label);
            if request.action == AssumptionReviewAction::Flag {
                brief.pending_decisions.push(label);
            }
            persist_brief_revision(&tx, &f.id, f.revision, &brief)?;
            f.revision + 1
        };
        record(
            &tx,
            &request.project_id,
            &request.request_id,
            kind,
            request,
            &revision,
        )?;
        tx.commit()?;
        Ok(revision)
    }
}
