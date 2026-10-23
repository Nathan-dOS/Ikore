//! The Notes view (ADR-029): capture and find notes on the left, read and
//! refine the selected one on the right. Agent-written text is labelled; the
//! developer's own words stay one click away in the raw-input drawer.
use crate::{
    AppModel,
    icons::Icon,
    notes::{NotesAction, visible_notes},
    selectable_message::SelectableMessage,
    sizing::{StyledExt, text_area},
    theme,
    widgets::{ButtonVariant, btn, btn_confirm, chip, empty_state, icon, info_tip},
};
use gpui::{AnyElement, Context, Div, SharedString, div, prelude::*, px};
use hvc_core::companion_notes::{
    CompanionNote, LIST_LIMIT, NoteSource, NoteStatus, NoteTag, NoteTurnKind, explain_note_failure,
};
use hvc_core::feature_forge::TurnFailureKind;
use hvc_editor::parse_markdown;

pub(crate) fn notes_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    if model.workspace.read(cx).project_id.is_none() {
        return crate::widgets::empty_panel(
            Icon::NotebookPen,
            "Open a project to keep notes",
            "Notes belong to a project. Open one to capture ideas for it.",
        );
    }
    let master = master_column(model, cx);
    let detail = detail_column(model, cx);
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .min_w_0()
                        .child(heading("Notes"))
                        .child(info_tip(
                            "notes-about",
                            "Write a loose thought and press Enter. The Notes Scribe turns it into a note with a title, a summary and tags, in your language. It never changes code, tasks or features.",
                        )),
                )
                .child(action_button(
                    "notes-refresh",
                    "Refresh",
                    ButtonVariant::Ghost,
                    !model.notes.busy,
                    NotesAction::Refresh,
                    cx,
                )),
        )
        .when_some(model.notes.error.clone(), |panel, error| {
            panel.child(crate::widgets::inline_banner(
                error,
                crate::widgets::BannerKind::Error,
            ))
        })
        .child(
            div()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .flex()
                .child(crate::split_pane::split_pane(
                    "notes-columns",
                    &model.notes_split,
                    master,
                    detail,
                    cx,
                )),
        )
        .into_any_element()
}

fn heading(value: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(value.into())
}

fn label(value: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(value.into())
}

/// A composer hint beside its button. It takes the free width but never less
/// than a readable minimum, so a narrow column wraps the button under it
/// rather than squeezing the hint into a column of single words.
fn hint(value: impl Into<SharedString>) -> Div {
    label(value).flex_1().min_w(px(140.0))
}

fn section_label(value: impl Into<SharedString>) -> Div {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text_subtle())
        .child(value.into())
}

fn action_button(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    variant: ButtonVariant,
    enabled: bool,
    action: NotesAction,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    btn(id, title, variant, enabled, cx, move |model, cx| {
        model.notes_action(action.clone(), cx)
    })
}

/// Capture, search and filters stay put; only the list scrolls.
fn master_column(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let notes = &model.notes;
    let can_capture = !notes.busy && !notes.capture.read(cx).value().trim().is_empty();
    let capture = div()
        .debug_selector(|| "notes-capture".into())
        .flex()
        .flex_col()
        .gap_2()
        .flex_shrink_0()
        .child(text_area(notes.capture.clone()))
        .child(
            // Wraps on a narrow column, so the button drops under the hint
            // instead of leaving the column.
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .min_w_0()
                .child(hint(match &model.voice_status {
                    // Push-to-talk works from anywhere; the capture box is where it shows.
                    Some(voice) if voice.listening => format!(
                        "Enter saves · or hold {} and speak",
                        voice.settings.hotkey.label()
                    ),
                    _ => "Enter saves · Shift+Enter new line".to_string(),
                }))
                .child(action_button(
                    "notes-capture-save",
                    "Save note",
                    ButtonVariant::Primary,
                    can_capture,
                    NotesAction::Capture,
                    cx,
                )),
        );
    let search = div()
        .debug_selector(|| "notes-search".into())
        .relative()
        .flex_shrink_0()
        .child(notes.search.clone())
        .child(div().absolute().left(px(10.0)).top(px(11.0)).child(icon(
            Icon::Search,
            px(14.0),
            theme::text_subtle(),
        )));
    let mut tags = crate::widgets::segment_track(div())
        .debug_selector(|| "notes-tag-filter".into())
        .flex_wrap()
        .flex_shrink_0();
    for tag in NoteTag::ALL {
        let id = SharedString::from(format!("notes-tag-{}", tag.id()));
        let selector = id.clone();
        tags = tags.child(
            crate::widgets::segment(
                div()
                    .id(id)
                    .debug_selector(move || selector.to_string())
                    .tab_index(0),
                notes.tag_filter.contains(&tag),
            )
            .on_click(cx.listener(move |model, _, _, cx| {
                model.notes_action(NotesAction::ToggleTag(tag), cx)
            }))
            .child(tag.label()),
        );
    }
    let mut column = div()
        .debug_selector(|| "notes-master".into())
        .size_full()
        .min_w_0()
        .min_h_0()
        .pr_3()
        .flex()
        .flex_col()
        .gap_3()
        .child(capture)
        .child(search)
        .child(tags);
    if notes.has_voice_notes() {
        column = column.child(source_filter(model, cx));
    }
    column.child(note_list(model, cx)).into_any_element()
}

fn source_filter(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut track = crate::widgets::segment_track(div())
        .flex_shrink_0()
        .self_start();
    for (id, title, source) in [
        ("notes-source-all", "All", None),
        ("notes-source-text", "Typed", Some(NoteSource::Text)),
        ("notes-source-voice", "Spoken", Some(NoteSource::Voice)),
    ] {
        track = track.child(
            crate::widgets::segment(
                div().id(id).debug_selector(move || id.into()).tab_index(0),
                model.notes.source_filter == source,
            )
            .on_click(cx.listener(move |model, _, _, cx| {
                model.notes_action(NotesAction::SetSource(source), cx)
            }))
            .child(title),
        );
    }
    track.into_any_element()
}

fn note_list(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let notes = &model.notes;
    let query = notes.search.read(cx).value().to_string();
    let visible = visible_notes(
        &notes.snapshot.notes,
        &query,
        &notes.tag_filter,
        notes.source_filter,
    );
    let mut list = div()
        .id("notes-list")
        .debug_selector(|| "notes-list".into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();
    if notes.snapshot.notes.is_empty() {
        return list
            .child(
                div()
                    .debug_selector(|| "notes-empty".into())
                    .child(empty_state(
                        "No notes yet. Write a thought above and press Enter.",
                    )),
            )
            .into_any_element();
    }
    if visible.is_empty() {
        return list
            .child(
                div()
                    .debug_selector(|| "notes-no-matches".into())
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(empty_state("No note matches the search and filters."))
                    .child(action_button(
                        "notes-clear-filters",
                        "Clear filters",
                        ButtonVariant::Secondary,
                        true,
                        NotesAction::ClearFilters,
                        cx,
                    )),
            )
            .into_any_element();
    }
    let now = chrono::Utc::now();
    for note in visible {
        list = list.child(note_card(model, note, now, cx));
    }
    if notes.snapshot.truncated {
        list = list.child(label(format!("Showing the newest {LIST_LIMIT} notes.")));
    }
    list.into_any_element()
}

fn note_card(
    model: &AppModel,
    note: &CompanionNote,
    now: chrono::DateTime<chrono::Utc>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let selected = model.notes.selected.as_ref() == Some(&note.id);
    let background = if selected {
        theme::accent_soft()
    } else {
        theme::surface_raised()
    };
    let id = SharedString::from(format!("note-{}", note.id));
    let selector = id.clone();
    let mut meta = div().flex().flex_wrap().items_center().gap_1().min_w_0();
    match note.status {
        NoteStatus::Structuring => {
            meta = meta.child(crate::widgets::loading_pill("Structuring"));
        }
        NoteStatus::Failed => meta = meta.child(chip("Not structured", theme::warning())),
        NoteStatus::Ready => {}
    }
    for tag in &note.tags {
        meta = meta.child(chip(tag.label(), theme::text_muted()));
    }
    meta = meta.child(
        div()
            .ml_auto()
            .text_xs()
            .text_color(theme::text_subtle())
            .child(crate::time::relative_timestamp(&note.created_at, now)),
    );
    let note_id = note.id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex_shrink_0()
        .p_3()
        .rounded_md()
        .bg(background)
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .tab_index(0)
        .hover(|style| style.border_color(theme::border()))
        .focus_visible(|style| style.border_color(theme::accent()))
        .on_click(cx.listener(move |model, _, _, cx| {
            model.notes_action(NotesAction::Select(note_id.clone()), cx)
        }))
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(crate::fading_title::FadingTitle::new(
                    note.display_title(),
                    background,
                )),
        )
        .child(meta)
        .into_any_element()
}

/// The note scrolls; the refinement composer stays pinned under it.
fn detail_column(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some(note) = model.notes.note().cloned() else {
        return div()
            .debug_selector(|| "notes-detail".into())
            .size_full()
            .flex()
            .child(crate::widgets::empty_panel(
                Icon::NotebookPen,
                "Select a note",
                "Pick a note on the left, or capture a new thought.",
            ))
            .into_any_element();
    };
    let body = div()
        .id("notes-detail-body")
        .debug_selector(|| "notes-detail-body".into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .pl_3()
        .flex()
        .flex_col()
        .gap_4()
        .child(note_header(model, &note, cx))
        .when_some(status_banner(model, &note, cx), |body, banner| {
            body.child(banner)
        })
        .when_some(note_text(&note), |body, text| body.child(text))
        .child(raw_drawer(model, &note, cx))
        .when_some(conversation(model, &note, cx), |body, conversation| {
            body.child(conversation)
        });
    div()
        .debug_selector(|| "notes-detail".into())
        .size_full()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .child(body)
        .child(refine_composer(model, &note, cx))
        .into_any_element()
}

fn note_header(model: &AppModel, note: &CompanionNote, cx: &mut Context<AppModel>) -> AnyElement {
    let mut meta = div().flex().flex_wrap().items_center().gap_2().min_w_0();
    for tag in &note.tags {
        meta = meta.child(chip(tag.label(), theme::accent()));
    }
    meta = meta.child(label(format!(
        "{} · {}",
        match note.source_type {
            NoteSource::Text => "Typed",
            NoteSource::Voice => "Spoken",
        },
        crate::time::format_timestamp(&note.created_at)
    )));
    if let Some(language) = note.output_language.as_deref() {
        meta = meta.child(
            div()
                .debug_selector(|| "note-agent-label".into())
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::agent_color(hvc_core::llm::AgentRole::NotesScribe))
                .child(format!(
                    "WRITTEN BY THE NOTES SCRIBE · {}",
                    language.to_uppercase()
                )),
        );
    }
    let confirming = model.notes.confirm_delete.as_ref() == Some(&note.id);
    let deletable = !model.notes.busy && model.notes.running_turn().is_none();
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap_3()
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .debug_selector(|| "note-title".into())
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .min_w_0()
                        .child(SelectableMessage::new(
                            format!("note-title:{}", note.id),
                            note.display_title(),
                        )),
                )
                .child(meta),
        )
        .child(btn_confirm(
            "note-delete",
            "Delete",
            "Confirm delete",
            confirming,
            deletable,
            ButtonVariant::Ghost,
            cx,
            |model, cx| model.notes_action(NotesAction::Delete, cx),
            |model, cx| model.notes_action(NotesAction::ConfirmDelete, cx),
        ))
        .into_any_element()
}

/// What the Scribe is doing with the note, or why it could not.
fn status_banner(
    model: &AppModel,
    note: &CompanionNote,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    if let Some(turn) = model.notes.running_turn().cloned() {
        let structuring = note.status == NoteStatus::Structuring;
        return Some(
            div()
                .debug_selector(|| "note-working".into())
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .p_3()
                .rounded_md()
                .bg(theme::accent_soft())
                .text_sm()
                .text_color(theme::accent())
                .child(if structuring {
                    "The Notes Scribe is organizing this note…"
                } else {
                    "The Notes Scribe is rewriting this note…"
                })
                .child(action_button(
                    "note-cancel",
                    "Cancel",
                    ButtonVariant::Secondary,
                    true,
                    NotesAction::Cancel(turn),
                    cx,
                ))
                .into_any_element(),
        );
    }
    if note.status != NoteStatus::Failed {
        return structure_notices(model);
    }
    let latest = model
        .notes
        .history
        .iter()
        .rev()
        .find(|turn| turn.kind == NoteTurnKind::Structure);
    let failure = latest.and_then(|turn| turn.failure.clone());
    let explanation = failure.as_ref().map(explain_note_failure);
    let mut card = div()
        .debug_selector(|| "note-failed".into())
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(theme::danger_soft())
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::danger())
                .child(
                    explanation
                        .as_ref()
                        .map(|e| e.summary.clone())
                        .unwrap_or_else(|| "This note has not been structured yet.".into()),
                ),
        );
    if let Some(explanation) = &explanation
        && failure.as_ref().map(|f| f.kind) != Some(TurnFailureKind::Cancelled)
    {
        card = card
            .child(div().text_sm().child(explanation.cause.clone()))
            .child(label(explanation.remedy.clone()));
    }
    let mut buttons = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(action_button(
            "note-retry-structure",
            "Structure again",
            ButtonVariant::Tinted,
            !model.notes.busy,
            NotesAction::RetryStructure,
            cx,
        ));
    if let (Some(turn), Some(failure)) = (latest, failure.as_ref())
        && failure.kind != TurnFailureKind::Cancelled
    {
        let expanded = model.notes.expanded_failures.contains(&turn.id);
        buttons = buttons.child(action_button(
            "note-failure-detail",
            if expanded {
                "Hide technical detail"
            } else {
                "Technical detail"
            },
            ButtonVariant::Ghost,
            true,
            NotesAction::ToggleFailureDetail(turn.id.clone()),
            cx,
        ));
        card = card.child(buttons);
        if expanded {
            card = card.child(
                div()
                    .text_xs()
                    .text_color(theme::text_subtle())
                    .min_w_0()
                    .child(SelectableMessage::new(
                        format!("note-technical:{}", turn.id),
                        failure.technical.clone(),
                    )),
            );
        }
    } else {
        card = card.child(buttons);
    }
    Some(card.into_any_element())
}

/// What the host accepted the latest structuring with a reservation about.
fn structure_notices(model: &AppModel) -> Option<AnyElement> {
    let turn = model
        .notes
        .history
        .iter()
        .rev()
        .find(|turn| turn.kind == NoteTurnKind::Structure && turn.status == "completed")?;
    if turn.notices.is_empty() {
        return None;
    }
    let mut notices = div()
        .debug_selector(|| "note-notices".into())
        .flex()
        .flex_col()
        .gap_1();
    for notice in &turn.notices {
        notices = notices.child(crate::widgets::inline_banner(
            notice.clone(),
            crate::widgets::BannerKind::Warning,
        ));
    }
    Some(notices.into_any_element())
}

fn note_text(note: &CompanionNote) -> Option<AnyElement> {
    let summary = note.summary.as_ref()?;
    let mut text = div()
        .flex()
        .flex_col()
        .gap_3()
        .min_w_0()
        .child(section_label("SUMMARY"))
        .child(
            div()
                .debug_selector(|| "note-summary".into())
                .text_sm()
                .min_w_0()
                .child(SelectableMessage::new(
                    format!("note-summary:{}", note.id),
                    summary.clone(),
                )),
        );
    if let Some(content) = &note.content_markdown {
        text = text.child(section_label("TOPICS AND IMPACT")).child(
            div()
                .debug_selector(|| "note-content".into())
                .min_w_0()
                .text_sm()
                .children(
                    parse_markdown(content)
                        .into_iter()
                        .map(crate::workspace_view::render_markdown_block),
                ),
        );
    }
    Some(text.into_any_element())
}

fn raw_drawer(model: &AppModel, note: &CompanionNote, cx: &mut Context<AppModel>) -> AnyElement {
    let open = model.notes.raw_open;
    let redacted = match note.raw_redactions {
        0 => String::new(),
        1 => " · 1 secret removed".into(),
        count => format!(" · {count} secrets removed"),
    };
    let mut drawer = div()
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .child(
            div()
                .id("note-raw-toggle")
                .debug_selector(|| "note-raw-toggle".into())
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .tab_index(0)
                .text_sm()
                .hover(|style| style.bg(theme::surface_hover()))
                .focus_visible(|style| style.bg(theme::surface_hover()))
                .on_click(
                    cx.listener(|model, _, _, cx| model.notes_action(NotesAction::ToggleRaw, cx)),
                )
                .child(icon(
                    if open {
                        Icon::ChevronDown
                    } else {
                        Icon::ChevronRight
                    },
                    px(12.0),
                    theme::text_subtle(),
                ))
                .child(format!("Your original text{redacted}")),
        );
    if open {
        drawer = drawer.child(
            div()
                .debug_selector(|| "note-raw".into())
                .px_3()
                .pb_3()
                .text_sm()
                .text_color(theme::text_muted())
                .min_w_0()
                .child(SelectableMessage::new(
                    format!("note-raw:{}", note.id),
                    note.raw_input.clone(),
                )),
        );
    }
    drawer.into_any_element()
}

/// The refinement exchange, oldest first.
fn conversation(
    model: &AppModel,
    note: &CompanionNote,
    cx: &mut Context<AppModel>,
) -> Option<AnyElement> {
    let turns = model
        .notes
        .history
        .iter()
        .filter(|turn| turn.kind == NoteTurnKind::Refine && turn.note_id == note.id)
        .collect::<Vec<_>>();
    if turns.is_empty() {
        return None;
    }
    let mut column = div()
        .debug_selector(|| "note-conversation".into())
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .child(section_label("REFINEMENTS"));
    for turn in turns {
        if let Some(message) = &turn.user_message {
            column = column.child(message_card(
                format!("note-you:{}", turn.id),
                "You",
                message,
            ));
        }
        if let Some(reply) = &turn.assistant_message {
            column = column.child(message_card(
                format!("note-scribe:{}", turn.id),
                "Notes Scribe",
                reply,
            ));
        }
        for notice in &turn.notices {
            column = column.child(crate::widgets::inline_banner(
                notice.clone(),
                crate::widgets::BannerKind::Warning,
            ));
        }
        if let Some(failure) = &turn.failure {
            let explanation = explain_note_failure(failure);
            let mut card = div()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded_md()
                .bg(if failure.kind == TurnFailureKind::Cancelled {
                    theme::surface_raised()
                } else {
                    theme::danger_soft()
                })
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(explanation.summary),
                );
            if failure.kind != TurnFailureKind::Cancelled {
                let expanded = model.notes.expanded_failures.contains(&turn.id);
                card = card
                    .child(label(format!(
                        "{} {}",
                        explanation.cause, explanation.remedy
                    )))
                    .child(action_button(
                        SharedString::from(format!("note-turn-detail-{}", turn.id)),
                        if expanded {
                            "Hide technical detail"
                        } else {
                            "Technical detail"
                        },
                        ButtonVariant::Ghost,
                        true,
                        NotesAction::ToggleFailureDetail(turn.id.clone()),
                        cx,
                    ));
                if expanded {
                    card = card.child(div().text_xs().text_color(theme::text_subtle()).child(
                        SelectableMessage::new(
                            format!("note-turn-technical:{}", turn.id),
                            failure.technical.clone(),
                        ),
                    ));
                }
            }
            column = column.child(card);
        }
    }
    Some(column.into_any_element())
}

fn message_card(id: String, author: &str, message: &str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded_md()
        .bg(theme::surface_raised())
        .min_w_0()
        .child(section_label(author.to_uppercase()))
        .child(div().text_sm().child(SelectableMessage::new(id, message)))
        .into_any_element()
}

fn refine_composer(
    model: &AppModel,
    note: &CompanionNote,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let ready = note.status == NoteStatus::Ready;
    let free = model.notes.running_turn().is_none() && !model.notes.busy;
    let has_text = !model.notes.refine.read(cx).value().trim().is_empty();
    div()
        .debug_selector(|| "notes-refine".into())
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap_2()
        .pl_3()
        .pt_3()
        .border_t_1()
        .border_color(theme::border())
        .child(text_area(model.notes.refine.clone()))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .min_w_0()
                .child(hint(if ready {
                    "Enter sends · The Scribe changes only this note"
                } else {
                    "Available once the note is structured"
                }))
                .child(action_button(
                    "notes-refine-send",
                    "Send",
                    ButtonVariant::Tinted,
                    ready && free && has_text,
                    NotesAction::Refine,
                    cx,
                )),
        )
        .into_any_element()
}
