//! The derived review brief of a checkpoint (ADR-027 M1–M4).
//!
//! A native rendering of `hvc_core::orchestration::review_brief::ReviewBrief`. The host
//! decided every count, status and rule; this module only draws them. Text an agent wrote
//! (objective, decisions, requirements, interface and unit descriptions) is labelled where
//! it appears, and colour always comes with an icon and a word. Each judgement item can be
//! confirmed or flagged (M2); flags and their notes become the Request revision feedback.
//! After Request revision, a card lists what changed since the previous candidate (M3).
//! Contract, Test and Integrator checkpoints draw their own stage visual, and a stage
//! failure lists what each permitted decision does (M4, `stages`).

mod stages;
mod visualizer;

pub(crate) use stages::failure_choices_card;

use std::collections::HashMap;

use gpui::{AnyElement, Context, Entity, SharedString, div, prelude::*, px};
use hvc_core::orchestration::composition::{InterfaceIntent, ProposalKind, VerificationMode};
use hvc_core::orchestration::review_brief::{
    BriefChange, BriefCriterion, BriefFile, BriefFileGroup, BriefUnit, ChangeKind, ChangeSubject,
    JudgementItem, JudgementKind, ReviewBrief, RevisionDiff, SpecificationVisual, StageVisual,
};
use hvc_core::persistence_service::PulsationStage;

use crate::app_model::AppModel;
use crate::hitl::{ItemMark, judgement_kind_label};
use crate::icons::Icon;
use crate::text_input::NativeTextInput;
use crate::theme;
use crate::widgets::icon;

/// Width under which a two-card row stacks; keeps each card readable at 960 px.
const CARD_MIN_WIDTH: f32 = 320.0;

/// The developer's marks over the brief, as the Review state holds them.
pub(crate) struct BriefMarks<'a> {
    pub(crate) marks: &'a [ItemMark],
    pub(crate) cursor: usize,
    pub(crate) notes: &'a HashMap<usize, Entity<NativeTextInput>>,
    /// False while a decision is being applied or no checkpoint is waiting.
    pub(crate) interactive: bool,
}

/// The Visualizer's note on the same checkpoint, when the project asked for one.
pub(crate) type BriefVisualization<'a> =
    Option<&'a hvc_core::orchestration::review_brief::CheckpointVisualization>;

pub(crate) fn brief_panel(
    brief: &ReviewBrief,
    marks: &BriefMarks<'_>,
    visualization: BriefVisualization<'_>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let emphasis: Vec<usize> = visualization
        .and_then(|visualization| visualization.note.as_ref())
        .map(|note| note.emphasis.clone())
        .unwrap_or_default();
    div()
        .debug_selector(|| "review-brief".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .child(decision_card(brief))
        .when_some(visualization, |panel, visualization| {
            panel.child(visualizer::visualizer_card(visualization))
        })
        .when_some(brief.revision.as_ref(), |panel, diff| {
            panel.child(revision_card(diff))
        })
        .child(
            row()
                .child(judgement_card(&brief.judgement, marks, &emphasis, cx))
                .child(verified_card(&brief.verified)),
        )
        .children(stage_visual(brief))
        .child(div().text_xs().text_color(theme::text_muted()).child(
            "Derived from this checkpoint's validated payload, the frozen task batch and the host's own records. \
                     The documents in the next tabs are the record; approval still requires them.",
        ))
        .into_any_element()
}

/// The stage's own cards, after the shared anatomy.
fn stage_visual(brief: &ReviewBrief) -> Vec<AnyElement> {
    match &brief.visual {
        StageVisual::Specification(visual) => vec![
            criteria_card(visual),
            row()
                .child(files_card(visual))
                .child(units_card(&visual.units))
                .into_any_element(),
        ],
        StageVisual::Contract(visual) => {
            vec![stages::interfaces_card(visual), units_card(&visual.units)]
        }
        StageVisual::Test(visual) => vec![
            stages::coverage_card(visual),
            row()
                .child(stages::checks_card(&visual.checks))
                .child(stages::test_files_card(&visual.files))
                .into_any_element(),
        ],
        // Paths need the width: publications get a row of their own.
        StageVisual::Integrator(visual) => vec![
            stages::verdicts_card(visual),
            stages::publications_card(&visual.publications),
            stages::checks_card(&visual.checks),
        ],
    }
}

/// Shown on the Brief tab when the brief could not be built; the documents are unaffected.
pub(crate) fn brief_error_panel(error: &str) -> AnyElement {
    div()
        .debug_selector(|| "review-brief-error".into())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(theme::warning())
                .child(icon(Icon::TriangleAlert, px(16.0), theme::warning()))
                .child("The brief for this checkpoint could not be built."),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(SharedString::from(error.to_owned())),
        )
        .child(
            div()
                .text_sm()
                .child("The complete documents in the next tabs are unaffected and remain the basis of your decision."),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

fn row() -> gpui::Div {
    // `items_start`: a short card keeps its own height instead of stretching
    // to its neighbour's.
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_start()
        .gap_4()
}

fn card(selector: &'static str) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .flex_1()
        .min_w(px(CARD_MIN_WIDTH))
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
}

#[derive(Clone, Copy)]
enum Provenance {
    Host,
    Agent,
    Developer,
}

fn provenance(kind: Provenance) -> AnyElement {
    let (label, ink, fill) = match kind {
        Provenance::Host => ("HOST-VERIFIED", theme::accent(), theme::accent_soft()),
        Provenance::Agent => ("AGENT-WRITTEN", theme::purple(), theme::purple_soft()),
        Provenance::Developer => ("YOU DECIDE", theme::warning(), theme::warning_soft()),
    };
    div()
        .flex_shrink_0()
        .px_2()
        .rounded_full()
        .bg(fill)
        .text_color(ink)
        .text_xs()
        .child(label)
        .into_any_element()
}

fn card_header(
    glyph: Icon,
    ink: gpui::Rgba,
    title: &'static str,
    count: Option<usize>,
    tag: Provenance,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .child(icon(glyph, px(16.0), ink))
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
        .when_some(count, |header, count| {
            header.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(count.to_string()),
            )
        })
        .child(div().flex_1())
        .child(provenance(tag))
        .into_any_element()
}

fn pill(label: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(1.0))
        .rounded_md()
        .bg(theme::surface_raised())
        .text_xs()
        .text_color(theme::text_muted())
        .child(label.into())
        .into_any_element()
}

fn mono(text: impl Into<SharedString>) -> AnyElement {
    div()
        .min_w_0()
        .font_family(theme::FONT_MONO)
        .text_xs()
        .child(text.into())
        .into_any_element()
}

/// A labelled box for text an agent wrote, so it never reads as a host fact.
fn agent_text(text: &str) -> AnyElement {
    div()
        .min_w_0()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::purple())
                .child("Agent-written"),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(SharedString::from(text.to_owned())),
        )
        .into_any_element()
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

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

/// What approving starts, in the words of the stage under review.
fn headline(brief: &ReviewBrief) -> String {
    let subject = match brief.stage {
        PulsationStage::Specification => "this specification",
        PulsationStage::Contract => "this contract",
        PulsationStage::Test => "these tests",
        PulsationStage::Integrator => "this integration",
        PulsationStage::Intake | PulsationStage::Code => "this stage",
    };
    match brief.next_stage {
        Some(next) => format!(
            "Approve {subject} so the {} stage can start.",
            stage_name(next)
        ),
        None => format!("Approve {subject} to verify the published files and close the pulsation."),
    }
}

fn decision_card(brief: &ReviewBrief) -> AnyElement {
    let stage = stage_name(brief.stage);
    let headline = headline(brief);
    let objective_label = if brief.stage == PulsationStage::Specification {
        "Objective"
    } else {
        "Objective of the approved specification"
    };
    let chips = brief.counts.clone();

    div()
        .debug_selector(|| "brief-decision".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::accent_soft())
        .bg(theme::surface())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme::text_muted())
                .child(div().size_2().rounded_full().bg(theme::warning()))
                .child(format!(
                    "{} · WAITING FOR YOUR DECISION",
                    stage.to_uppercase()
                )),
        )
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(headline),
        )
        .when_some(brief.objective.clone(), |card, objective| {
            card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(objective_label)
                            .child(provenance(Provenance::Agent)),
                    )
                    .child(div().text_sm().child(SharedString::from(objective))),
            )
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(chips.into_iter().map(|chip| {
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::surface_raised())
                        .text_xs()
                        .child(chip)
                })),
        )
        .into_any_element()
}

fn change_look(kind: ChangeKind) -> (Icon, gpui::Rgba, &'static str) {
    match kind {
        ChangeKind::Added => (Icon::Plus, theme::success(), "Added"),
        ChangeKind::Removed => (Icon::Minus, theme::danger(), "Removed"),
        ChangeKind::Moved => (Icon::ArrowRight, theme::accent(), "Moved"),
        ChangeKind::Changed => (Icon::Pencil, theme::warning(), "Changed"),
    }
}

fn subject_name(subject: ChangeSubject) -> &'static str {
    match subject {
        ChangeSubject::Objective => "Objective",
        ChangeSubject::Decision => "Decision",
        ChangeSubject::Criterion => "Criterion",
        ChangeSubject::Interface => "Interface",
        ChangeSubject::Exclusion => "Exclusion",
        ChangeSubject::File => "File",
        ChangeSubject::Unit => "Work unit",
        ChangeSubject::Check => "Check",
        ChangeSubject::Blocker => "Blocker",
    }
}

/// ADR-027 part 3: the previous candidate of this stage next to this one,
/// under the feedback the run received. The host compared them; the quoted
/// text is the agent's, except the feedback.
fn revision_card(diff: &RevisionDiff) -> AnyElement {
    let header = card_header(
        Icon::GitCompare,
        theme::accent(),
        "Changed since the previous revision",
        diff.changes.as_ref().map(Vec::len),
        Provenance::Host,
    );
    let card = card("brief-revision")
        .w_full()
        .border_color(theme::accent_soft())
        .child(header)
        .when_some(diff.feedback.as_deref(), |card, feedback| {
            card.child(revision_feedback(feedback))
        });
    let Some(changes) = diff.changes.as_deref() else {
        return card
            .child(
                div()
                    .debug_selector(|| "brief-revision-unavailable".into())
                    .flex()
                    .items_start()
                    .gap_2()
                    .text_sm()
                    .child(icon(Icon::TriangleAlert, px(16.0), theme::warning()))
                    .child(div().flex_1().min_w_0().child(
                        "The previous revision is unavailable, so nothing is compared. \
                         The documents in the next tabs are complete.",
                    )),
            )
            .into_any_element();
    };
    if changes.is_empty() {
        return card
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Nothing in this brief changed since the previous revision."),
            )
            .into_any_element();
    }
    card.child(
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("Compared by criterion, path and work unit. Decisions and exclusions have no id, so an edited one shows as removed and added. Quoted text is the agent's."),
    )
    .children(
        changes
            .iter()
            .enumerate()
            .map(|(index, change)| change_row(index, change)),
    )
    .into_any_element()
}

fn revision_feedback(feedback: &str) -> AnyElement {
    div()
        .debug_selector(|| "brief-revision-feedback".into())
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .pl_3()
        .border_l_2()
        .border_color(theme::warning())
        .child(
            div()
                .text_xs()
                .text_color(theme::warning())
                .child("Revision feedback this run received"),
        )
        .children(feedback.trim().lines().map(|line| {
            div()
                .min_w_0()
                .text_sm()
                .child(SharedString::from(line.to_owned()))
        }))
        .into_any_element()
}

/// `AC-2 · proof plan`: the compared key and, for an edit, the part that changed.
fn change_key(change: &BriefChange) -> Option<String> {
    match (change.label.is_empty(), change.field) {
        (true, None) => None,
        (true, Some(field)) => Some(field.to_owned()),
        (false, None) => Some(change.label.clone()),
        (false, Some(field)) => Some(format!("{} · {field}", change.label)),
    }
}

fn change_row(index: usize, change: &BriefChange) -> AnyElement {
    let (glyph, ink, word) = change_look(change.kind);
    let side = |label: &'static str, text: &str, muted: bool| {
        div()
            .min_w_0()
            .flex()
            .items_start()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .flex_shrink_0()
                    .w(px(36.0))
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(muted, |text| text.text_color(theme::text_muted()))
                    .child(SharedString::from(text.to_owned())),
            )
    };
    div()
        .debug_selector(move || format!("brief-change-{index}"))
        .min_w_0()
        .flex()
        .items_start()
        .gap_2()
        .child(div().pt(px(2.0)).child(icon(glyph, px(14.0), ink)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                // Fixed words keep their width; only the key wraps, at its
                // spaces, as one text. A shrinkable label inside a wrapping
                // row collapsed to one character per line in the real app.
                .child(
                    div()
                        .debug_selector(move || format!("brief-change-head-{index}"))
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap_2()
                        .text_xs()
                        .child(div().flex_shrink_0().text_color(ink).child(word))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme::text_muted())
                                .child(subject_name(change.subject)),
                        )
                        .when_some(change_key(change), |line, key| {
                            line.child(div().flex_1().min_w_0().child(SharedString::from(key)))
                        }),
                )
                .when_some(change.before.as_deref(), |rows, before| {
                    rows.child(side("Was", before, change.after.is_some()))
                })
                .when_some(change.after.as_deref(), |rows, after| {
                    rows.child(side("Now", after, false))
                }),
        )
        .into_any_element()
}

fn judgement_look(kind: JudgementKind) -> (Icon, gpui::Rgba, &'static str) {
    match kind {
        JudgementKind::Decision => (
            Icon::Signpost,
            theme::purple(),
            "Decisions change scope without a criterion of their own.",
        ),
        JudgementKind::ReadingOnlyProof => (
            Icon::CircleHelp,
            theme::warning(),
            "No host check will execute it; later stages can only report it as unverified.",
        ),
        JudgementKind::NotApplicable => (
            Icon::CircleHelp,
            theme::warning(),
            "It will not be verified at all. The reason is the agent's.",
        ),
        JudgementKind::InterfaceChange => (
            Icon::Plug,
            theme::warning(),
            "Code you already own changes shape.",
        ),
        JudgementKind::Exclusions => (
            Icon::Ban,
            theme::text_muted(),
            "Exclusions are easy to miss in prose. Check nothing here was expected.",
        ),
        JudgementKind::NewInterface => (
            Icon::Plug,
            theme::success(),
            "Other code will depend on this signature once it exists.",
        ),
        JudgementKind::ExistingTestsOnly => (
            Icon::CircleHelp,
            theme::warning(),
            "No test was written for it; tests that already existed are assumed to cover it.",
        ),
        JudgementKind::UnexecutedCheck => (
            Icon::TriangleAlert,
            theme::warning(),
            "The project requires this check, and nothing the host ran passed it.",
        ),
        JudgementKind::Contradicted => (
            Icon::X,
            theme::danger(),
            "The reviewer found the delivered code contradicts the criterion.",
        ),
        JudgementKind::Unverified => (
            Icon::CircleHelp,
            theme::warning(),
            "Nothing the reviewer saw shows the criterion holds.",
        ),
        JudgementKind::Blocker => (
            Icon::Ban,
            theme::danger(),
            "The reviewer reports this stops delivery.",
        ),
    }
}

fn judgement_card(
    items: &[JudgementItem],
    marks: &BriefMarks<'_>,
    emphasis: &[usize],
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let header = card_header(
        Icon::Signpost,
        theme::warning(),
        "Needs your judgement",
        Some(items.len()),
        Provenance::Developer,
    );
    let card = card("brief-judgement").child(header);
    if items.is_empty() {
        return card
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("Nothing here needs a decision the host cannot make."),
            )
            .into_any_element();
    }
    let rows: Vec<AnyElement> = items
        .iter()
        .enumerate()
        .map(|(index, item)| judgement_item(index, item, marks, emphasis.contains(&index), cx))
        .collect();
    card.child(
        div()
            .text_xs()
            .text_color(theme::text_muted())
            .child("Confirm what looks right and flag what should change. Flags and their notes become your revision feedback. C confirms, F flags, Tab moves."),
    )
    .children(rows)
    .into_any_element()
}

fn mark_button(
    id: SharedString,
    glyph: Icon,
    ink: gpui::Rgba,
    label: &'static str,
    active: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let selector = id.to_string();
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if active { ink } else { theme::border() })
        .bg(if active {
            theme::surface_raised()
        } else {
            theme::surface()
        })
        .text_xs()
        .child(icon(glyph, px(14.0), ink))
        .child(label)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.opacity(theme::HOVER_OPACITY))
        })
        .when(!enabled, |button| button.opacity(0.45))
}

fn judgement_item(
    index: usize,
    item: &JudgementItem,
    marks: &BriefMarks<'_>,
    emphasized: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let (glyph, ink, why) = judgement_look(item.kind);
    let mark = marks.marks.get(index).copied().unwrap_or_default();
    let focused = marks.cursor == index;
    let note = (mark == ItemMark::Flagged)
        .then(|| marks.notes.get(&index).cloned())
        .flatten();
    let enabled = marks.interactive;
    let state = match mark {
        ItemMark::Open => None,
        ItemMark::Confirmed => Some((Icon::Check, theme::success(), "Confirmed")),
        ItemMark::Flagged => Some((Icon::Flag, theme::warning(), "Flagged")),
    };
    let border = match mark {
        _ if focused && enabled => theme::accent(),
        ItemMark::Flagged => theme::warning(),
        _ => theme::border(),
    };
    let confirm = mark_button(
        SharedString::from(format!("brief-confirm-{index}")),
        Icon::Check,
        theme::success(),
        "Looks right · C",
        mark == ItemMark::Confirmed,
        enabled,
    )
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, window, cx| {
            model.toggle_brief_mark(index, ItemMark::Confirmed, Some(window), cx);
        }))
    });
    let flag = mark_button(
        SharedString::from(format!("brief-flag-{index}")),
        Icon::Flag,
        theme::warning(),
        "Flag · F",
        mark == ItemMark::Flagged,
        enabled,
    )
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, window, cx| {
            model.toggle_brief_mark(index, ItemMark::Flagged, Some(window), cx);
        }))
    });

    div()
        .debug_selector(move || format!("brief-item-{index}"))
        .min_w_0()
        .flex()
        .gap_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(if mark == ItemMark::Flagged {
            theme::warning_soft()
        } else {
            theme::canvas()
        })
        .child(div().pt(px(2.0)).child(icon(glyph, px(18.0), ink)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(judgement_kind_label(item.kind).to_uppercase())
                        .child(
                            div()
                                .text_color(theme::text())
                                .child(SharedString::from(item.label.clone())),
                        )
                        .child(div().flex_1())
                        .when(emphasized, |row| {
                            row.child(
                                div()
                                    .debug_selector(move || format!("brief-emphasis-{index}"))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_2()
                                    .rounded_full()
                                    .bg(theme::purple_soft())
                                    .text_color(theme::purple())
                                    .child(icon(Icon::Sparkles, px(12.0), theme::purple()))
                                    .child("Visualizer: look here first"),
                            )
                        })
                        .when_some(state, |row, (glyph, ink, label)| {
                            row.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_color(ink)
                                    .child(icon(glyph, px(14.0), ink))
                                    .child(label),
                            )
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(SharedString::from(item.text.clone())),
                )
                .child(div().text_xs().text_color(theme::text_muted()).child(why))
                .when_some(item.detail.as_deref(), |column, detail| {
                    column.child(agent_text(detail))
                })
                .when(!item.refs.is_empty(), |column| {
                    column.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(item.refs.iter().map(|reference| pill(reference.clone()))),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .pt_1()
                        .child(confirm)
                        .child(flag),
                )
                .when_some(note, |column, note| {
                    column.child(
                        div()
                            .debug_selector(move || format!("brief-note-{index}"))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("Your note · sent with Request revision"),
                            )
                            .child(crate::sizing::text_area(note)),
                    )
                }),
        )
        .into_any_element()
}

fn verified_card(lines: &[String]) -> AnyElement {
    card("brief-verified")
        .child(card_header(
            Icon::ShieldCheck,
            theme::accent(),
            "Already verified",
            Some(lines.len()),
            Provenance::Host,
        ))
        .children(lines.iter().map(|line| {
            div()
                .flex()
                .gap_2()
                .text_sm()
                .text_color(theme::text_muted())
                .child(
                    div()
                        .pt(px(2.0))
                        .child(icon(Icon::Check, px(14.0), theme::success())),
                )
                .child(div().flex_1().min_w_0().child(SharedString::from(line.clone())))
        }))
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("The host rejects a payload that breaks any of these, so you do not need to re-check them."),
        )
        .into_any_element()
}

fn proof_label(criterion: &BriefCriterion) -> (Icon, gpui::Rgba, String) {
    match criterion.verification {
        VerificationMode::ExecutedCheck => (
            Icon::Play,
            theme::success(),
            if criterion.checks.is_empty() {
                "Executed check".to_owned()
            } else {
                format!("Executed · {}", criterion.checks.join(", "))
            },
        ),
        VerificationMode::DocumentCheck => (
            Icon::Eye,
            theme::warning(),
            "By reading · document check".to_owned(),
        ),
        VerificationMode::NotApplicable => (
            Icon::CircleHelp,
            theme::warning(),
            "Not applicable".to_owned(),
        ),
    }
}

fn criteria_card(brief: &SpecificationVisual) -> AnyElement {
    let facts = &brief.facts;
    let total = facts.criteria.max(1) as f32;
    let bar = div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(220.0))
                .h(px(8.0))
                .flex()
                .gap(px(2.0))
                .when(facts.executed > 0, |bar| {
                    bar.child(
                        div()
                            .h_full()
                            .w(px(220.0 * facts.executed as f32 / total))
                            .bg(theme::success()),
                    )
                })
                .when(facts.by_reading + facts.not_applicable > 0, |bar| {
                    bar.child(
                        div()
                            .h_full()
                            .w(px(
                                220.0 * (facts.by_reading + facts.not_applicable) as f32 / total
                            ))
                            .bg(theme::warning()),
                    )
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "{} by an executed host check · {} not executed",
                    facts.executed,
                    facts.by_reading + facts.not_applicable
                )),
        );

    div()
        .debug_selector(|| "brief-criteria".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .child(card_header(
            Icon::Target,
            theme::text_muted(),
            "Acceptance criteria and how each will be proven",
            None,
            Provenance::Host,
        ))
        .child(bar)
        .children(brief.tasks.iter().enumerate().map(|(index, task)| {
            // The task id is an internal UUID; the developer knows the task by its title.
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .debug_selector(move || format!("brief-task-{index}"))
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme::text_muted())
                                .child("TASK"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(task.title.clone())),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .rounded_md()
                        .border_1()
                        .border_color(theme::border())
                        .children(task.criteria.iter().map(criterion_row)),
                )
        }))
        .into_any_element()
}

fn criterion_row(criterion: &BriefCriterion) -> AnyElement {
    let (glyph, ink, label) = proof_label(criterion);
    let flagged = criterion.verification != VerificationMode::ExecutedCheck;
    div()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border())
        .bg(if flagged {
            theme::warning_soft()
        } else {
            theme::canvas()
        })
        .child(
            div()
                .w(px(56.0))
                .flex_shrink_0()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(SharedString::from(criterion.criterion_id.clone())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(200.0))
                .text_sm()
                .child(SharedString::from(criterion.description.clone())),
        )
        .child(
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme::text_muted())
                .child(icon(glyph, px(14.0), ink))
                .child(label),
        )
        .into_any_element()
}

fn files_card(brief: &SpecificationVisual) -> AnyElement {
    let total: usize = brief.files.iter().map(|group| group.files.len()).sum();
    card("brief-files")
        .child(card_header(
            Icon::FolderOpen,
            theme::text_muted(),
            "What it will touch",
            Some(total),
            Provenance::Host,
        ))
        .children(brief.files.iter().map(file_group))
        .when(!brief.unattached_interfaces.is_empty(), |card| {
            card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("Interfaces not tied to an existing file"),
                    )
                    .children(
                        brief.unattached_interfaces.iter().map(|interface| {
                            interface_line(interface.intent, &interface.description)
                        }),
                    ),
            )
        })
        .into_any_element()
}

fn file_group(group: &BriefFileGroup) -> AnyElement {
    let root = if group.root.is_empty() {
        "Project root".to_owned()
    } else {
        group.root.clone()
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(theme::text_muted())
                .child(icon(Icon::FolderOpen, px(14.0), theme::text_muted()))
                .child(root),
        )
        .children(group.files.iter().map(file_row))
        .into_any_element()
}

fn file_row(file: &BriefFile) -> AnyElement {
    div()
        .ml_4()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(mono(file.path.clone())))
                .child(if file.planned {
                    div()
                        .px_2()
                        .rounded_full()
                        .bg(theme::success_soft())
                        .text_color(theme::success())
                        .text_xs()
                        .child("New")
                        .into_any_element()
                } else {
                    pill("Modified")
                }),
        )
        .children(
            file.interfaces
                .iter()
                .map(|interface| interface_line(interface.intent, &interface.description)),
        )
        .into_any_element()
}

fn interface_line(intent: InterfaceIntent, description: &str) -> AnyElement {
    let (label, ink) = match intent {
        InterfaceIntent::Preserve => ("Preserve", theme::text_muted()),
        InterfaceIntent::Change => ("Change", theme::warning()),
        InterfaceIntent::New => ("New", theme::success()),
        InterfaceIntent::None => ("No interface", theme::text_muted()),
    };
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .child(
            div()
                .flex_shrink_0()
                .px_2()
                .rounded_full()
                .border_1()
                .border_color(ink)
                .text_color(ink)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(theme::text_muted())
                .child(SharedString::from(description.to_owned())),
        )
        .into_any_element()
}

/// Longest dependency chain ending at each unit; units with an unknown
/// dependency fall back to the first step rather than disappearing.
fn unit_steps(units: &[BriefUnit]) -> Vec<usize> {
    let mut steps = vec![0usize; units.len()];
    for _ in 0..units.len() {
        let mut changed = false;
        for (index, unit) in units.iter().enumerate() {
            let step = unit
                .depends_on
                .iter()
                .filter_map(|key| units.iter().position(|other| &other.key == key))
                .map(|dependency| steps[dependency] + 1)
                .max()
                .unwrap_or(0);
            if step > steps[index] && step < units.len() {
                steps[index] = step;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    steps
}

fn units_card(units: &[BriefUnit]) -> AnyElement {
    let card = card("brief-units").child(card_header(
        Icon::Network,
        theme::text_muted(),
        "Work plan",
        Some(units.len()),
        Provenance::Host,
    ));
    if units.is_empty() {
        return card
            .child(
                div()
                    .text_sm()
                    .text_color(theme::text_muted())
                    .child("No work units were proposed; Code runs as one unit."),
            )
            .into_any_element();
    }
    let steps = unit_steps(units);
    let last = steps.iter().copied().max().unwrap_or(0);
    card.child(
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .children((0..=last).map(|step| {
                div()
                    .flex_1()
                    .min_w(px(180.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(format!("Step {}", step + 1)),
                    )
                    .children(
                        units
                            .iter()
                            .zip(&steps)
                            .filter(|(_, unit_step)| **unit_step == step)
                            .map(|(unit, _)| unit_node(unit)),
                    )
            })),
    )
    .into_any_element()
}

fn unit_node(unit: &BriefUnit) -> AnyElement {
    let (kind, ink, fill) = match unit.kind {
        ProposalKind::Code => ("CODE", theme::accent(), theme::accent_soft()),
        ProposalKind::Test => ("TEST", theme::purple(), theme::purple_soft()),
    };
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(fill)
                        .text_color(ink)
                        .text_xs()
                        .child(kind),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(SharedString::from(unit.key.clone())),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(SharedString::from(unit.destinations.join(" · "))),
        )
        .child(
            div().flex().flex_wrap().gap_1().children(
                unit.criteria
                    .iter()
                    .map(|criterion| pill(criterion.clone())),
            ),
        )
        .when(!unit.interfaces.is_empty(), |node| {
            node.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("Implements {}", unit.interfaces.join(", "))),
            )
        })
        .when(!unit.depends_on.is_empty(), |node| {
            node.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("After {}", unit.depends_on.join(", "))),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(key: &str, depends_on: &[&str]) -> BriefUnit {
        BriefUnit {
            key: key.into(),
            kind: ProposalKind::Code,
            destinations: Vec::new(),
            criteria: Vec::new(),
            depends_on: depends_on.iter().map(|key| (*key).into()).collect(),
            objective: String::new(),
            interfaces: Vec::new(),
        }
    }

    #[test]
    fn units_are_placed_after_their_longest_dependency_chain() {
        let units = [
            unit("ui", &["core"]),
            unit("core", &[]),
            unit("docs", &["ui", "core"]),
            unit("stray", &["missing"]),
        ];
        assert_eq!(unit_steps(&units), [1, 0, 2, 0]);
    }
}
