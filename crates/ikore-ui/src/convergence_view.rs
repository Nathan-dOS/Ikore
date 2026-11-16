//! Revision rounds at a checkpoint (ADR-032 M4).
//!
//! The host counts human revision rounds per input revision and refuses a
//! plain Request revision beyond the project's threshold. This module draws
//! the count and, beyond the threshold, the two explicit decisions that
//! replace the plain revision button. The reason is the feedback text.

use gpui::{AnyElement, Context, div, prelude::*};
use ikore_core::persistence_service::ConvergenceStatus;

use crate::app_model::AppModel;
use crate::theme;
use crate::widgets::{ButtonVariant, btn};

/// "k of T revision rounds used · N technical attempts", for pipeline v4.
pub(crate) fn round_line(status: &ConvergenceStatus) -> Option<AnyElement> {
    if !status.applies {
        return None;
    }
    let reached = status.threshold_reached();
    Some(
        div()
            .debug_selector(|| "convergence-rounds".into())
            .text_xs()
            .text_color(if reached {
                theme::warning()
            } else {
                theme::text_muted()
            })
            .child(format!(
                "Revision rounds: {} of {} · {} technical attempt(s){}",
                status.rounds,
                status.threshold,
                status.technical_attempts,
                if reached {
                    " · each further revision needs a decision"
                } else {
                    ""
                }
            ))
            .into_any_element(),
    )
}

/// The explicit decisions beyond the threshold. `failure` selects the
/// failure checkpoint's handlers instead of the artifact review's.
pub(crate) fn threshold_buttons(
    enabled: bool,
    failure: bool,
    cx: &mut Context<AppModel>,
) -> Vec<AnyElement> {
    let (continue_id, replan_id) = if failure {
        ("failure-continue-revision", "failure-replan")
    } else {
        ("hitl-continue-revision", "hitl-replan")
    };
    vec![
        btn(
            continue_id,
            "Continue revising",
            ButtonVariant::Secondary,
            enabled,
            cx,
            move |model, cx| {
                if failure {
                    model.revise_failure_beyond_threshold(false, cx);
                } else {
                    model.revise_beyond_threshold(false, cx);
                }
            },
        ),
        btn(
            replan_id,
            "Replan from Specification",
            ButtonVariant::Secondary,
            enabled,
            cx,
            move |model, cx| {
                if failure {
                    model.revise_failure_beyond_threshold(true, cx);
                } else {
                    model.revise_beyond_threshold(true, cx);
                }
            },
        ),
    ]
}
