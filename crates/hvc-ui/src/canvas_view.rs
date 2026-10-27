//! Native GPUI drawing surface and compact project document explorer.
use gpui::{
    AnyElement, App, Bounds, Context, FontWeight, MouseButton, PathBuilder, Pixels, SharedString,
    TextRun, Window, canvas, div, fill, font, point, prelude::*, px, rgb, size,
};
use hvc_core::companion_drawings::{
    CanvasBackgroundMode, CanvasElementKindV1 as Kind, CanvasElementV1 as Element,
    CanvasSceneV1 as Scene, CanvasSemanticColor as Color, CanvasStrokeStyle as Stroke,
    CanvasStrokeWidth as Width, CanvasViewportV1 as Viewport, geometry,
};

use crate::{
    AppModel,
    canvas::{CanvasAction as Action, EditorState, Tool},
    icons::Icon,
    theme,
    widgets::{ButtonVariant, btn},
};

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
    for (index, (tool, label)) in Tool::ALL.into_iter().enumerate() {
        tools = tools.child(action(
            format!("canvas-tool-{index}"),
            label,
            Action::Tool(tool),
            enabled,
            ui.editor.tool == tool,
            cx,
        ));
    }
    tools = tools.child(action(
        "canvas-tool-lock",
        if ui.editor.locked {
            "Locked · Q"
        } else {
            "Lock · Q"
        },
        Action::Lock,
        enabled,
        ui.editor.locked,
        cx,
    ));
    panel = panel.child(tools).child(styles(model, cx));
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
                .on_action(cx.listener(AppModel::cancel_canvas_label))
                .on_action(cx.listener(AppModel::save_canvas_label))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("Label · Ctrl+Enter to apply · Escape to cancel"),
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
                                .child("Enter adds a line"),
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
        let mut spacing = 24. * view.zoom;
        while spacing < 12. {
            spacing *= 2.;
        }
        let start_x = view.pan_x.rem_euclid(spacing);
        let start_y = view.pan_y.rem_euclid(spacing);
        let color = if scene.background_mode == CanvasBackgroundMode::Dark {
            rgb(0x1d2937)
        } else {
            rgb(0xd8dde5)
        };
        let mut y = start_y;
        while y < f32::from(bounds.size.height) {
            let mut x = start_x;
            while x < f32::from(bounds.size.width) {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + px(x), bounds.top() + px(y)),
                        size(px(1.), px(1.)),
                    ),
                    color,
                ));
                x += spacing;
            }
            y += spacing;
        }
    }
    let visible = geometry::Bounds {
        x: -view.pan_x / view.zoom,
        y: -view.pan_y / view.zoom,
        width: f32::from(bounds.size.width) / view.zoom,
        height: f32::from(bounds.size.height) / view.zoom,
    };
    for element in geometry::paint_order(scene) {
        if !geometry::element_bounds(element)
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
            let mut builder = PathBuilder::stroke(px(element.stroke_width.units() * view.zoom));
            builder = match element.stroke_style {
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
                let centre =
                    geometry::world_to_screen(view, [element.x + element.width / 2., label.y]);
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

fn shape_path(path: &mut PathBuilder, element: &Element, view: &Viewport, bounds: Bounds<Pixels>) {
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
