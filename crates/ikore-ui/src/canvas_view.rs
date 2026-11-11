//! Native GPUI drawing surface and compact project document explorer.
use gpui::{
    AnyElement, App, Bounds, Context, FontWeight, MouseButton, PathBuilder, Pixels, SharedString,
    TextRun, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use ikore_core::companion_drawings::{
    CanvasBackgroundMode, CanvasConnectorCurve as Curve, CanvasElementKindV1 as Kind,
    CanvasElementV1 as Element, CanvasSceneV1 as Scene, CanvasSemanticColor as Color,
    CanvasStrokeRendering as Rendering, CanvasStrokeStyle as Stroke, CanvasStrokeWidth as Width,
    CanvasViewportV1 as Viewport, geometry, visual_geometry,
};

use crate::{
    AppModel,
    canvas::{CanvasAction as Action, EditorState, Tool},
    icons::Icon,
    theme,
    widgets::{ButtonVariant, btn},
};

#[cfg(test)]
#[path = "canvas_tool_tests.rs"]
mod tool_tests;

pub(crate) fn canvas_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    if model.workspace.read(cx).project_id.is_none() {
        return crate::widgets::empty_panel(
            Icon::Network,
            "Open a project to draw its architecture",
            "Drawings stay with their project and are saved as JSON and SVG.",
        );
    }
    let detail = detail(model, cx);
    if model.canvas.list_collapsed {
        return detail;
    }
    let master = master(model, cx);
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .child(crate::split_pane::split_pane(
            "canvas-columns",
            &model.canvas.split,
            master,
            detail,
            cx,
        ))
        .into_any_element()
}

fn action(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    command: Action,
    enabled: bool,
    selected: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    btn(
        id,
        label,
        if selected {
            ButtonVariant::Selected
        } else {
            ButtonVariant::Ghost
        },
        enabled,
        cx,
        move |model, cx| model.canvas_action(command.clone(), cx),
    )
}

/// Compact, keyboard-readable controls: the glyph identifies the tool and the full
/// name stays available in the native hover tooltip.
#[allow(clippy::too_many_arguments)]
fn tool_control(
    id: impl Into<SharedString>,
    glyph: Icon,
    name: &'static str,
    shortcut: &'static str,
    command: Action,
    enabled: bool,
    selected: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let glyph_debug = format!("{id}:icon");
    let key_debug = format!("{id}:key");
    let ink = if selected {
        theme::accent()
    } else {
        theme::text_muted()
    };
    let tooltip = crate::widgets::Tooltip::new(name).with_shortcut(shortcut);
    crate::widgets::button_frame(
        div().id(id).debug_selector(move || debug_id.to_string()),
        if selected {
            ButtonVariant::Selected
        } else {
            ButtonVariant::Ghost
        },
        enabled,
    )
    .w(px(56.))
    .h(px(36.))
    .gap(px(6.))
    .tab_index(0)
    .tooltip(move |_, cx| cx.new(|_| tooltip.clone()).into())
    .when(enabled, |button| {
        button.on_click(cx.listener(move |model, _, _, cx| {
            model.canvas_action(command.clone(), cx);
        }))
    })
    .child(
        div()
            .debug_selector(move || glyph_debug.clone())
            .flex()
            .items_center()
            .child(crate::widgets::icon(glyph, px(18.), ink)),
    )
    .child(
        div()
            .debug_selector(move || key_debug.clone())
            .text_xs()
            .font_family(theme::FONT_MONO)
            .child(shortcut),
    )
    .into_any_element()
}

fn master(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let ui = &model.canvas;
    let query = ui.search.read(cx).value().to_lowercase();
    let enabled = !ui.busy && !model.canvas_project_switching;
    let mut column = div()
        .id("canvas-drawings")
        .h_full()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Project drawings"),
                )
                .child(action(
                    "canvas-collapse-list",
                    "Hide",
                    Action::ToggleList,
                    true,
                    false,
                    cx,
                )),
        )
        .child(action(
            "canvas-new-drawing",
            "New drawing",
            Action::New,
            enabled,
            false,
            cx,
        ))
        .child(ui.search.clone());
    let mut list = div()
        .id("canvas-drawing-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_1();
    for drawing in ui
        .drawings
        .iter()
        .filter(|d| d.title.to_lowercase().contains(&query))
    {
        let id = drawing.id.clone();
        let selected = ui.active.as_ref().is_some_and(|d| d.id == id);
        list = list.child(
            div()
                .id(SharedString::from(format!("canvas-drawing:{id}")))
                .debug_selector({
                    let id = id.clone();
                    move || format!("canvas-drawing:{id}")
                })
                .p_2()
                .rounded_md()
                .cursor_pointer()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .bg(if selected {
                    theme::accent_soft()
                } else {
                    theme::surface()
                })
                .hover(|s| s.bg(theme::surface_hover()))
                .child(
                    div()
                        .text_sm()
                        .text_color(if selected {
                            theme::accent()
                        } else {
                            theme::text()
                        })
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(drawing.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_subtle())
                        .child(drawing.sync_status.label()),
                )
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |model, _, _, cx| {
                        model.canvas_action(Action::Open(id.clone()), cx)
                    }))
                }),
        );
    }
    if ui.drawings.is_empty() {
        list = list.child(div().p_2().text_sm().text_color(theme::text_muted()).child(
            if ui.busy {
                "Loading drawings…"
            } else {
                "Create a drawing to start."
            },
        ));
    }
    column = column.child(list).child(
        div()
            .text_xs()
            .text_color(theme::text_subtle())
            .child("Local files: .hvc/drawings/"),
    );
    column.into_any_element()
}

fn detail(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let ui = &model.canvas;
    let enabled = ui.active.is_some() && !ui.busy && !model.canvas_project_switching;
    let mut panel = div()
        .id("canvas-panel")
        .h_full()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_hidden();
    let mut header = div()
        .flex()
        .items_center()
        .flex_wrap()
        .gap_1()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(theme::border());
    if ui.list_collapsed {
        header = header.child(action(
            "canvas-show-list",
            "Drawings",
            Action::ToggleList,
            true,
            false,
            cx,
        ));
    }
    header = header.child(
        div()
            .flex_1()
            .min_w_0()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .child(
                ui.active
                    .as_ref()
                    .map_or("Canvas", |d| d.title.as_str())
                    .to_owned(),
            ),
    );
    for (id, label, command) in [
        ("canvas-rename", "Rename", Action::BeginRename),
        ("canvas-duplicate", "Duplicate", Action::Duplicate),
        ("canvas-export", "Export SVG/JSON", Action::RetryFiles),
        ("canvas-delete", "Delete", Action::Delete),
    ] {
        header = header.child(action(id, label, command, enabled, false, cx));
    }
    panel = panel.child(header);
    if ui.renaming {
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .child(div().flex_1().min_w_0().child(ui.title.clone()))
                .child(action(
                    "canvas-apply-title",
                    "Apply title",
                    Action::Rename,
                    enabled,
                    false,
                    cx,
                )),
        );
    }
    if ui.confirm_delete {
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .px_3()
                .py_2()
                .bg(theme::danger_soft())
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::danger())
                        .child("Delete this drawing and its managed files?"),
                )
                .child(btn(
                    "canvas-confirm-delete",
                    "Delete drawing",
                    ButtonVariant::Danger,
                    enabled,
                    cx,
                    |m, cx| m.canvas_action(Action::ConfirmDelete, cx),
                ))
                .child(action(
                    "canvas-cancel-delete",
                    "Cancel",
                    Action::Delete,
                    true,
                    false,
                    cx,
                )),
        );
    }
    if let Some(error) = &ui.error {
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .px_3()
                .py_2()
                .bg(theme::danger_soft())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(theme::danger())
                        .child(error.clone()),
                )
                .child(action(
                    "canvas-retry-save",
                    "Retry save",
                    Action::Save,
                    !ui.saving,
                    false,
                    cx,
                ))
                .child(action(
                    "canvas-discard-draft",
                    "Discard changes",
                    Action::Discard,
                    !ui.saving && !ui.busy,
                    false,
                    cx,
                )),
        );
    }
    if let Some(notice) = &ui.notice {
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap_2()
                .px_3()
                .py_1()
                .bg(theme::warning_soft())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::warning())
                        .child(notice.clone()),
                )
                .child(action(
                    "canvas-retry-files",
                    "Retry files",
                    Action::RetryFiles,
                    !ui.syncing,
                    false,
                    cx,
                ))
                .child(action(
                    "canvas-export-copy",
                    "Export copy",
                    Action::ExportCopy,
                    enabled,
                    false,
                    cx,
                )),
        );
    }
    if ui.active.is_none() {
        return panel.child(crate::widgets::empty_panel(Icon::Network,"Think through a system with shapes",
            "Create a drawing, then add components, decisions and connections. No model or network connection is required.")).into_any_element();
    }
    let mut tools = div()
        .id("canvas-tools")
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .px_2()
        .py_1();
    for (index, (tool, _)) in Tool::ALL.into_iter().enumerate() {
        if ui.editor.scene.document.is_some()
            && !matches!(tool, Tool::Select | Tool::Hand | Tool::Eraser)
        {
            continue;
        }
        tools = tools.child(tool_control(
            format!("canvas-tool-{index}"),
            tool.icon(),
            tool.name(),
            tool.shortcut(),
            Action::Tool(tool),
            enabled,
            ui.editor.tool == tool,
            cx,
        ));
    }
    tools = tools.child(tool_control(
        "canvas-tool-lock",
        if ui.editor.locked {
            Icon::Lock
        } else {
            Icon::LockOpen
        },
        if ui.editor.locked {
            "Tool locked"
        } else {
            "Keep tool active"
        },
        "Q",
        Action::Lock,
        enabled,
        ui.editor.locked,
        cx,
    ));
    panel = panel.child(tools).child(styles(model, cx));
    if ui.editor.scene.document.is_some() {
        panel = panel.child(semantic_controls(model, cx));
    }
    panel = panel.child(surface(model, cx));
    panel
        .child(
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .justify_between()
                .gap_1()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(action(
                            "canvas-zoom-out",
                            "−",
                            Action::Zoom(0.8),
                            enabled,
                            false,
                            cx,
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child(format!("{:.0}%", ui.editor.viewport.zoom * 100.)),
                        )
                        .child(action(
                            "canvas-zoom-in",
                            "+",
                            Action::Zoom(1.25),
                            enabled,
                            false,
                            cx,
                        ))
                        .child(action("canvas-fit", "Fit", Action::Fit, enabled, false, cx))
                        .child(action(
                            "canvas-undo",
                            "Undo",
                            Action::Undo,
                            enabled && ui.editor.can_undo(),
                            false,
                            cx,
                        ))
                        .child(action(
                            "canvas-redo",
                            "Redo",
                            Action::Redo,
                            enabled && ui.editor.can_redo(),
                            false,
                            cx,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(if ui.editor.dirty {
                                    theme::warning()
                                } else {
                                    theme::text_muted()
                                })
                                .child(if ui.redaction_notice {
                                    format!("{} · secrets redacted", ui.status())
                                } else {
                                    ui.status().to_owned()
                                }),
                        )
                        .child(action(
                            "canvas-save",
                            "Save",
                            Action::Save,
                            enabled && !ui.saving,
                            false,
                            cx,
                        )),
                ),
        )
        .into_any_element()
}

fn semantic_controls(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use crate::canvas::state::semantic_editing::semantic_name;
    use ikore_core::companion_drawings::semantics::{
        CanvasProfileV1, CanvasSemanticKindV1 as Semantic,
    };
    let editor = &model.canvas.editor;
    let document = editor.scene.document.as_ref().expect("semantic document");
    let selected = editor.selected_semantic();
    let kind = selected.map_or(editor.creation_kind, |e| e.kind);
    let enabled = !model.canvas.busy && !model.canvas_project_switching;
    let value_label = |id: Option<&str>| {
        id.and_then(|id| editor.scene.elements.iter().find(|e| e.id == id))
            .map(|e| {
                if e.label.is_empty() {
                    e.id[..e.id.len().min(8)].to_owned()
                } else {
                    e.label.clone()
                }
            })
            .unwrap_or_else(|| "None".into())
    };
    let mut row = div()
        .id("canvas-semantic-controls")
        .debug_selector(|| "canvas-semantic-controls".into())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(match document.profile {
                    CanvasProfileV1::Architecture => "Architecture",
                    CanvasProfileV1::Bpmn => "Process · HVC BPMN profile",
                }),
        )
        .child(crate::select_menu::compact_control(
            model.canvas_choice_id("create"),
            "Add",
            semantic_name(editor.creation_kind),
            enabled,
            cx,
        ));
    if selected.is_some() {
        row = row.child(crate::select_menu::compact_control(
            model.canvas_choice_id("type"),
            "Type",
            semantic_name(kind),
            enabled,
            cx,
        ));
    }
    if document.profile == CanvasProfileV1::Bpmn
        && matches!(
            kind,
            Semantic::Task
                | Semantic::UserTask
                | Semantic::ServiceTask
                | Semantic::StartEvent
                | Semantic::EndEvent
                | Semantic::ExclusiveGateway
                | Semantic::ParallelGateway
                | Semantic::Lane
        )
    {
        let participant = selected
            .map(|e| e.participant_id.as_deref())
            .unwrap_or(editor.participant_id.as_deref());
        row = row.child(crate::select_menu::compact_control(
            model.canvas_choice_id("participant"),
            "Pool",
            value_label(participant),
            enabled,
            cx,
        ));
        if kind != Semantic::Lane {
            let lane = selected
                .map(|e| e.lane_id.as_deref())
                .unwrap_or(editor.lane_id.as_deref());
            row = row.child(crate::select_menu::compact_control(
                model.canvas_choice_id("lane"),
                "Lane",
                value_label(lane),
                enabled,
                cx,
            ));
        }
    }
    if matches!(
        kind,
        Semantic::Relationship
            | Semantic::SequenceFlow
            | Semantic::MessageFlow
            | Semantic::Association
    ) {
        for (field, label, default) in [
            ("source", "From", editor.semantic_source.as_deref()),
            ("target", "To", editor.semantic_target.as_deref()),
        ] {
            let value = selected
                .map(|e| {
                    if field == "source" {
                        e.source_id.as_deref()
                    } else {
                        e.target_id.as_deref()
                    }
                })
                .unwrap_or(default);
            row = row.child(crate::select_menu::compact_control(
                model.canvas_choice_id(field),
                label,
                value_label(value),
                enabled,
                cx,
            ));
        }
    }
    row = row.child(action(
        "canvas-explanation",
        "Explanation",
        Action::ToggleExplanation,
        true,
        model.canvas.show_explanation,
        cx,
    ));
    let mut panel = div().flex().flex_col().child(row);
    if !editor.diagnostics.is_empty() {
        let mut guidance = div()
            .id("canvas-semantic-guidance")
            .debug_selector(|| "canvas-semantic-guidance".into())
            .px_3()
            .py_1()
            .text_xs()
            .text_color(theme::warning())
            .flex()
            .flex_col()
            .gap_1()
            .child(format!(
                "{} guidance warning(s). These draft checks do not prove process correctness.",
                editor.diagnostics.len()
            ));
        for warning in editor.diagnostics.iter().take(3) {
            let label = warning
                .element_id
                .as_deref()
                .and_then(|id| editor.scene.elements.iter().find(|e| e.id == id))
                .map(|e| e.label.chars().take(80).collect::<String>())
                .filter(|label| !label.is_empty());
            guidance = guidance.child(match label {
                Some(label) => format!("{label}: {}", warning.message),
                None => warning.message.clone(),
            });
        }
        if editor.diagnostics.len() > 3 {
            guidance = guidance.child(format!(
                "{} additional guidance warning(s) remain.",
                editor.diagnostics.len() - 3
            ));
        }
        panel = panel.child(guidance);
    }
    if let Some(error) = &editor.semantic_error {
        panel = panel.child(
            div()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(theme::danger())
                .child(error.clone()),
        );
    }
    if model.canvas.show_explanation {
        let source = document
            .provenance
            .as_ref()
            .map(|p| {
                format!(
                    "Source {} · revision {}",
                    p.source_id,
                    p.source_revision
                        .map_or_else(|| "snapshot".into(), |r| r.to_string())
                )
            })
            .unwrap_or_else(|| "Manually authored document".into());
        panel = panel.child(
            div()
                .id("canvas-explanation-text")
                .max_h(px(140.))
                .overflow_y_scroll()
                .px_3()
                .py_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(source),
                )
                .child(div().text_sm().child(document.narrative.clone())),
        );
    }
    panel.into_any_element()
}

fn styles(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let ui = &model.canvas;
    let selected = ui.editor.selected_element();
    let color = selected.map_or(ui.editor.style.color, |e| e.stroke_color);
    let width = selected.map_or(ui.editor.style.width, |e| e.stroke_width);
    let stroke = selected.map_or(ui.editor.style.stroke, |e| e.stroke_style);
    let fill_enabled = selected.map_or(ui.editor.style.fill, |e| e.fill_enabled);
    let enabled = !ui.busy && !model.canvas_project_switching;
    let mut row = div()
        .id("canvas-styles")
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(theme::border());
    for (index, value) in Color::ALL.into_iter().enumerate() {
        row = row.child(
            div()
                .id(SharedString::from(format!("canvas-color-{index}")))
                .size(px(24.))
                .m_1()
                .rounded_full()
                .bg(rgb(value.rgb(ui.editor.scene.background_mode)))
                .border_2()
                .border_color(if value == color {
                    theme::accent()
                } else {
                    theme::border_strong()
                })
                .cursor_pointer()
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |model, _, _, cx| {
                        model.canvas_action(Action::Color(value), cx)
                    }))
                }),
        );
    }
    for (index, value, label) in [
        (0, Width::Thin, "Thin"),
        (1, Width::Medium, "Medium"),
        (2, Width::Bold, "Bold"),
    ] {
        row = row.child(action(
            format!("canvas-width-{index}"),
            label,
            Action::Width(value),
            enabled,
            width == value,
            cx,
        ));
    }
    for (index, value, label) in [
        (0, Stroke::Solid, "Solid"),
        (1, Stroke::Dashed, "Dashed"),
        (2, Stroke::Dotted, "Dotted"),
    ] {
        row = row.child(action(
            format!("canvas-stroke-{index}"),
            label,
            Action::Stroke(value),
            enabled,
            stroke == value,
            cx,
        ));
    }
    let rendering = selected.map_or(ui.editor.style.rendering, |e| e.stroke_rendering);
    for (index, value, label) in [
        (0, Rendering::Sketch, "Sketch"),
        (1, Rendering::Clean, "Clean"),
    ] {
        row = row.child(action(
            format!("canvas-rendering-{index}"),
            label,
            Action::Rendering(value),
            enabled,
            rendering == value,
            cx,
        ));
    }
    if selected.is_some_and(|e| e.shape.is_connector())
        || (selected.is_none() && matches!(ui.editor.tool, Tool::Arrow | Tool::Line))
    {
        let curve = selected.map_or(ui.editor.style.curve, |e| e.connector_curve);
        for (index, value, label) in [
            (0, Curve::Straight, "Straight"),
            (1, Curve::CurveLeft, "Curve left"),
            (2, Curve::CurveRight, "Curve right"),
        ] {
            row = row.child(action(
                format!("canvas-curve-{index}"),
                label,
                Action::Curve(value),
                enabled,
                curve == value,
                cx,
            ));
        }
    }
    row = row.child(action(
        "canvas-fill",
        "Fill",
        Action::Fill,
        enabled,
        fill_enabled,
        cx,
    ));
    if selected.is_some_and(|e| matches!(e.shape, Kind::Rectangle { .. }))
        || (selected.is_none() && ui.editor.tool == Tool::Rectangle)
    {
        let round = selected.map_or(ui.editor.style.round, |e| {
            matches!(e.shape, Kind::Rectangle { round: true })
        });
        row = row.child(action(
            "canvas-round",
            "Round",
            Action::Round,
            enabled,
            round,
            cx,
        ));
    }
    if selected.is_some_and(|e| matches!(e.shape, Kind::Arrow { .. }))
        || (selected.is_none() && ui.editor.tool == Tool::Arrow)
    {
        let both = selected.map_or(ui.editor.style.bidirectional, |e| {
            matches!(
                e.shape,
                Kind::Arrow {
                    bidirectional: true,
                    ..
                }
            )
        });
        row = row.child(action(
            "canvas-bidirectional",
            "↔",
            Action::Bidirectional,
            enabled,
            both,
            cx,
        ));
    }
    let font_size = selected.map_or(ui.editor.style.font_size, geometry::label_font_size);
    row = row.child(
        div()
            .flex()
            .items_center()
            .gap_1()
            .ml_2()
            .child(action(
                "canvas-text-smaller",
                "A−",
                Action::FontSize((font_size - 2.).max(8.)),
                enabled && font_size > 8.,
                false,
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!("{font_size:.0}")),
            )
            .child(action(
                "canvas-text-larger",
                "A+",
                Action::FontSize((font_size + 2.).min(96.)),
                enabled && font_size < 96.,
                false,
                cx,
            )),
    );
    row.child(action(
        "canvas-background",
        if ui.editor.scene.background_mode == CanvasBackgroundMode::Dark {
            "Light background"
        } else {
            "Dark background"
        },
        Action::Background,
        enabled,
        false,
        cx,
    ))
    .child(action(
        "canvas-grid",
        "Grid",
        Action::Grid,
        enabled,
        ui.editor.scene.show_grid,
        cx,
    ))
    .into_any_element()
}

fn surface(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let ui = &model.canvas;
    let scene = ui.editor.scene.clone();
    let viewport = ui.editor.viewport;
    let selected = ui.editor.selected.clone();
    let snap = ui.editor.snap_target.clone();
    let editing = ui.editing_label.clone();
    let measured = ui.bounds.clone();
    let mut surface = div()
        .id("canvas-surface")
        .debug_selector(|| "canvas-surface".into())
        .relative()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .track_focus(&ui.focus)
        .key_context("Canvas")
        .bg(rgb(scene.background_mode.rgb()))
        .cursor(match ui.editor.tool {
            Tool::Hand => gpui::CursorStyle::OpenHand,
            Tool::Select => gpui::CursorStyle::Arrow,
            _ => gpui::CursorStyle::Crosshair,
        })
        .on_key_down(cx.listener(|m, event, window, cx| m.canvas_key(event, window, cx)))
        .on_key_up(cx.listener(|m, event, _, cx| m.canvas_key_up(event, cx)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|m, event, window, cx| m.canvas_press(event, window, cx)),
        )
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(|m, event, window, cx| m.canvas_press(event, window, cx)),
        )
        .on_mouse_move(cx.listener(|m, event, _, cx| m.canvas_drag(event, cx)))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|m, _, window, cx| m.canvas_release(window, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|m, _, window, cx| m.canvas_release(window, cx)),
        )
        .on_mouse_up(
            MouseButton::Middle,
            cx.listener(|m, _, window, cx| m.canvas_release(window, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Middle,
            cx.listener(|m, _, window, cx| m.canvas_release(window, cx)),
        )
        .on_scroll_wheel(cx.listener(|m, event, window, cx| m.canvas_scroll(event, window, cx)))
        .child(
            canvas(
                move |bounds, _, _| {
                    measured.set(bounds);
                    (scene, viewport, selected, snap, editing)
                },
                |bounds, (scene, viewport, selected, snap, editing), window, cx| {
                    paint_scene(
                        bounds,
                        &scene,
                        &viewport,
                        selected.as_deref(),
                        snap.as_deref(),
                        editing.as_deref(),
                        window,
                        cx,
                    );
                },
            )
            .absolute()
            .size_full(),
        );
    if let Some(id) = &ui.editing_label
        && let Some(element) = ui.editor.scene.elements.iter().find(|e| &e.id == id)
    {
        let centre =
            geometry::world_to_screen(&viewport, geometry::element_bounds(element).centre());
        let panel = ui.bounds.get();
        let available = f32::from(panel.size.width).max(260.);
        let width = available.clamp(240., 420.) - 16.;
        let left = (centre[0] - width / 2.).clamp(8., (available - width - 8.).max(8.));
        let top = (centre[1] - 75.).clamp(8., (f32::from(panel.size.height) - 190.).max(8.));
        surface = surface.child(
            div()
                .id("canvas-label-editor")
                .key_context("CanvasLabel")
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(178.))
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(theme::accent())
                .bg(theme::surface_raised())
                .shadow_lg()
                .flex()
                .flex_col()
                .gap_1()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, cx| cx.stop_propagation()),
                )
                .on_action(cx.listener(AppModel::apply_canvas_label))
                .on_action(cx.listener(AppModel::newline_canvas_label))
                .on_action(cx.listener(AppModel::cancel_canvas_label))
                .on_action(cx.listener(AppModel::save_canvas_label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Enter applies · Shift+Enter adds a line · Esc cancels"),
                )
                .child(div().flex_1().min_h_0().child(ui.label.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(btn(
                            "canvas-apply-label",
                            "Apply",
                            ButtonVariant::Tinted,
                            true,
                            cx,
                            |m, cx| m.commit_canvas_label(cx),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_subtle())
                                .child("Enter applies without adding a line"),
                        ),
                ),
        );
    }
    surface.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn paint_scene(
    bounds: Bounds<Pixels>,
    scene: &Scene,
    view: &Viewport,
    selected: Option<&str>,
    snap: Option<&str>,
    editing: Option<&str>,
    window: &mut Window,
    cx: &mut App,
) {
    if scene.show_grid {
        crate::canvas::grid::paint_grid(bounds, view, scene.background_mode, window);
    }
    let visible = geometry::Bounds {
        x: -view.pan_x / view.zoom,
        y: -view.pan_y / view.zoom,
        width: f32::from(bounds.size.width) / view.zoom,
        height: f32::from(bounds.size.height) / view.zoom,
    };
    for element in geometry::paint_order(scene) {
        let semantic_kind = scene
            .document
            .as_ref()
            .and_then(|d| d.elements.iter().find(|s| s.element_id == element.id))
            .map(|s| s.kind);
        if !geometry::ink_bounds(element)
            .union(geometry::label_bounds(element))
            .expanded(20.)
            .intersects(visible)
        {
            continue;
        }
        let color = rgb(element.stroke_color.rgb(scene.background_mode));
        if element.fill_enabled
            && !element.shape.is_connector()
            && !matches!(element.shape, Kind::Text { .. })
        {
            let mut builder = PathBuilder::fill();
            shape_path(&mut builder, element, view, bounds);
            if let Ok(path) = builder.build() {
                window.paint_path(path, gpui::Rgba { a: 0.18, ..color });
            }
        }
        if !matches!(element.shape, Kind::Text { .. }) {
            use ikore_core::companion_drawings::semantics::CanvasSemanticKindV1 as Semantic;
            let stroke = match semantic_kind {
                Some(Semantic::MessageFlow) => Stroke::Dashed,
                Some(Semantic::Association) => Stroke::Dotted,
                _ => element.stroke_style,
            };
            let line_width = if semantic_kind == Some(Semantic::EndEvent) {
                element.stroke_width.units().max(4.)
            } else {
                element.stroke_width.units()
            };
            let mut builder = PathBuilder::stroke(px(line_width * view.zoom));
            builder = match stroke {
                Stroke::Solid => builder,
                Stroke::Dashed => builder.dash_array(&[px(8. * view.zoom), px(6. * view.zoom)]),
                Stroke::Dotted => builder.dash_array(&[px(view.zoom), px(5. * view.zoom)]),
            };
            shape_path(&mut builder, element, view, bounds);
            if let Ok(path) = builder.build() {
                window.paint_path(path, color);
            }
            if let Kind::Arrow { bidirectional, .. } = element.shape
                && let Some([start, end]) = geometry::connector_world_points(element)
            {
                if element.stroke_rendering == Rendering::Sketch
                    || element.connector_curve != Curve::Straight
                {
                    paint_styled_arrowhead(element, false, view, bounds, color, window);
                    if bidirectional {
                        paint_styled_arrowhead(element, true, view, bounds, color, window);
                    }
                } else {
                    paint_arrowhead(
                        start,
                        end,
                        element.stroke_width.units(),
                        view,
                        bounds,
                        color,
                        window,
                    );
                    if bidirectional {
                        paint_arrowhead(
                            end,
                            start,
                            element.stroke_width.units(),
                            view,
                            bounds,
                            color,
                            window,
                        );
                    }
                }
            }
        }
        if let Some(kind) = semantic_kind {
            paint_semantic_marker(kind, element, view, bounds, color, window, cx);
        }
        if editing != Some(element.id.as_str()) && !element.label.is_empty() {
            let font_size = geometry::label_font_size(element) * view.zoom;
            let label = geometry::label_bounds(element);
            let is_text = matches!(element.shape, Kind::Text { .. });
            for (index, text) in geometry::label_lines(element).iter().enumerate() {
                let run = TextRun {
                    len: text.len(),
                    font: font(theme::FONT_UI),
                    color: color.into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(
                    (*text).to_owned().into(),
                    px(font_size),
                    &[run],
                    None,
                );
                let centre = geometry::world_to_screen(view, [label.centre()[0], label.y]);
                let x = if is_text {
                    element.x * view.zoom + view.pan_x
                } else {
                    centre[0] - f32::from(line.width) / 2.
                };
                let y = centre[1] + index as f32 * font_size * 1.3;
                let _ = line.paint(
                    point(bounds.left() + px(x), bounds.top() + px(y)),
                    px(font_size * 1.3),
                    window,
                    cx,
                );
            }
        }
        if selected == Some(element.id.as_str()) || snap == Some(element.id.as_str()) {
            let rect = geometry::element_bounds(element).expanded(4. / view.zoom);
            let origin = geometry::world_to_screen(view, [rect.x, rect.y]);
            let outline = Bounds::new(
                point(bounds.left() + px(origin[0]), bounds.top() + px(origin[1])),
                size(px(rect.width * view.zoom), px(rect.height * view.zoom)),
            );
            window.paint_quad(gpui::outline(outline, theme::accent(), Default::default()));
            if selected == Some(element.id.as_str()) {
                for handle in EditorState::handles(element) {
                    let p = geometry::world_to_screen(view, handle);
                    let handle_bounds = Bounds::new(
                        point(bounds.left() + px(p[0] - 4.), bounds.top() + px(p[1] - 4.)),
                        size(px(8.), px(8.)),
                    );
                    window.paint_quad(fill(handle_bounds, rgb(scene.background_mode.rgb())));
                    window.paint_quad(gpui::outline(
                        handle_bounds,
                        theme::accent(),
                        Default::default(),
                    ));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_semantic_marker(
    kind: ikore_core::companion_drawings::semantics::CanvasSemanticKindV1,
    element: &Element,
    view: &Viewport,
    bounds: Bounds<Pixels>,
    color: gpui::Rgba,
    window: &mut Window,
    cx: &mut App,
) {
    use ikore_core::companion_drawings::semantics::CanvasSemanticKindV1 as Semantic;
    let marker = match kind {
        Semantic::ExclusiveGateway => "×",
        Semantic::ParallelGateway => "+",
        Semantic::UserTask => "User",
        Semantic::ServiceTask => "Service",
        Semantic::Participant => "Pool",
        Semantic::Lane => "Lane",
        _ => return,
    };
    let gateway = matches!(kind, Semantic::ExclusiveGateway | Semantic::ParallelGateway);
    let font_size = if gateway { 14. } else { 10. } * view.zoom;
    let run = TextRun {
        len: marker.len(),
        font: font(theme::FONT_UI),
        color: color.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(marker.into(), px(font_size), &[run], None);
    let position = geometry::world_to_screen(
        view,
        if gateway {
            [
                element.x + element.width / 2.,
                element.y + element.height * 0.2,
            ]
        } else {
            [element.x + 6., element.y + 3.]
        },
    );
    let x = if gateway {
        position[0] - f32::from(line.width) / 2.
    } else {
        position[0]
    };
    let _ = line.paint(
        point(bounds.left() + px(x), bounds.top() + px(position[1])),
        px(font_size * 1.2),
        window,
        cx,
    );
}

fn shape_path(path: &mut PathBuilder, element: &Element, view: &Viewport, bounds: Bounds<Pixels>) {
    if element.stroke_rendering == Rendering::Sketch
        || (element.shape.is_connector() && element.connector_curve != Curve::Straight)
    {
        vector_path(path, &visual_geometry::element_path(element), view, bounds);
        return;
    }
    let p = |x: f32, y: f32| {
        let p = geometry::world_to_screen(view, [x, y]);
        point(bounds.left() + px(p[0]), bounds.top() + px(p[1]))
    };
    let (x, y, w, h) = (element.x, element.y, element.width, element.height);
    match &element.shape {
        Kind::Rectangle { round: false } => {
            path.add_polygon(&[p(x, y), p(x + w, y), p(x + w, y + h), p(x, y + h)], true)
        }
        Kind::Rectangle { round: true } => {
            let r = 12_f32.min(w / 4.).min(h / 4.);
            let radii = point(px(r * view.zoom), px(r * view.zoom));
            path.move_to(p(x + r, y));
            path.line_to(p(x + w - r, y));
            path.arc_to(radii, px(0.), false, true, p(x + w, y + r));
            path.line_to(p(x + w, y + h - r));
            path.arc_to(radii, px(0.), false, true, p(x + w - r, y + h));
            path.line_to(p(x + r, y + h));
            path.arc_to(radii, px(0.), false, true, p(x, y + h - r));
            path.line_to(p(x, y + r));
            path.arc_to(radii, px(0.), false, true, p(x + r, y));
            path.close();
        }
        Kind::Diamond => path.add_polygon(
            &[
                p(x + w / 2., y),
                p(x + w, y + h / 2.),
                p(x + w / 2., y + h),
                p(x, y + h / 2.),
            ],
            true,
        ),
        Kind::Ellipse => {
            let radii = point(px(w * view.zoom / 2.), px(h * view.zoom / 2.));
            path.move_to(p(x, y + h / 2.));
            path.arc_to(radii, px(0.), false, true, p(x + w, y + h / 2.));
            path.arc_to(radii, px(0.), false, true, p(x, y + h / 2.));
            path.close();
        }
        Kind::Arrow { .. } | Kind::Line { .. } => {
            if let Some([a, b]) = geometry::connector_world_points(element) {
                path.move_to(p(a[0], a[1]));
                path.line_to(p(b[0], b[1]));
            }
        }
        Kind::Text { .. } => {}
    }
}

fn vector_path(
    path: &mut PathBuilder,
    vector: &visual_geometry::VectorPath,
    view: &Viewport,
    bounds: Bounds<Pixels>,
) {
    let p = |world| {
        let screen = geometry::world_to_screen(view, world);
        point(bounds.left() + px(screen[0]), bounds.top() + px(screen[1]))
    };
    for command in &vector.commands {
        match command {
            visual_geometry::Command::Move(a) => path.move_to(p(*a)),
            visual_geometry::Command::Line(a) => path.line_to(p(*a)),
            visual_geometry::Command::Quadratic(control, end) => {
                path.curve_to(p(*end), p(*control))
            }
            visual_geometry::Command::Cubic(a, b, end) => {
                path.cubic_bezier_to(p(*end), p(*a), p(*b))
            }
            visual_geometry::Command::Close => path.close(),
        }
    }
}

fn paint_styled_arrowhead(
    element: &Element,
    at_start: bool,
    view: &Viewport,
    bounds: Bounds<Pixels>,
    color: gpui::Rgba,
    window: &mut Window,
) {
    let vector = visual_geometry::arrowhead_path(element, at_start);
    let mut builder = if vector.closed {
        PathBuilder::fill()
    } else {
        PathBuilder::stroke(px(element.stroke_width.units() * view.zoom))
    };
    vector_path(&mut builder, &vector, view, bounds);
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_arrowhead(
    start: [f32; 2],
    end: [f32; 2],
    width: f32,
    view: &Viewport,
    bounds: Bounds<Pixels>,
    color: gpui::Rgba,
    window: &mut Window,
) {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let length = dx.hypot(dy);
    if length < 0.001 {
        return;
    }
    let direction = [dx / length, dy / length];
    let head = (width * 3.5).max(10.).min(length / 2.);
    let base = [end[0] - direction[0] * head, end[1] - direction[1] * head];
    let p = |p: [f32; 2]| {
        let p = geometry::world_to_screen(view, p);
        point(bounds.left() + px(p[0]), bounds.top() + px(p[1]))
    };
    let mut path = PathBuilder::fill();
    path.add_polygon(
        &[
            p(end),
            p([
                base[0] - direction[1] * head * 0.45,
                base[1] + direction[0] * head * 0.45,
            ]),
            p([
                base[0] + direction[1] * head * 0.45,
                base[1] - direction[0] * head * 0.45,
            ]),
        ],
        true,
    );
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}
