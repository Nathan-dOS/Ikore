//! The derived review brief of a checkpoint (ADR-024 M1).
//!
//! A native rendering of `hvc_core::orchestration::review_brief::ReviewBrief`. The host
//! decided every count, status and rule; this module only draws them. Text an agent wrote
//! (objective, decisions, requirements, interface and unit descriptions) is labelled where
//! it appears, and colour always comes with an icon and a word. Read-only in M1: the
//! Confirm/Flag controls arrive with M2.

use gpui::{AnyElement, SharedString, div, prelude::*, px};
use hvc_core::orchestration::composition::{InterfaceIntent, ProposalKind, VerificationMode};
use hvc_core::orchestration::review_brief::{
    BriefCriterion, BriefFile, BriefFileGroup, BriefUnit, JudgementItem, JudgementKind, ReviewBrief,
};
use hvc_core::persistence_service::PulsationStage;

use crate::icons::Icon;
use crate::theme;
use crate::widgets::icon;

/// Width under which a two-card row stacks; keeps each card readable at 960 px.
const CARD_MIN_WIDTH: f32 = 320.0;

pub(crate) fn brief_panel(brief: &ReviewBrief) -> AnyElement {
    div()
        .debug_selector(|| "review-brief".into())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_4()
        .child(decision_card(brief))
        .child(
            row()
                .child(judgement_card(&brief.judgement))
                .child(verified_card(&brief.verified)),
        )
        .child(criteria_card(brief))
        .child(
            row()
                .child(files_card(brief))
                .child(units_card(&brief.units)),
        )
        .child(div().text_xs().text_color(theme::text_muted()).child(
            "Derived from this checkpoint's validated payload and the frozen task batch. \
                     The documents in the next tabs are the record; approval still requires them.",
        ))
        .into_any_element()
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

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

// ---------------------------------------------------------------------------
// Sections
// ---------------------------------------------------------------------------

fn decision_card(brief: &ReviewBrief) -> AnyElement {
    let stage = stage_name(brief.stage);
    let headline = match brief.next_stage {
        Some(next) => format!(
            "Approve this {} so the {} stage can start.",
            stage.to_lowercase(),
            stage_name(next)
        ),
        None => format!("Approve this {}.", stage.to_lowercase()),
    };
    let facts = &brief.facts;
    let mut proof = vec![format!("{} executed", facts.executed)];
    if facts.by_reading > 0 {
        proof.push(format!("{} by reading", facts.by_reading));
    }
    if facts.not_applicable > 0 {
        proof.push(format!("{} not applicable", facts.not_applicable));
    }
    let mut chips = vec![
        plural(facts.tasks, "task", "tasks"),
        format!(
            "{} · {}",
            plural(facts.criteria, "criterion", "criteria"),
            proof.join(", ")
        ),
        format!(
            "{} · {} new",
            plural(facts.existing_files + facts.planned_files, "file", "files"),
            facts.planned_files
        ),
    ];
    if facts.changed_interfaces > 0 {
        chips.push(format!(
            "{} changed",
            plural(
                facts.changed_interfaces,
                "existing interface",
                "existing interfaces"
            )
        ));
    }
    if facts.units > 0 {
        chips.push(plural(facts.units, "work unit", "work units"));
    }

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
        .child(
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
                        .child("Objective")
                        .child(provenance(Provenance::Agent)),
                )
                .child(
                    div()
                        .text_sm()
                        .child(SharedString::from(brief.objective.clone())),
                ),
        )
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

fn judgement_look(kind: JudgementKind) -> (Icon, gpui::Rgba, &'static str, &'static str) {
    match kind {
        JudgementKind::Decision => (
            Icon::Signpost,
            theme::purple(),
            "Decision taken by the agent",
            "Decisions change scope without a criterion of their own.",
        ),
        JudgementKind::ReadingOnlyProof => (
            Icon::CircleHelp,
            theme::warning(),
            "Proof by reading only",
            "No host check will execute it; later stages can only report it as unverified.",
        ),
        JudgementKind::NotApplicable => (
            Icon::CircleHelp,
            theme::warning(),
            "Marked not applicable",
            "It will not be verified at all. The reason is the agent's.",
        ),
        JudgementKind::InterfaceChange => (
            Icon::Plug,
            theme::warning(),
            "Existing interface changes",
            "Code you already own changes shape.",
        ),
        JudgementKind::Exclusions => (
            Icon::Ban,
            theme::text_muted(),
            "Left out of scope",
            "Exclusions are easy to miss in prose. Check nothing here was expected.",
        ),
    }
}

fn judgement_card(items: &[JudgementItem]) -> AnyElement {
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
    card.children(items.iter().map(judgement_item))
        .into_any_element()
}

fn judgement_item(item: &JudgementItem) -> AnyElement {
    let (glyph, ink, kind_label, why) = judgement_look(item.kind);
    div()
        .min_w_0()
        .flex()
        .gap_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
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
                        .gap_2()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(kind_label.to_uppercase())
                        .child(
                            div()
                                .text_color(theme::text())
                                .child(SharedString::from(item.label.clone())),
                        ),
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

fn criteria_card(brief: &ReviewBrief) -> AnyElement {
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

fn files_card(brief: &ReviewBrief) -> AnyElement {
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
