use super::*;
use crate::AppView;
use gpui::TestAppContext;

#[gpui::test]
fn authored_open_keeps_the_requested_drawing_while_a_refresh_is_busy(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    model.update(visual, |model, cx| {
        let project = model.canvas_project(cx).unwrap();
        model.canvas.project_id = Some(project.clone());
        model.canvas.busy = true;
        model.open_authored_canvas(project, "requested-drawing".into(), cx);
        assert_eq!(model.active_view(), AppView::Canvas);
        assert!(matches!(&model.canvas.pending_action, Some(CanvasAction::Open(id)) if id=="requested-drawing"));
    });
}

#[gpui::test]
fn authored_open_cannot_navigate_to_a_foreign_project(cx: &mut TestAppContext) {
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    model.update(visual, |model, cx| {
        let previous = model.active_view();
        model.open_authored_canvas("foreign-project".into(), "drawing".into(), cx);
        assert_eq!(model.active_view(), previous);
        assert!(model.canvas.pending_action.is_none());
    });
}

#[test]
fn an_explicit_open_never_falls_back_to_a_different_drawing() {
    let drawings = vec![CompanionDrawingSummary {
        id: "another".into(),
        project_id: "project".into(),
        title: "Another".into(),
        revision: 1,
        basename: "another".into(),
        created_at: "now".into(),
        updated_at: "now".into(),
        sync_status: ikore_core::companion_drawings::DrawingSyncStatus::Synced,
    }];
    assert!(resolve_canvas_selection(Some("deleted"), Some("another".into()), &drawings).is_err());
    assert_eq!(
        resolve_canvas_selection(Some("another"), None, &drawings).unwrap(),
        Some("another".into())
    );
    assert_eq!(
        resolve_canvas_selection(None, None, &drawings).unwrap(),
        Some("another".into())
    );
}

#[gpui::test]
fn semantic_controls_use_explicit_types_and_reject_stale_drawing_menus(cx: &mut TestAppContext) {
    use ikore_core::companion_drawings::semantics::{
        CanvasDocumentV1, CanvasProfileV1, CanvasSemanticKindV1,
    };
    use ikore_core::companion_drawings::{CanvasSceneV1, CanvasViewportV1, DrawingSyncStatus};
    let (model, visual) = cx.add_window_view(|_, cx| crate::layout_tests::fixture(cx));
    model.update(visual, |model, cx| {
        let scene = CanvasSceneV1 {
            document: Some(CanvasDocumentV1 {
                schema_version: 1,
                profile: CanvasProfileV1::Architecture,
                language: "pt-BR".into(),
                narrative: "Explica\u{e7}\u{e3}o do sistema".into(),
                provenance: None,
                elements: vec![],
            }),
            ..CanvasSceneV1::default()
        };
        model.canvas.active = Some(CompanionDrawing {
            id: "semantic-fixture".into(),
            project_id: "layout-project".into(),
            title: "Architecture".into(),
            scene: scene.clone(),
            viewport: CanvasViewportV1::default(),
            revision: 1,
            basename: "semantic-fixture".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            sync_status: DrawingSyncStatus::Synced,
        });
        model.canvas.project_id = Some("layout-project".into());
        model.canvas.editor.load(scene, CanvasViewportV1::default());
        model.select_view(AppView::Canvas, cx);
        let menu = model.canvas_choice_id("create");
        let boundary = model
            .canvas_choices(&menu)
            .into_iter()
            .find(|choice| choice.label == "Boundary")
            .unwrap();
        model.apply_choice(&menu, &boundary.value, cx);
        assert_eq!(
            model.canvas.editor.creation_kind,
            CanvasSemanticKindV1::Boundary
        );
        assert_eq!(model.canvas.editor.tool, Tool::Rectangle);
        model.canvas.active.as_mut().unwrap().id = "different-drawing".into();
        assert!(model.canvas_choices(&menu).is_empty());
        model.apply_choice(&menu, "0", cx);
        assert_eq!(
            model.canvas.editor.creation_kind,
            CanvasSemanticKindV1::Boundary
        );
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("canvas-semantic-controls").is_some());
    assert!(visual.debug_bounds("canvas-semantic-guidance").is_some());
    model.update(visual, |model, cx| {
        assert_eq!(model.canvas.editor.diagnostics.len(), 1);
        assert!(model.canvas.editor.mutate(|scene, _| {
            let mut element = ikore_core::companion_drawings::CanvasElementV1::new(
                CanvasElementKindV1::Rectangle { round: false },
                20.,
                20.,
                140.,
                90.,
            );
            element.label = "Core".into();
            scene.elements.push(element);
        }));
        assert!(model.canvas.editor.diagnostics.is_empty());
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("canvas-semantic-guidance").is_none());
    model.update(visual, |model, cx| {
        assert!(model.canvas.editor.undo());
        assert_eq!(model.canvas.editor.diagnostics.len(), 1);
        cx.notify();
    });
    visual.run_until_parked();
    assert!(visual.debug_bounds("canvas-semantic-guidance").is_some());
}
