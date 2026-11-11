//! The stage visuals after Specification (ADR-027 M4) and the stage-failure brief.
//!
//! Contract shows its interfaces; Test keeps the coverage the agent declared apart from
//! the checks the host executed; Integrator shows a verdict per criterion, the host checks
//! and every file's publication state. A stage failure lists what each permitted decision
//! does. Rows follow the layout rule the M3 diff learned in the running app: fixed words
//! keep their width and only one text per row wraps.

use gpui::{AnyElement, SharedString, div, prelude::*, px};
use ikore_core::orchestration::PublicationState;
use ikore_core::orchestration::composition::{CheckStatus, CoverageKind, GroupIntent, Verdict};
use ikore_core::orchestration::review_brief::{
    BriefCheck, BriefCoverage, BriefFileGroup, BriefInterfaceGroup, BriefPublication, BriefVerdict,
    ContractVisual, FailureDecisionKind, IntegratorVisual, TestVisual, failure_choices,
};
use ikore_core::persistence_service::StageFailureCheckpoint;

use super::{Provenance, card, card_header, file_group, mono, pill, stage_name};
use crate::icons::Icon;
use crate::theme;
use crate::widgets::icon;

/// A full-width card, like the criteria card: rows need the width.
pub(super) fn wide_card(selector: &'static str) -> gpui::Div {
    card(selector).w_full()
}

/// An icon and a word in one colour, never shrinking.
pub(super) fn badge(glyph: Icon, ink: gpui::Rgba, word: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(ink)
        .child(icon(glyph, px(14.0), ink))
        .child(word.into())
        .into_any_element()
}

/// One wrapping line of muted detail.
pub(super) fn detail(text: impl Into<SharedString>) -> AnyElement {
    div()
        .min_w_0()
        .text_xs()
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

pub(super) fn empty(text: &'static str) -> AnyElement {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

/// `key` in bold, a badge, and nothing else on the line.
pub(super) fn row_head(
    selector: String,
    key: &str,
    badge_element: AnyElement,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .min_w_0()
        .flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(SharedString::from(key.to_owned())),
        )
        .child(badge_element)
}

fn listed(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

fn group_look(intent: GroupIntent) -> (Icon, gpui::Rgba, &'static str) {
    match intent {
        GroupIntent::Change => (Icon::Plug, theme::warning(), "Changes"),
        GroupIntent::New => (Icon::Plus, theme::success(), "New"),
        GroupIntent::Preserve => (Icon::Check, theme::text_muted(), "Preserved"),
    }
}

pub(super) fn interfaces_card(visual: &ContractVisual) -> AnyElement {
    let card = wide_card("brief-interfaces").child(card_header(
        Icon::Plug,
        theme::purple(),
        "Interfaces the code will be written against",
        Some(visual.interfaces.len()),
        Provenance::Agent,
    ));
    if visual.interfaces.is_empty() {
        return card
            .child(empty(
                "No new or changed interface is required by the approved specification.",
            ))
            .into_any_element();
    }
    card.children(
        visual
            .interfaces
            .iter()
            .enumerate()
            .map(|(index, group)| interface_group(index, group)),
    )
    .into_any_element()
}

fn interface_group(index: usize, group: &BriefInterfaceGroup) -> AnyElement {
    let (glyph, ink, word) = group_look(group.intent);
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(if group.intent == GroupIntent::Preserve {
            theme::border()
        } else {
            ink
        })
        .bg(theme::canvas())
        .child(row_head(
            format!("brief-interface-{index}"),
            &group.key,
            badge(glyph, ink, word),
        ))
        .child(
            div()
                .min_w_0()
                .px_2()
                .py_1()
                .rounded_sm()
                .bg(theme::surface_raised())
                .child(mono(group.signature.clone())),
        )
        .children(group.invariants.iter().map(|invariant| {
            div()
                .min_w_0()
                .flex()
                .items_start()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme::text_muted())
                        .child("Invariant"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(invariant.clone())),
                )
        }))
        .child(detail(format!(
            "Sources: {} · Criteria: {} · Bound to: {}",
            listed(&group.sources),
            listed(&group.criteria),
            listed(&group.units)
        )))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

fn coverage_look(row: &BriefCoverage) -> (Icon, gpui::Rgba, String) {
    match row.coverage {
        CoverageKind::Authored => (
            Icon::Check,
            theme::success(),
            match row.tests.len() {
                1 => "Authored · 1 test file".to_owned(),
                count => format!("Authored · {count} test files"),
            },
        ),
        CoverageKind::Existing => (
            Icon::CircleHelp,
            theme::warning(),
            "Existing tests only".to_owned(),
        ),
        CoverageKind::NotApplicable => (
            Icon::CircleHelp,
            theme::warning(),
            "Not applicable".to_owned(),
        ),
    }
}

pub(super) fn coverage_card(visual: &TestVisual) -> AnyElement {
    wide_card("brief-coverage")
        .child(card_header(
            Icon::Target,
            theme::purple(),
            "Coverage the agent declared",
            Some(visual.coverage.len()),
            Provenance::Agent,
        ))
        .child(detail(
            "A declaration never means a test ran. What the host executed is in Host checks.",
        ))
        .children(
            visual
                .coverage
                .iter()
                .enumerate()
                .map(|(index, row)| coverage_row(index, row)),
        )
        .into_any_element()
}

fn coverage_row(index: usize, row: &BriefCoverage) -> AnyElement {
    let (glyph, ink, word) = coverage_look(row);
    let attention = row.coverage != CoverageKind::Authored;
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(if attention {
            theme::warning_soft()
        } else {
            theme::canvas()
        })
        .child(row_head(
            format!("brief-coverage-{index}"),
            &row.criterion_id,
            badge(glyph, ink, word),
        ))
        .child(
            div()
                .min_w_0()
                .text_sm()
                .child(SharedString::from(row.description.clone())),
        )
        .child(detail(format!("Task: {}", row.task_title)))
        .when(!row.tests.is_empty(), |item| {
            item.child(detail(format!("Tests: {}", row.tests.join(", "))))
        })
        .when(!row.declared_checks.is_empty(), |item| {
            item.child(detail(format!(
                "Declares the checks: {}",
                row.declared_checks.join(", ")
            )))
        })
        .child(detail(row.explanation.clone()))
        .into_any_element()
}

fn check_look(status: CheckStatus) -> (Icon, gpui::Rgba, &'static str) {
    match status {
        CheckStatus::Passed => (Icon::Check, theme::success(), "Passed"),
        CheckStatus::Failed => (Icon::X, theme::danger(), "Failed"),
        CheckStatus::TimedOut => (Icon::Clock, theme::danger(), "Timed out"),
        CheckStatus::Error => (Icon::X, theme::danger(), "Error"),
        CheckStatus::Cancelled => (Icon::Ban, theme::warning(), "Cancelled"),
        CheckStatus::Unavailable => (Icon::TriangleAlert, theme::warning(), "Unavailable"),
        CheckStatus::NotRun => (Icon::Pause, theme::warning(), "Not run"),
        CheckStatus::Pending => (Icon::Clock, theme::text_muted(), "Pending"),
        CheckStatus::Running => (Icon::Clock, theme::accent(), "Running"),
    }
}

pub(super) fn checks_card(checks: &[BriefCheck]) -> AnyElement {
    let card = card("brief-checks").child(card_header(
        Icon::ShieldCheck,
        theme::accent(),
        "Host checks",
        Some(checks.len()),
        Provenance::Host,
    ));
    if checks.is_empty() {
        return card
            .child(empty(
                "No host check runs for this stage, so nothing here was executed.",
            ))
            .into_any_element();
    }
    card.children(checks.iter().enumerate().map(|(index, check)| {
        let (glyph, ink, word) = check_look(check.status);
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(row_head(
                format!("brief-check-{index}"),
                &check.check_ref,
                badge(glyph, ink, word),
            ))
            .child(detail(match (&check.receipt, check.required) {
                (Some(receipt), true) => format!("Required · receipt {receipt}"),
                (Some(receipt), false) => format!("Optional · receipt {receipt}"),
                (None, true) => "Required · no receipt".to_owned(),
                (None, false) => "Optional · no receipt".to_owned(),
            }))
    }))
    .into_any_element()
}

pub(super) fn test_files_card(files: &[BriefFileGroup]) -> AnyElement {
    let total: usize = files.iter().map(|group| group.files.len()).sum();
    let card = card("brief-test-files").child(card_header(
        Icon::FolderOpen,
        theme::text_muted(),
        "Test files it wrote",
        Some(total),
        Provenance::Host,
    ));
    if files.is_empty() {
        return card
            .child(empty("It wrote no test file."))
            .into_any_element();
    }
    card.children(files.iter().map(file_group))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Integrator
// ---------------------------------------------------------------------------

fn verdict_look(verdict: Verdict) -> (Icon, gpui::Rgba, &'static str) {
    match verdict {
        Verdict::Supported => (Icon::Check, theme::success(), "Supported"),
        Verdict::Contradicted => (Icon::X, theme::danger(), "Contradicted"),
        Verdict::Unverified => (Icon::CircleHelp, theme::warning(), "Unverified"),
    }
}

pub(super) fn verdicts_card(visual: &IntegratorVisual) -> AnyElement {
    wide_card("brief-verdicts")
        .child(card_header(
            Icon::Target,
            theme::purple(),
            "Verdict per criterion",
            Some(visual.verdicts.len()),
            Provenance::Agent,
        ))
        .children(
            visual
                .verdicts
                .iter()
                .enumerate()
                .map(|(index, verdict)| verdict_row(index, verdict)),
        )
        .into_any_element()
}

fn verdict_row(index: usize, row: &BriefVerdict) -> AnyElement {
    let (glyph, ink, word) = verdict_look(row.verdict);
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(match row.verdict {
            Verdict::Supported => theme::canvas(),
            Verdict::Contradicted => theme::danger_soft(),
            Verdict::Unverified => theme::warning_soft(),
        })
        .child(row_head(
            format!("brief-verdict-{index}"),
            &row.criterion_id,
            badge(glyph, ink, word),
        ))
        .child(
            div()
                .min_w_0()
                .text_sm()
                .child(SharedString::from(row.description.clone())),
        )
        .child(detail(format!("Task: {}", row.task_title)))
        .child(detail(row.explanation.clone()))
        .into_any_element()
}

fn publication_look(state: PublicationState) -> (Icon, gpui::Rgba, &'static str) {
    match state {
        PublicationState::Applied => (Icon::Check, theme::success(), "Published"),
        PublicationState::Pending => (Icon::Clock, theme::text_muted(), "Publishes on approval"),
        PublicationState::Applying => (Icon::Clock, theme::accent(), "Publishing"),
        PublicationState::Conflict => (Icon::TriangleAlert, theme::danger(), "Conflict"),
        PublicationState::NotRequired => (Icon::Check, theme::text_muted(), "Not published"),
    }
}

pub(super) fn publications_card(files: &[BriefPublication]) -> AnyElement {
    let card = wide_card("brief-publications").child(card_header(
        Icon::Layers,
        theme::text_muted(),
        "Files and their publication",
        Some(files.len()),
        Provenance::Host,
    ));
    if files.is_empty() {
        return card
            .child(empty("This input revision has no file to publish."))
            .into_any_element();
    }
    card.children(files.iter().enumerate().map(|(index, file)| {
        let (glyph, ink, word) = publication_look(file.state);
        div()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .debug_selector(move || format!("brief-publication-{index}"))
                    .min_w_0()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(mono(file.path.clone())))
                    .child(badge(glyph, ink, word)),
            )
            .child(detail(format!("Written by {}", stage_name(file.stage))))
    }))
    .into_any_element()
}

// ---------------------------------------------------------------------------
// Stage failure
// ---------------------------------------------------------------------------

/// What each decision the host permits will do, above the failure's facts.
pub(crate) fn failure_choices_card(checkpoint: &StageFailureCheckpoint) -> AnyElement {
    let choices = failure_choices(checkpoint);
    div()
        .debug_selector(|| "failure-choices".into())
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::warning_soft())
        .bg(theme::surface())
        .child(card_header(
            Icon::Signpost,
            theme::warning(),
            "What each decision does",
            Some(choices.len()),
            Provenance::Developer,
        ))
        .children(choices.iter().enumerate().map(|(index, choice)| {
            let (glyph, ink, word) = match choice.decision {
                FailureDecisionKind::GrantScope => (
                    Icon::ShieldCheck,
                    theme::accent(),
                    "Grant and retry".to_owned(),
                ),
                FailureDecisionKind::Retry => (Icon::RotateCw, theme::accent(), "Retry".to_owned()),
                FailureDecisionKind::Revision => (
                    Icon::Pencil,
                    theme::warning(),
                    choice
                        .target
                        .map(|target| format!("Revise from {}", stage_name(target)))
                        .unwrap_or_else(|| "Request revision".to_owned()),
                ),
                FailureDecisionKind::Abort => (Icon::X, theme::danger(), "Abort".to_owned()),
            };
            div()
                .debug_selector(move || format!("failure-choice-{index}"))
                .min_w_0()
                .flex()
                .items_start()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(theme::border())
                .bg(theme::canvas())
                .child(
                    div()
                        .w(px(150.0))
                        .flex_shrink_0()
                        .child(badge(glyph, ink, word)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(SharedString::from(choice.effect.clone())),
                )
        }))
        .child(pill("The buttons below act; this list only explains them."))
        .into_any_element()
}
