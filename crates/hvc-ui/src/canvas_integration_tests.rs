//! Native component regressions for navigation, pointer gestures and text focus.
use crate::{
    AppModel, AppView,
    canvas::{CanvasAction, Tool},
};
use gpui::{
    Entity, Focusable, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px, size,
};
use hvc_core::companion_drawings::{
    CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, CanvasViewportV1, CompanionDrawing,
    DrawingSyncStatus,
};

fn document() -> CompanionDrawing {
    CompanionDrawing {
        id: "drawing-fixture".into(),
        project_id: "layout-project".into(),
        title: "System design".into(),
        scene: CanvasSceneV1::default(),
        viewport: CanvasViewportV1::default(),
        revision: 1,
        basename: "system-design-00000000-0000-4000-8000-000000000001".into(),
        created_at: "2026-10-26T12:00:00Z".into(),
        updated_at: "2026-10-26T12:00:00Z".into(),
        sync_status: DrawingSyncStatus::Synced,
    }
}

fn install(model: &Entity<AppModel>, visual: &mut VisualTestContext, with_shape: bool) {
    model.update(visual, |model, cx| {
        model.select_view(AppView::Canvas, cx);
        let mut drawing = document();
        if with_shape {
            let mut shape = CanvasElementV1::new(
                CanvasElementKindV1::Rectangle { round: false },
                50.,
                50.,
                130.,
                80.,
            );
            shape.label = "Core".into();
            drawing.scene.elements.push(shape);
        }
        model.canvas.project_id = Some(drawing.project_id.clone());
        model.canvas.drawings = vec![drawing.summary()];
        model
            .canvas
            .editor
            .load(drawing.scene.clone(), drawing.viewport);
        model.canvas.active = Some(drawing);
        cx.notify();
    });
    visual.run_until_parked();
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

#[gpui::test]
fn canvas_layout_preserves_surface_and_controls_at_supported_sizes(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    for (width, height) in [(960., 640.), (1280., 800.)] {
        visual.simulate_resize(size(px(width), px(height)));
        install(&model, visual, true);
        let surface = visual.debug_bounds("canvas-surface").unwrap();
        let left = visual.debug_bounds("canvas-columns-left").unwrap();
        let footer = visual.debug_bounds("status-bar").unwrap();
        assert!(
            surface.size.width > px(250.),
            "surface too narrow: {surface:?}"
        );
        assert!(
            surface.size.height > px(140.),
            "surface too short: {surface:?}"
        );
        assert!(surface.left() >= left.right());
        assert!(surface.right() <= px(width) && surface.bottom() <= footer.top());
        for selector in [
            "canvas-new-drawing",
            "canvas-rename",
            "canvas-duplicate",
            "canvas-export",
            "canvas-delete",
        ] {
            let bounds = visual.debug_bounds(selector).unwrap();
            assert!(bounds.left() >= px(0.) && bounds.right() <= px(width));
            assert!(bounds.bottom() <= footer.top());
        }
    }
}

#[gpui::test]
fn canvas_pointer_gesture_and_undo_are_one_document_operation(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    visual.simulate_resize(size(px(1280.), px(800.)));
    install(&model, visual, false);
    let bounds = visual.debug_bounds("canvas-surface").unwrap();
    let start = bounds.origin + point(px(30.), px(30.));
    let end = bounds.origin + point(px(180.), px(110.));
    visual.simulate_click(start, Modifiers::default());
    visual.simulate_keystrokes("r");
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements.len()),
        1
    );
    visual.simulate_keystrokes("ctrl-z");
    visual.run_until_parked();
    assert!(model.read_with(visual, |model, _| {
        model.canvas.editor.scene.elements.is_empty()
    }));
    visual.simulate_keystrokes("ctrl-y");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements.len()),
        1
    );
}

#[gpui::test]
fn canvas_inputs_do_not_trigger_surface_shortcuts(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    install(&model, visual, true);
    model.update(visual, |model, cx| {
        model.canvas_action(CanvasAction::BeginRename, cx)
    });
    visual.run_until_parked();
    // Focus the actual native title input, then type tool shortcuts as ordinary text.
    visual.update(|window, cx| {
        model.update(cx, |model, cx| {
            model.canvas.title.read(cx).focus_handle(cx).focus(window);
        })
    });
    visual.simulate_input("r d o a");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.tool),
        Tool::Select
    );
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements.len()),
        1
    );
    assert!(model.read_with(visual, |model, cx| {
        model.canvas.title.read(cx).value().contains("r d o a")
    }));
}

#[gpui::test]
fn canvas_label_editor_keeps_unicode_and_local_undo(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    visual.update(|_, cx| {
        crate::native_editor::register_editor_keybindings(cx);
        crate::shell::register_navigation_keybindings(cx);
        crate::canvas::register_canvas_keybindings(cx);
    });
    install(&model, visual, true);
    let id = model.read_with(visual, |model, _| {
        model.canvas.editor.scene.elements[0].id.clone()
    });
    visual.update(|window, cx| {
        model.update(cx, |model, cx| model.start_canvas_label(id, window, cx))
    });
    visual.run_until_parked();
    visual.simulate_keystrokes("ctrl-a");
    visual.simulate_input("Decis\u{e3}o → a\u{e7}\u{e3}o");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements[0]
            .label
            .clone()),
        "Core"
    );
    visual.simulate_keystrokes("ctrl-enter");
    visual.run_until_parked();
    assert!(
        !model.read_with(visual, |model, _| model.forge.new_open),
        "applying a label must never open the global New work dialog"
    );
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements[0]
            .label
            .clone()),
        "Decis\u{e3}o → a\u{e7}\u{e3}o"
    );
    visual.simulate_keystrokes("ctrl-z");
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements[0]
            .label
            .clone()),
        "Core"
    );
}

#[gpui::test]
fn canvas_close_refuses_to_discard_a_failed_or_pending_commit(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    install(&model, visual, true);
    model.update(visual, |model, cx| {
        model.canvas_action(CanvasAction::Background, cx)
    });
    visual.update(|window, cx| {
        model.update(cx, |model, cx| {
            // No persistence backend in the component fixture: this draft cannot commit.
            assert!(!model.request_canvas_close(window, cx));
            assert!(model.canvas_has_pending());
            assert!(model.canvas.editor.dirty);
            assert_eq!(model.canvas.editor.scene.elements.len(), 1);
        })
    });
    visual.run_until_parked();
    assert!(model.read_with(visual, |model, _| model.canvas_close_requested));
    click(visual, "rail-nav-home");
    assert!(model.read_with(visual, |model, _| model.canvas.editor.dirty));
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.scene.elements.len()),
        1
    );
}
