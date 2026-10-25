//! What the Integrator reviewed (ADR-032): the host's record of the
//! reviewer's classified inputs, its evidence gaps and the review policy, and
//! the notice shown when a pulsation's pipeline predates the policy. Every
//! state and count here is the host's; this module only draws them.

use gpui::{AnyElement, SharedString, div, prelude::*, px};
use hvc_core::orchestration::composition::review_boundary::{
    BaseState, Binding, CheckpointReviewInputs, DiffState, EvidenceClass, ReviewInputManifestV1,
};
use hvc_core::orchestration::composition::review_policy::{PolicyFileState, PolicyState};

use super::stages::{badge, detail, row_head, wide_card};
use super::{Provenance, card_header, mono, pill};
use crate::icons::Icon;
use crate::theme;
use crate::widgets::icon;

/// The review-inputs card, or the legacy notice, for an Integrator checkpoint.
pub(crate) fn review_inputs_panel(inputs: &CheckpointReviewInputs) -> AnyElement {
    match inputs {
        CheckpointReviewInputs::LegacyPipeline => legacy_notice(),
        CheckpointReviewInputs::Recorded {
            manifest,
            changed_policy_files,
        } => review_inputs_card(manifest, changed_policy_files),
    }
}

/// Earlier pipelines review without the classified inputs or the policy.
pub(crate) fn legacy_notice() -> AnyElement {
    div()
        .debug_selector(|| "review-inputs-legacy".into())
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(theme::text_muted())
        .child(icon(Icon::Info, px(14.0), theme::text_muted()))
        .child("Review policy not applied (legacy pipeline): this pulsation predates pipeline v4, so the reviewer's inputs were not classified or recorded.")
        .into_any_element()
}

fn class_name(class: EvidenceClass) -> &'static str {
    match class {
        EvidenceClass::BaselineContext => "context",
        EvidenceClass::Candidate => "candidate",
        EvidenceClass::StageDocument => "stage document (not delivered)",
    }
}

fn policy_word(state: PolicyState) -> (Icon, gpui::Rgba, &'static str) {
    match state {
        PolicyState::Absent => (Icon::Info, theme::text_muted(), "No REVIEW.md"),
        PolicyState::Loaded => (Icon::ShieldCheck, theme::success(), "Loaded"),
        PolicyState::Partial => (Icon::TriangleAlert, theme::warning(), "Partly loaded"),
        PolicyState::Invalid => (Icon::TriangleAlert, theme::warning(), "Unreadable"),
        PolicyState::OverBudget => (Icon::TriangleAlert, theme::warning(), "Over budget"),
    }
}

fn candidate_line(
    base: BaseState,
    binding: Binding,
    diff: DiffState,
) -> (Icon, gpui::Rgba, String) {
    let base = match base {
        BaseState::Absent => "created",
        BaseState::Present => "modified",
        BaseState::PresentUnstored => "modified, base not stored",
        BaseState::Unknown => "base unknown",
    };
    let diff = match diff {
        DiffState::Included => "diff shown",
        DiffState::Truncated => "diff cut at the budget",
        DiffState::Created => "full file shown",
        DiffState::Unchanged => "unchanged",
        DiffState::TooLarge => "too large to diff",
        DiffState::Unavailable => "no diff",
        DiffState::OverBudget => "diff over budget",
    };
    match binding {
        Binding::Fresh => (Icon::Check, theme::success(), format!("{base} · {diff}")),
        Binding::ChangedOutsidePipeline => (
            Icon::TriangleAlert,
            theme::danger(),
            format!("{base} · changed after publication"),
        ),
        Binding::Unreadable => (
            Icon::TriangleAlert,
            theme::danger(),
            format!("{base} · unreadable now"),
        ),
    }
}

fn section(title: &'static str) -> AnyElement {
    div()
        .pt_1()
        .text_xs()
        .text_color(theme::text_muted())
        .child(title)
        .into_any_element()
}

fn review_inputs_card(manifest: &ReviewInputManifestV1, changed: &[String]) -> AnyElement {
    let delivered = manifest.sources.iter().filter(|source| source.delivered);
    let mut counts = std::collections::BTreeMap::<&'static str, usize>::new();
    for source in &manifest.sources {
        *counts.entry(class_name(source.class)).or_default() += 1;
    }
    let (policy_glyph, policy_ink, policy_label) = policy_word(manifest.policy.state);
    let limiting = manifest.limiting_gaps().count();
    let scope = if manifest.tool_scope == "host_context_only" {
        "Host-selected context only: no workspace reads"
    } else {
        "Workspace snapshot"
    };
    wide_card("review-inputs")
        .child(card_header(
            Icon::GitCompare,
            theme::accent(),
            "What the reviewer received",
            Some(delivered.count()),
            Provenance::Host,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(counts.into_iter().map(|(class, count)| pill(format!("{count} {class}"))))
                .child(pill(format!("{} author claim(s)", manifest.author_claims)))
                .child(pill(format!("{} check(s)", manifest.checks.len())))
                .child(pill(scope)),
        )
        .child(section("Review policy"))
        .child(row_head(
            "review-inputs-policy".into(),
            "REVIEW.md",
            badge(policy_glyph, policy_ink, policy_label),
        ))
        .children(manifest.policy.files.iter().map(|file| {
            let state = match file.state {
                PolicyFileState::Loaded => "delivered",
                PolicyFileState::OverBudget => "not delivered: over budget",
                PolicyFileState::Invalid => "not delivered: unreadable",
            };
            let sha = file
                .sha256
                .as_deref()
                .map(|sha| format!(" · {}", &sha[..sha.len().min(8)]))
                .unwrap_or_default();
            // ADR-034 M3: a review under the Intake capture says so.
            let changed = match (changed.contains(&file.path), &manifest.review_policy_snapshot_sha256) {
                (false, _) => "",
                (true, Some(_)) => " · changed since intake",
                (true, None) => " · changed since review",
            };
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(mono(file.path.clone()))
                .child(detail(format!("{state}{sha}{changed}")))
        }))
        .child(section("Candidate files (base → candidate)"))
        .when(manifest.candidates.is_empty(), |card| {
            card.child(detail("No file was published by Code or Test in this pulsation."))
        })
        // Path on its own line: long paths and a two-part state would both
        // wrap side by side in a narrow window.
        .children(manifest.candidates.iter().enumerate().map(|(index, candidate)| {
            let (glyph, ink, word) =
                candidate_line(candidate.base, candidate.binding, candidate.diff);
            let selector = format!("review-inputs-candidate-{index}");
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(mono(candidate.path.clone()))
                .child(badge(glyph, ink, word))
        }))
        .child(section("Evidence gaps"))
        .when(manifest.gaps.is_empty(), |card| {
            card.child(detail("None: the host established every input it delivered."))
        })
        .when(limiting > 0, |card| {
            card.child(
                div()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(format!(
                        "{limiting} gap(s) limit verdicts: the host turned any `supported` resting on them into `unverified`."
                    )),
            )
        })
        .children(manifest.gaps.iter().enumerate().map(|(index, gap)| {
            let (glyph, ink) = if gap.limits_verdicts() {
                (Icon::TriangleAlert, theme::warning())
            } else {
                (Icon::Info, theme::text_muted())
            };
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(row_head(
                    format!("review-inputs-gap-{index}"),
                    gap.code.as_str(),
                    badge(
                        glyph,
                        ink,
                        if gap.limits_verdicts() {
                            "limits verdicts"
                        } else if gap.code.downgrades() {
                            "no criterion depends on it"
                        } else {
                            "shown only"
                        },
                    ),
                ))
                .child(detail(SharedString::from(gap.subject.clone())))
                .when(!gap.affected_criteria.is_empty(), |row| {
                    row.child(detail(format!(
                        "Affects {}",
                        gap.affected_criteria
                            .iter()
                            .map(|handle| handle.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )))
                })
        }))
        .into_any_element()
}
