use super::*;
use ikore_core::companion_drawings::semantics::{
    CanvasDocumentV1, CanvasProfileV1, CanvasSemanticElementV1, CanvasSemanticKindV1 as Semantic,
};

fn editor(profile: CanvasProfileV1) -> EditorState {
    let mut state = EditorState::default();
    state.load(
        Scene {
            document: Some(CanvasDocumentV1 {
                schema_version: 1,
                profile,
                language: "pt-BR".into(),
                narrative: "Explicação".into(),
                provenance: None,
                elements: vec![],
            }),
            ..Scene::default()
        },
        Viewport::default(),
    );
    state
}

fn draw(state: &mut EditorState, kind: Semantic, start: [f32; 2], end: [f32; 2]) -> String {
    state.set_creation_kind(kind);
    state.press(start, false);
    state.drag(end);
    assert!(state.release().0, "{:?}", state.semantic_error);
    state.selected.clone().unwrap()
}

fn semantic(state: &EditorState, id: &str) -> CanvasSemanticElementV1 {
    state
        .scene
        .document
        .as_ref()
        .unwrap()
        .elements
        .iter()
        .find(|e| e.element_id == id)
        .unwrap()
        .clone()
}

#[test]
fn architecture_manual_gestures_preserve_semantics_and_history() {
    let mut state = editor(CanvasProfileV1::Architecture);
    let a = draw(&mut state, Semantic::Component, [20., 30.], [120., 100.]);
    let b = draw(&mut state, Semantic::Boundary, [240., 30.], [380., 120.]);
    let edge = draw(&mut state, Semantic::Relationship, [120., 60.], [240., 60.]);
    assert_eq!(
        semantic(&state, &edge).source_id.as_deref(),
        Some(a.as_str())
    );
    assert_eq!(
        semantic(&state, &edge).target_id.as_deref(),
        Some(b.as_str())
    );
    let completed = state.scene.clone();
    state.selected = Some(a.clone());
    assert!(state.delete_selection());
    assert_eq!(state.scene.elements.len(), 1);
    assert!(state.undo());
    assert_eq!(state.scene, completed);
    assert!(state.redo());
    assert_eq!(state.scene.elements.len(), 1);
    assert!(state.undo());
    assert!(state.mutate(|scene, _| {
        scene.elements.iter_mut().find(|e| e.id == a).unwrap().label = "Núcleo".into()
    }));
    let roundtrip: Scene =
        serde_json::from_str(&serde_json::to_string(&state.scene).unwrap()).unwrap();
    assert_eq!(roundtrip, state.scene);
    ikore_core::companion_drawings::normalize_scene(&roundtrip).unwrap();
}

#[test]
fn bpmn_creation_requires_explicit_membership_and_invalid_actions_are_atomic() {
    let mut state = editor(CanvasProfileV1::Bpmn);
    let pool = draw(&mut state, Semantic::Participant, [10., 10.], [450., 300.]);
    let before = state.scene.clone();
    state.set_creation_kind(Semantic::Task);
    state.press([40., 70.], false);
    state.drag([140., 140.]);
    assert!(!state.release().0);
    assert_eq!(state.scene, before);
    assert!(state.semantic_error.is_some());
    assert!(state.set_membership(Some(pool.clone()), None));
    let lane = draw(&mut state, Semantic::Lane, [20., 35.], [440., 280.]);
    state.set_creation_kind(Semantic::UserTask);
    assert!(state.set_membership(Some(pool.clone()), Some(lane.clone())));
    let task = draw(&mut state, Semantic::UserTask, [50., 70.], [150., 140.]);
    assert_eq!(
        semantic(&state, &task).participant_id.as_deref(),
        Some(pool.as_str())
    );
    assert_eq!(
        semantic(&state, &task).lane_id.as_deref(),
        Some(lane.as_str())
    );
    let before = state.scene.clone();
    state.selected = Some(pool);
    assert!(
        !state.delete_selection(),
        "a participant with members cannot be erased"
    );
    assert_eq!(state.scene, before);
    state.selected = Some(task.clone());
    assert!(state.set_selected_kind(Semantic::ServiceTask));
    assert!(state.undo());
    assert_eq!(semantic(&state, &task).kind, Semantic::UserTask);
    assert!(state.redo());
    assert_eq!(semantic(&state, &task).kind, Semantic::ServiceTask);
}

#[test]
fn bpmn_sequence_scope_and_endpoint_changes_use_core_validation() {
    let mut state = editor(CanvasProfileV1::Bpmn);
    let first = draw(&mut state, Semantic::Participant, [0., 0.], [400., 260.]);
    let second = draw(&mut state, Semantic::Participant, [500., 0.], [900., 260.]);
    state.selected = None;
    assert!(state.set_membership(Some(first.clone()), None));
    let start = draw(&mut state, Semantic::StartEvent, [40., 60.], [80., 100.]);
    let task = draw(&mut state, Semantic::Task, [180., 50.], [280., 120.]);
    let flow = draw(&mut state, Semantic::SequenceFlow, [80., 80.], [180., 80.]);
    state.selected = None;
    assert!(state.set_membership(Some(second), None));
    let other = draw(&mut state, Semantic::Task, [570., 50.], [670., 120.]);
    let before = state.scene.clone();
    state.selected = Some(flow.clone());
    assert!(!state.set_endpoint(false, &other));
    assert_eq!(state.scene, before);
    assert!(!state.set_endpoint(false, &start));
    assert_eq!(state.scene, before);
    state.selected = Some(task.clone());
    assert!(!state.set_membership(None, None));
    assert_eq!(state.scene, before);
    // Geometry does not infer semantic ownership from the containing pool.
    assert!(
        state.mutate(|scene, _| scene.elements.iter_mut().find(|e| e.id == task).unwrap().x = 600.)
    );
    assert_eq!(
        semantic(&state, &task).participant_id.as_deref(),
        Some(first.as_str())
    );
    assert!(state.undo());
    assert_eq!(state.scene, before);
}

#[test]
fn semantic_move_keeps_bound_endpoints_and_resizing_is_one_undo_unit() {
    let mut state = editor(CanvasProfileV1::Architecture);
    let node = draw(&mut state, Semantic::Component, [20., 20.], [120., 100.]);
    draw(&mut state, Semantic::Component, [240., 20.], [340., 100.]);
    let edge = draw(&mut state, Semantic::Relationship, [120., 60.], [240., 60.]);
    state.tool = Tool::Select;
    state.selected = Some(node.clone());
    let before = state.scene.clone();
    state.press([120., 100.], false);
    state.drag([160., 140.]);
    assert!(state.release().0, "{:?}", state.semantic_error);
    assert_eq!(
        semantic(&state, &edge).source_id.as_deref(),
        Some(node.as_str())
    );
    assert!(state.undo());
    assert_eq!(state.scene, before);
}

#[test]
fn semantic_endpoint_drag_cannot_leave_an_unbound_or_cross_process_connection() {
    let mut state = editor(CanvasProfileV1::Architecture);
    draw(&mut state, Semantic::Component, [20., 20.], [120., 100.]);
    draw(&mut state, Semantic::Component, [240., 20.], [340., 100.]);
    let edge = draw(&mut state, Semantic::Relationship, [120., 60.], [240., 60.]);
    state.tool = Tool::Select;
    state.selected = Some(edge);
    let before = state.scene.clone();
    state.press([240., 60.], false);
    state.drag([600., 400.]);
    assert!(!state.release().0);
    assert_eq!(state.scene, before);
    assert!(state.semantic_error.is_some());
    assert!(!state.mutate(|scene, _| scene.elements[0].x = f32::NAN));
    assert_eq!(state.scene, before);
}

#[test]
fn semantic_eraser_removes_nodes_and_dependent_edges_in_one_undo() {
    let mut state = editor(CanvasProfileV1::Architecture);
    draw(&mut state, Semantic::Component, [20., 20.], [120., 100.]);
    let remaining = draw(&mut state, Semantic::Component, [240., 20.], [340., 100.]);
    draw(&mut state, Semantic::Relationship, [120., 60.], [240., 60.]);
    let before = state.scene.clone();
    state.tool = Tool::Eraser;
    state.press([50., 60.], false);
    state.drag([180., 60.]);
    assert!(state.release().0, "{:?}", state.semantic_error);
    assert_eq!(state.scene.elements.len(), 1);
    assert_eq!(state.scene.elements[0].id, remaining);
    assert!(state.undo());
    assert_eq!(state.scene, before);
    assert!(state.redo());
    assert_eq!(state.scene.elements.len(), 1);

    let mut process = editor(CanvasProfileV1::Bpmn);
    let pool = draw(&mut process, Semantic::Participant, [0., 0.], [440., 300.]);
    process.set_creation_kind(Semantic::Lane);
    assert!(process.set_membership(Some(pool.clone()), None));
    let lane = draw(&mut process, Semantic::Lane, [10., 20.], [430., 290.]);
    process.set_creation_kind(Semantic::Task);
    assert!(process.set_membership(Some(pool), Some(lane)));
    draw(&mut process, Semantic::Task, [40., 60.], [140., 130.]);
    draw(&mut process, Semantic::Task, [240., 60.], [340., 130.]);
    draw(
        &mut process,
        Semantic::SequenceFlow,
        [140., 95.],
        [240., 95.],
    );
    let before = process.scene.clone();
    assert!(
        process.mutate(|scene, _| scene.elements.clear()),
        "{:?}",
        process.semantic_error
    );
    assert!(process.scene.document.as_ref().unwrap().elements.is_empty());
    assert!(process.undo());
    assert_eq!(process.scene, before);
}
