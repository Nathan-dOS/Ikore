//! The Vault's documentation reconciliation surfaces (ADR-034 M5). The text
//! names what changed and when it was seen; it never says who changed it nor
//! that the change is a defect.

use gpui::{
    AnyElement, Context, Div, IntoElement, ParentElement, SharedString, Styled, div, prelude::*,
};
use ikore_core::doc_reconciliation::{
    ChangeKind, DriftedNote, ProposalState, ReconciliationProposal, indicator_text,
};

use crate::AppModel;
use crate::doc_drift::DocDriftAction;
use crate::theme;
use crate::widgets::{ButtonVariant, btn};

fn label(text: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(text.into())
}

fn mono(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(theme::FONT_MONO)
        .text_xs()
        .whitespace_normal()
        .child(text.into())
}

fn action(
    id: impl Into<SharedString>,
    title: &'static str,
    variant: ButtonVariant,
    enabled: bool,
    action: DocDriftAction,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    btn(id, title, variant, enabled, cx, move |model, cx| {
        model.doc_drift_action(action.clone(), cx)
    })
}

/// The list in the Vault: one row per note whose linked code changed.
pub(crate) fn drift_card(model: &AppModel, cx: &mut Context<AppModel>) -> Option<AnyElement> {
    let ui = &model.doc_drift;
    let overview = &ui.overview;
    if overview.tracked_links == 0 && overview.notes.is_empty() && overview.silenced.is_empty() {
        return None;
    }
    let drifted = overview.drifted();
    let mut card = div()
        .id("doc-drift-card")
        .debug_selector(|| "doc-drift-card".into())
        .p_3()
        .rounded_md()
        .bg(if drifted > 0 {
            theme::warning_soft()
        } else {
            theme::surface_raised()
        })
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if drifted > 0 {
                            indicator_text(drifted)
                        } else {
                            "Linked notes match their code".to_owned()
                        }),
                )
                .child(crate::widgets::info_tip(
                    "doc-drift-info",
                    "Notes are compared with the files in their `source-files` and `test-files`. Only hashes are compared; nothing is sent to a model until you ask for a proposal.",
                )),
        )
        .child(label(format!(
            "{} linked file(s) tracked · {} note(s) without links",
            overview.tracked_links, overview.unlinked_notes
        )));
    if let Some(scan) = overview.scan.as_ref().filter(|scan| scan.partial) {
        card = card.child(label(format!(
            "The last scan compared {} of {} links; the rest is compared as files change.",
            scan.scanned, scan.total
        )));
    }
    for note in &overview.notes {
        let path = note.note_path.clone();
        let selector = format!("doc-drift-note:{path}");
        let files = note
            .signals
            .iter()
            .map(|signal| signal.code_path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        card = card.child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| {
                    model.doc_drift_action(DocDriftAction::Open(path.clone()), cx)
                }))
                .child(div().text_sm().child(note.note_path.clone()))
                .child(label(if note.proposal.is_some() {
                    format!("{files} · proposal ready to review")
                } else {
                    files
                })),
        );
    }
    for silenced in &overview.silenced {
        let selector = format!(
            "doc-drift-unsilence:{}:{}",
            silenced.note_path, silenced.code_path
        );
        card = card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(label(format!(
                    "Silenced: {} → {}",
                    silenced.note_path, silenced.code_path
                )))
                .child(action(
                    selector,
                    "Track again",
                    ButtonVariant::Ghost,
                    !ui.busy,
                    DocDriftAction::Unsilence(
                        silenced.note_path.clone(),
                        silenced.code_path.clone(),
                    ),
                    cx,
                )),
        );
    }
    Some(card.into_any_element())
}

fn code_column(
    note: &DriftedNote,
    proposal: Option<&ReconciliationProposal>,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> gpui::Stateful<Div> {
    let mut column = div()
        .id("doc-drift-code")
        .debug_selector(|| "doc-drift-code".into())
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Code changes"),
        );
    for signal in &note.signals {
        let selector = format!("doc-drift-silence:{}", signal.code_path);
        column = column.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().text_sm().child(signal.code_path.clone()))
                .child(label(format!(
                    "{} · {}",
                    if signal.current_sha256.is_none() {
                        "removed"
                    } else {
                        "changed"
                    },
                    signal.origin.label()
                )))
                .child(action(
                    selector,
                    "Silence this file",
                    ButtonVariant::Ghost,
                    !busy,
                    DocDriftAction::Silence(note.note_path.clone(), signal.code_path.clone()),
                    cx,
                )),
        );
    }
    if let Some(proposal) = proposal {
        for change in &proposal.code_changes {
            column = column.child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "{}{}",
                        change.path,
                        match (&change.change, change.gap.as_deref()) {
                            (ChangeKind::Removed, _) => " (removed)".to_owned(),
                            (_, Some(gap)) => format!(" ({})", gap.replace('_', " ")),
                            _ => String::new(),
                        }
                    )),
            );
            if let Some(diff) = change.diff.as_ref().or(change.current.as_ref()) {
                column = column.child(mono(diff.clone()));
            }
        }
    }
    column
}

fn note_column(
    model: &AppModel,
    note: &DriftedNote,
    cx: &mut Context<AppModel>,
) -> gpui::Stateful<Div> {
    let ui = &model.doc_drift;
    let mut column = div()
        .id("doc-drift-proposal")
        .debug_selector(|| "doc-drift-proposal".into())
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Proposed note update"),
        );
    let generating = ui.generating(&note.note_path);
    let Some(proposal) = note.proposal.as_ref() else {
        if let Some(failure) = ui.failure(&note.note_path) {
            column = column.child(crate::widgets::inline_banner(
                format!("The last proposal could not be prepared: {failure}"),
                crate::widgets::BannerKind::Warning,
            ));
        }
        column = column
            .child(label(
                "A proposal sends the sanitized note and the sanitized line diffs of these files to the Documentation Reconciler. Nothing is written until you approve.",
            ))
            .child(if generating {
                crate::widgets::loading_pill("Preparing a proposal")
            } else {
                action(
                    "doc-drift-propose",
                    "Propose an update",
                    ButtonVariant::Primary,
                    !ui.busy && !note.signals.is_empty(),
                    DocDriftAction::Propose(note.note_path.clone()),
                    cx,
                )
            });
        return column;
    };
    for item in &proposal.change_summary {
        column = column.child(div().text_sm().child(format!("• {item}")));
    }
    for item in &proposal.unresolved {
        column = column.child(label(format!("Unresolved: {item}")));
    }
    if proposal.state == ProposalState::Edited {
        column = column.child(label("Edited by you."));
    }
    if ui.editing {
        column = column
            .child(crate::sizing::text_area(ui.editor.clone()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(action(
                        "doc-drift-save-edit",
                        "Save the edit",
                        ButtonVariant::Primary,
                        !ui.busy,
                        DocDriftAction::SaveEdit(proposal.id.clone()),
                        cx,
                    ))
                    .child(action(
                        "doc-drift-cancel-edit",
                        "Cancel",
                        ButtonVariant::Ghost,
                        true,
                        DocDriftAction::CancelEdit,
                        cx,
                    )),
            );
        return column;
    }
    column = column
        .child(mono(if proposal.preview_diff.is_empty() {
            "The note stays as it is; see the unresolved items.".to_owned()
        } else {
            proposal.preview_diff.clone()
        }))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(action(
                    "doc-drift-approve",
                    "Approve and publish",
                    ButtonVariant::Primary,
                    !ui.busy,
                    DocDriftAction::Approve(proposal.id.clone()),
                    cx,
                ))
                .child(action(
                    "doc-drift-edit",
                    "Edit",
                    ButtonVariant::Tinted,
                    !ui.busy,
                    DocDriftAction::StartEdit,
                    cx,
                ))
                .child(action(
                    "doc-drift-reject",
                    "Discard proposal",
                    ButtonVariant::Ghost,
                    !ui.busy,
                    DocDriftAction::Reject(proposal.id.clone()),
                    cx,
                )),
        );
    column
}

/// The comparison of one note: code changes on the left, the proposed note
/// on the right, and the developer's decisions.
pub(crate) fn comparison(model: &AppModel, cx: &mut Context<AppModel>) -> Option<AnyElement> {
    let ui = &model.doc_drift;
    let note = ui.open()?.clone();
    let mut panel = div()
        .id("doc-drift-comparison")
        .debug_selector(|| "doc-drift-comparison".into())
        .overflow_y_scroll()
        .size_full()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .child(label("DOCUMENTATION RECONCILIATION"))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(note.note_path.clone()),
                        ),
                )
                .child(action(
                    "doc-drift-close",
                    "Back to the graph",
                    ButtonVariant::Ghost,
                    true,
                    DocDriftAction::Close,
                    cx,
                )),
        );
    if let Some(error) = &ui.error {
        panel = panel.child(crate::widgets::inline_banner(
            error.clone(),
            crate::widgets::BannerKind::Warning,
        ));
    }
    if let Some(notice) = &ui.notice {
        panel = panel.child(crate::widgets::inline_banner(
            notice.clone(),
            crate::widgets::BannerKind::Info,
        ));
    }
    panel = panel.child(
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(code_column(&note, note.proposal.as_ref(), ui.busy, cx))
            .child(note_column(model, &note, cx)),
    );
    if !note.signals.is_empty() {
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(label(
                    "Keep the note as it is: the current files become the reference.",
                ))
                .child(crate::sizing::sized_input(
                    ui.reason.clone(),
                    gpui::px(360.0),
                ))
                .child(action(
                    "doc-drift-ignore",
                    "Keep the note as it is",
                    ButtonVariant::Ghost,
                    !ui.busy,
                    DocDriftAction::Ignore(note.note_path.clone()),
                    cx,
                )),
        );
    }
    Some(panel.into_any_element())
}
