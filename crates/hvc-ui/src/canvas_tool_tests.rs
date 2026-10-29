//! Toolbar regressions: embedded glyphs, visible keycaps, hover help and tool dispatch.
use gpui::{AssetSource, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use hvc_core::companion_drawings::{
    CanvasSceneV1, CanvasViewportV1, CompanionDrawing, DrawingSyncStatus,
};

use crate::{AppModel, AppView, assets::HvcAssets, canvas::Tool};

fn install(model: &Entity<AppModel>, visual: &mut VisualTestContext) {
    model.update(visual, |model, cx| {
        model.select_view(AppView::Canvas, cx);
        let drawing = CompanionDrawing {
            id: "toolbar-drawing".into(),
            project_id: "layout-project".into(),
            title: "Toolbar fixture".into(),
            scene: CanvasSceneV1::default(),
            viewport: CanvasViewportV1::default(),
            revision: 1,
            basename: "toolbar-00000000-0000-4000-8000-000000000001".into(),
            created_at: "2026-10-27T12:00:00Z".into(),
            updated_at: "2026-10-27T12:00:00Z".into(),
            sync_status: DrawingSyncStatus::Synced,
        };
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

#[test]
fn canvas_tools_have_distinct_glyphs_names_and_shortcuts() {
    let mut paths = std::collections::HashSet::new();
    let mut shortcuts = std::collections::HashSet::new();
    for (tool, old_label) in Tool::ALL {
        assert!(paths.insert(tool.icon().path()), "duplicate tool glyph");
        assert!(shortcuts.insert(tool.shortcut()), "duplicate tool key");
        assert_eq!(old_label, format!("{} · {}", tool.name(), tool.shortcut()));
        let svg = HvcAssets.load(tool.icon().path()).unwrap().unwrap();
        let svg = std::str::from_utf8(&svg).unwrap();
        assert!(svg.contains("viewBox=\"0 0 24 24\""));
        assert!(svg.contains("stroke=\"currentColor\""));
        assert!(!svg.contains("<image") && !svg.contains("href="));
    }
}

#[gpui::test]
fn canvas_tool_buttons_show_glyph_and_key_and_select_each_tool(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    visual.simulate_resize(size(px(1280.), px(800.)));
    install(&model, visual);
    for ((tool, _), (button, icon, key)) in Tool::ALL.into_iter().zip([
        ("canvas-tool-0", "canvas-tool-0:icon", "canvas-tool-0:key"),
        ("canvas-tool-1", "canvas-tool-1:icon", "canvas-tool-1:key"),
        ("canvas-tool-2", "canvas-tool-2:icon", "canvas-tool-2:key"),
        ("canvas-tool-3", "canvas-tool-3:icon", "canvas-tool-3:key"),
        ("canvas-tool-4", "canvas-tool-4:icon", "canvas-tool-4:key"),
        ("canvas-tool-5", "canvas-tool-5:icon", "canvas-tool-5:key"),
        ("canvas-tool-6", "canvas-tool-6:icon", "canvas-tool-6:key"),
        ("canvas-tool-7", "canvas-tool-7:icon", "canvas-tool-7:key"),
        ("canvas-tool-8", "canvas-tool-8:icon", "canvas-tool-8:key"),
    ]) {
        let bounds = visual.debug_bounds(button).unwrap();
        assert_eq!(bounds.size.width, px(56.));
        assert!(visual.debug_bounds(icon).unwrap().size.width >= px(18.));
        assert!(visual.debug_bounds(key).unwrap().size.width > px(0.));
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        assert_eq!(
            model.read_with(visual, |model, _| model.canvas.editor.tool),
            tool
        );
    }
    let bounds = visual.debug_bounds("canvas-tool-lock").unwrap();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
    assert!(model.read_with(visual, |model, _| model.canvas.editor.locked));
}

#[gpui::test]
fn canvas_tool_hover_opens_help_and_disabled_controls_do_not_dispatch(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    visual.simulate_resize(size(px(1280.), px(800.)));
    install(&model, visual);
    let bounds = visual.debug_bounds("canvas-tool-2").unwrap();
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    visual
        .executor()
        .advance_clock(std::time::Duration::from_millis(600));
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("tooltip").is_some(),
        "tool help must appear on hover"
    );
    model.update(visual, |model, cx| {
        model.canvas.busy = true;
        cx.notify();
    });
    visual.run_until_parked();
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
    assert_eq!(
        model.read_with(visual, |model, _| model.canvas.editor.tool),
        Tool::Select
    );
}
