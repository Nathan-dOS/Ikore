//! The brief of a stage-failure checkpoint (ADR-021): what each decision the
//! host permits will do. The decision itself is the judgement; the effects
//! below restate what `resolve_stage_failure` does, nothing more.

use serde::Serialize;

use crate::persistence_service::{PulsationStage, StageFailureCheckpoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureDecisionKind {
    /// Grant a pending scope request and retry (ADR-035 M4).
    GrantScope,
    Retry,
    Revision,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureChoice {
    pub decision: FailureDecisionKind,
    /// The stage a revision runs again from.
    pub target: Option<PulsationStage>,
    /// What choosing it does, in host words.
    pub effect: String,
}

fn stage_name(stage: PulsationStage) -> &'static str {
    match stage {
        PulsationStage::Intake => "Intake",
        PulsationStage::Specification => "Specification",
        PulsationStage::Contract => "Contract",
        PulsationStage::Code => "Code",
        PulsationStage::Test => "Test",
        PulsationStage::Integrator => "Integrator",
    }
}

/// The permitted decisions in the order the decision bar shows them:
/// retry, each revision target, abort (always permitted).
pub fn failure_choices(checkpoint: &StageFailureCheckpoint) -> Vec<FailureChoice> {
    let stage = stage_name(checkpoint.stage);
    let mut choices = Vec::new();
    if let Some(request) = checkpoint
        .scope_request
        .as_ref()
        .filter(|request| request.state == "pending")
    {
        choices.push(FailureChoice {
            decision: FailureDecisionKind::GrantScope,
            target: None,
            effect: format!(
                "Gives unit `{}` write access to the {} file(s) it asked for, under a new mandate revision, and runs the failed {stage} job once more. The stage budget is not renewed; units already validated are reused without a call. Nothing is granted if a file changed since the request.",
                request.unit_key,
                request.accepted.len()
            ),
        });
    }
    if checkpoint.allow_retry {
        let mut effect = format!(
            "Runs the failed {stage} job once more. The automatic attempts stay spent, so another failure reopens this checkpoint."
        );
        if checkpoint.stage_execution_id.is_some()
            && let Some(left) = checkpoint.remaining_extra_invocations
        {
            effect.push_str(&format!(
                " It spends one of the {left} extra {} the stage has left.",
                if left == 1 { "permit" } else { "permits" }
            ));
        }
        choices.push(FailureChoice {
            decision: FailureDecisionKind::Retry,
            target: None,
            effect,
        });
    }
    if checkpoint.allow_revision {
        let targets = if checkpoint.allowed_revision_targets.is_empty() {
            checkpoint.revision_stage.into_iter().collect()
        } else {
            checkpoint.allowed_revision_targets.clone()
        };
        for target in targets {
            let from = stage_name(target);
            choices.push(FailureChoice {
                decision: FailureDecisionKind::Revision,
                target: Some(target),
                effect: format!(
                    "Supersedes the artifacts from {from} onward and runs {from} again. The stage receives your guidance followed by the host's error code `{}`; without guidance it receives only the error.",
                    checkpoint.error_code
                ),
            });
        }
    }
    choices.push(FailureChoice {
        decision: FailureDecisionKind::Abort,
        target: None,
        effect: "Ends the pulsation as failed. Pending publications are cancelled and effects already applied are marked for review.".to_owned(),
    });
    choices
}
