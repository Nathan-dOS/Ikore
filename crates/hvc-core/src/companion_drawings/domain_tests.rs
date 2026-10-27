use super::*;
use geometry::*;

fn rectangle(id: &str, x: f32, y: f32, width: f32, height: f32) -> CanvasElementV1 {
    let mut element = CanvasElementV1::new(
        CanvasElementKindV1::Rectangle { round: false },
        x,
        y,
        width,
        height,
    );
    element.id = id.into();
    element
}

fn scene(elements: Vec<CanvasElementV1>) -> CanvasSceneV1 {
    CanvasSceneV1 {
        elements,
        ..Default::default()
    }
}

fn arrow(id: &str, start: [f32; 2], end: [f32; 2]) -> CanvasElementV1 {
    let mut arrow = CanvasElementV1::new(
        CanvasElementKindV1::Arrow {
            points: [[0.0, 0.0], [1.0, 1.0]],
            bidirectional: false,
            start_binding: None,
            end_binding: None,
        },
        0.0,
        0.0,
        1.0,
        1.0,
    );
    arrow.id = id.into();
    set_connector_points(&mut arrow, [start, end]);
    arrow
}

#[test]
fn companion_drawings_scene_round_trip_is_stable_and_camera_free() {
    let mut b = rectangle("b", -30.0, 15.0, 100.0, 80.0);
    b.z_index = 1;
    let a = rectangle("a", 120.0, 15.0, 100.0, 80.0);
    let original = scene(vec![b, a]);
    let json = scene_json(&original).expect("serialize");
    let decoded: CanvasSceneV1 = serde_json::from_str(&json).expect("decode");
    assert_eq!(json, scene_json(&decoded).expect("serialize again"));
    assert_eq!(decoded.elements[0].id, "a");
    assert!(!json.contains("pan_x"));
    assert_eq!(
        CanvasSceneV1::default().background_mode,
        CanvasBackgroundMode::Dark
    );
}

#[test]
fn companion_drawings_normalizes_reverse_drag_and_vertical_connectors() {
    let reverse = rectangle("reverse", 100.0, 200.0, -40.0, -80.0);
    let vertical = arrow("vertical", [20.0, 100.0], [20.0, -20.0]);
    let normalized = normalize_scene(&scene(vec![reverse, vertical])).expect("normalize");
    let rectangle = normalized
        .elements
        .iter()
        .find(|e| e.id == "reverse")
        .expect("rectangle");
    assert_eq!(
        (rectangle.x, rectangle.y, rectangle.width, rectangle.height),
        (60.0, 120.0, 40.0, 80.0)
    );
    let connector = normalized
        .elements
        .iter()
        .find(|e| e.id == "vertical")
        .expect("connector");
    assert_eq!(connector.width, 0.0);
    assert_eq!(
        connector_world_points(connector),
        Some([[20.0, 100.0], [20.0, -20.0]])
    );
}

#[test]
fn companion_drawings_rejects_unknown_schema_nonfinite_and_duplicate_ids() {
    let mut candidate = scene(vec![rectangle("a", 0.0, 0.0, 100.0, 80.0)]);
    candidate.schema_version = 2;
    assert!(normalize_scene(&candidate).is_err());
    candidate.schema_version = CANVAS_SCHEMA_VERSION;
    candidate.elements[0].x = f32::NAN;
    assert!(normalize_scene(&candidate).is_err());
    candidate.elements[0].x = 0.0;
    candidate.elements.push(candidate.elements[0].clone());
    assert!(normalize_scene(&candidate).is_err());
}

#[test]
fn companion_drawings_rejects_dangling_and_cyclic_topology() {
    let mut a = rectangle("a", 0.0, 0.0, 100.0, 80.0);
    a.container_id = Some("missing".into());
    assert!(normalize_scene(&scene(vec![a.clone()])).is_err());
    a.stroke_style = CanvasStrokeStyle::Dashed;
    a.container_id = Some("b".into());
    let mut b = rectangle("b", 10.0, 10.0, 50.0, 40.0);
    b.stroke_style = CanvasStrokeStyle::Dashed;
    b.container_id = Some("a".into());
    assert!(normalize_scene(&scene(vec![a, b])).is_err());
    let mut edge = arrow("edge", [0.0, 0.0], [100.0, 0.0]);
    if let CanvasElementKindV1::Arrow { end_binding, .. } = &mut edge.shape {
        *end_binding = Some(ArrowBindingV1 {
            element_id: "missing".into(),
            anchor: [-1.0, 0.0],
            gap: 0.0,
        });
    }
    assert!(normalize_scene(&scene(vec![edge])).is_err());
}

#[test]
fn companion_drawings_redacts_before_json_and_svg_and_preserves_unicode() {
    let secret = "ghp_0123456789abcdefghijklmnop";
    let mut node = rectangle("node", 0.0, 0.0, 200.0, 100.0);
    node.label = format!("Decisão → serviço\r\nTOKEN={secret}");
    let candidate = scene(vec![node]);
    let normalized = normalize_scene(&candidate).expect("normalize");
    assert!(normalized.elements[0].label.contains("Decisão → serviço\n"));
    assert!(normalized.elements[0].label.contains("<REDACTED_SECRET:"));
    assert!(!scene_json(&candidate).expect("json").contains(secret));
    assert!(!export_svg(&candidate).expect("svg").contains(secret));
    assert!(
        !sanitize_title(&format!("Token {secret}"))
            .expect("title")
            .contains(secret)
    );
}

#[test]
fn companion_drawings_enforces_text_and_scene_limits_without_truncating() {
    let mut node = rectangle("node", 0.0, 0.0, 100.0, 80.0);
    node.label = "a".repeat(MAX_LABEL_CHARS + 1);
    assert!(normalize_scene(&scene(vec![node.clone()])).is_err());
    node.label = "bad\0label".into();
    assert!(normalize_scene(&scene(vec![node])).is_err());
    assert!(sanitize_title("").is_err());
    assert!(sanitize_title(&"a".repeat(MAX_TITLE_CHARS + 1)).is_err());
    assert!(sanitize_title("two\nlines").is_err());
    assert!(
        normalize_scene(&scene(
            (0..=MAX_ELEMENTS)
                .map(|n| rectangle(&n.to_string(), 0.0, 0.0, 10.0, 10.0))
                .collect()
        ))
        .is_err()
    );
}

#[test]
fn companion_drawings_hit_test_honors_shapes_and_paint_order() {
    let a = rectangle("a", 0.0, 0.0, 100.0, 100.0);
    let mut b = rectangle("b", 0.0, 0.0, 100.0, 100.0);
    b.shape = CanvasElementKindV1::Diamond;
    b.z_index = 2;
    let candidate = scene(vec![a, b]);
    assert_eq!(
        hit_test(&candidate, [50.0, 50.0], 0.0).as_deref(),
        Some("b")
    );
    assert_eq!(hit_test(&candidate, [2.0, 2.0], 0.0).as_deref(), Some("a"));
    assert!(hit_test(&candidate, [101.0, 101.0], 0.0).is_none());
    assert_eq!(
        distance_to_segment([25.0, 4.0], [0.0, 0.0], [50.0, 0.0]),
        4.0
    );
}

#[test]
fn companion_drawings_zoom_keeps_world_point_under_cursor() {
    let mut viewport = CanvasViewportV1 {
        pan_x: 42.0,
        pan_y: -18.0,
        zoom: 0.5,
    };
    let cursor = [213.0, 87.0];
    let world = screen_to_world(&viewport, cursor);
    zoom_at(&mut viewport, cursor, 4.0);
    let projected = world_to_screen(&viewport, world);
    assert!((projected[0] - cursor[0]).abs() < 0.001);
    assert!((projected[1] - cursor[1]).abs() < 0.001);
    zoom_at(&mut viewport, cursor, 100.0);
    assert_eq!(viewport.zoom, MAX_ZOOM);
    zoom_at(&mut viewport, cursor, 0.001);
    assert_eq!(viewport.zoom, MIN_ZOOM);
    assert!(validate_viewport(&viewport).is_ok());
    viewport.zoom = f32::NAN;
    assert!(validate_viewport(&viewport).is_err());
}

#[test]
fn companion_drawings_bindings_follow_move_and_resize_of_every_shape() {
    for shape in [
        CanvasElementKindV1::Rectangle { round: true },
        CanvasElementKindV1::Diamond,
        CanvasElementKindV1::Ellipse,
    ] {
        let mut target = rectangle("target", 100.0, 100.0, 100.0, 80.0);
        target.shape = shape;
        let mut edge = arrow("edge", [0.0, 140.0], [100.0, 140.0]);
        if let CanvasElementKindV1::Arrow { end_binding, .. } = &mut edge.shape {
            *end_binding = Some(ArrowBindingV1 {
                element_id: "target".into(),
                anchor: [-1.0, 0.0],
                gap: 0.0,
            });
        }
        let mut candidate = scene(vec![target, edge]);
        candidate.elements[0].x = 300.0;
        candidate.elements[0].width = 200.0;
        update_bindings(&mut candidate);
        assert_eq!(
            connector_world_points(&candidate.elements[1]).expect("points")[1],
            [300.0, 140.0]
        );
        assert_eq!(candidate.elements[1].height, 0.0);
        normalize_scene(&candidate).expect("valid resolved scene");
    }
}

#[test]
fn companion_drawings_snap_uses_border_distance_and_resolves_actual_shape() {
    let mut target = rectangle("target", 100.0, 100.0, 100.0, 100.0);
    target.shape = CanvasElementKindV1::Ellipse;
    let candidate = scene(vec![target]);
    let binding = snap_binding(&candidate, [96.0, 150.0], 14.0, None).expect("snap");
    assert_eq!(
        binding_point(&candidate.elements[0], &binding),
        [100.0, 150.0]
    );
    assert!(snap_binding(&candidate, [80.0, 150.0], 14.0, None).is_none());
    assert!(snap_binding(&candidate, [100.0, 150.0], 14.0, Some("target")).is_none());
    assert!(snap_binding(&candidate, [150.0, 150.0], 14.0, None).is_none());
}

#[test]
fn companion_drawings_containment_chooses_smallest_dashed_boundary() {
    let mut outer = rectangle("outer", 0.0, 0.0, 500.0, 500.0);
    outer.stroke_style = CanvasStrokeStyle::Dashed;
    let mut inner = rectangle("inner", 20.0, 20.0, 250.0, 250.0);
    inner.stroke_style = CanvasStrokeStyle::Dashed;
    let ordinary = rectangle("ordinary", 25.0, 25.0, 150.0, 150.0);
    let node = rectangle("node", 30.0, 30.0, 50.0, 50.0);
    let edge = arrow("edge", [40.0, 40.0], [200.0, 200.0]);
    let mut candidate = scene(vec![outer, inner, ordinary, node, edge]);
    update_containers(&mut candidate);
    assert_eq!(candidate.elements[1].container_id.as_deref(), Some("outer"));
    assert_eq!(candidate.elements[3].container_id.as_deref(), Some("inner"));
    assert!(candidate.elements[4].container_id.is_none());
    candidate.elements[1].stroke_style = CanvasStrokeStyle::Solid;
    update_containers(&mut candidate);
    assert_eq!(candidate.elements[3].container_id.as_deref(), Some("outer"));
}

#[test]
fn companion_drawings_delete_detaches_references_and_preserves_endpoints() {
    let mut boundary = rectangle("boundary", 0.0, 0.0, 300.0, 300.0);
    boundary.stroke_style = CanvasStrokeStyle::Dashed;
    let mut target = rectangle("target", 100.0, 100.0, 100.0, 80.0);
    target.container_id = Some("boundary".into());
    let mut edge = arrow("edge", [0.0, 140.0], [100.0, 140.0]);
    if let CanvasElementKindV1::Arrow { end_binding, .. } = &mut edge.shape {
        *end_binding = Some(ArrowBindingV1 {
            element_id: "target".into(),
            anchor: [-1.0, 0.0],
            gap: 0.0,
        });
    }
    let mut candidate = scene(vec![boundary, target, edge]);
    let original = connector_world_points(&candidate.elements[2]);
    detach_element(&mut candidate, "target");
    candidate.elements.retain(|element| element.id != "target");
    update_bindings(&mut candidate);
    assert_eq!(connector_world_points(&candidate.elements[1]), original);
    if let CanvasElementKindV1::Arrow { end_binding, .. } = &candidate.elements[1].shape {
        assert!(end_binding.is_none());
    }
}

#[test]
fn companion_drawings_svg_is_deterministic_escaped_and_complete() {
    let mut node = rectangle("node", -100.0, -50.0, 100.0, 80.0);
    node.label = "<&\"'>\nDecisão → Core".into();
    node.fill_enabled = true;
    node.stroke_style = CanvasStrokeStyle::Dashed;
    let candidate = scene(vec![node]);
    let svg = export_svg(&candidate).expect("svg");
    assert_eq!(svg, export_svg(&candidate).expect("again"));
    assert!(svg.contains("&lt;&amp;&quot;&apos;&gt;"));
    assert!(svg.contains("Decisão → Core"));
    assert!(svg.contains("stroke-dasharray=\"8 6\""));
    assert!(svg.contains("fill=\"#06080b\""));
    assert!(!svg.contains("foreignObject"));
    assert!(!svg.contains("pan_x"));
    assert_eq!(svg.matches("<tspan ").count(), 2);
    let mut light = candidate;
    light.background_mode = CanvasBackgroundMode::Light;
    assert!(
        export_svg(&light)
            .expect("light")
            .contains("fill=\"#f8f9fa\"")
    );
}

#[test]
fn companion_drawings_empty_svg_and_long_label_bounds_are_valid() {
    let svg = export_svg(&CanvasSceneV1::default()).expect("empty SVG");
    assert!(svg.starts_with("<svg "));
    assert!(svg.ends_with("</svg>\n"));
    let mut node = rectangle("node", 0.0, 0.0, 20.0, 20.0);
    node.label = "WWWWWWWWWWWWWWWWWWWW".into();
    let bounds = scene_bounds(&scene(vec![node]));
    assert!(bounds.width >= 320.0);
    assert!(bounds.x < 0.0);
}

#[test]
fn companion_drawings_disallows_unknown_fields_and_bad_binding_anchors() {
    assert!(serde_json::from_str::<CanvasSceneV1>(r#"{"schema_version":1,"background_mode":"dark","show_grid":true,"elements":[],"surprise":1}"#).is_err());
    let target = rectangle("target", 100.0, 0.0, 80.0, 80.0);
    let mut edge = arrow("edge", [0.0, 40.0], [100.0, 40.0]);
    if let CanvasElementKindV1::Arrow { end_binding, .. } = &mut edge.shape {
        *end_binding = Some(ArrowBindingV1 {
            element_id: "target".into(),
            anchor: [f32::INFINITY, 0.0],
            gap: 0.0,
        });
    }
    assert!(normalize_scene(&scene(vec![target, edge])).is_err());
}

#[test]
fn companion_drawings_caps_escaped_svg_before_materializing_an_unreadable_mirror() {
    let elements = (0..850)
        .map(|index| {
            let mut node = rectangle(&format!("node-{index}"), 0.0, 0.0, 100.0, 80.0);
            node.label = "&".repeat(MAX_LABEL_CHARS);
            node
        })
        .collect();
    let scene = scene(elements);
    assert!(
        scene_json(&scene).is_ok(),
        "The native JSON is within its limit"
    );
    assert!(
        export_svg(&scene).is_err(),
        "Escaped SVG must fit the mirror reader's limit"
    );
}

#[test]
fn companion_drawings_text_bounds_cover_all_lines_after_sanitization() {
    let mut text = CanvasElementV1::new(
        CanvasElementKindV1::Text { font_size: 20.0 },
        100.0,
        100.0,
        180.0,
        24.0,
    );
    text.label = "First\nSecond\nThird".into();
    let normalized = normalize_scene(&scene(vec![text])).expect("valid text scene");
    let element = &normalized.elements[0];
    assert_eq!(element.height, 78.0);
    assert_eq!(element.width, 120.0);
    assert!(
        hit_element(element, [150.0, 170.0], 0.0),
        "the final line is selectable"
    );
}

#[test]
fn companion_drawings_boundary_created_after_nodes_paints_behind_and_selects_child() {
    let mut child = rectangle("child", 80.0, 80.0, 80.0, 60.0);
    child.label = "CHILD".into();
    child.z_index = 1;
    let mut boundary = rectangle("boundary", 0.0, 0.0, 300.0, 240.0);
    boundary.label = "BOUNDARY".into();
    boundary.stroke_style = CanvasStrokeStyle::Dashed;
    boundary.fill_enabled = true;
    boundary.z_index = 99;
    let mut candidate = scene(vec![child, boundary]);
    update_containers(&mut candidate);
    assert_eq!(
        candidate.elements[0].container_id.as_deref(),
        Some("boundary")
    );
    assert_eq!(
        paint_order(&candidate)
            .iter()
            .map(|element| element.id.as_str())
            .collect::<Vec<_>>(),
        ["boundary", "child"]
    );
    assert_eq!(
        hit_test(&candidate, [100.0, 100.0], 0.0).as_deref(),
        Some("child")
    );
    assert_eq!(
        hit_test(&candidate, [20.0, 20.0], 0.0).as_deref(),
        Some("boundary")
    );
    let normalized = normalize_scene(&candidate).expect("normalize");
    assert_eq!(normalized.elements[0].id, "boundary");
    assert_eq!(
        normalized.elements[0].z_index, 99,
        "explicit layers are preserved"
    );
    let svg = export_svg(&candidate).expect("SVG");
    assert!(
        svg.find(">BOUNDARY</tspan>").expect("boundary label")
            < svg.find(">CHILD</tspan>").expect("child label")
    );
    let original = scene_json(&candidate).expect("canonical JSON");
    candidate.elements.reverse();
    assert_eq!(
        original,
        scene_json(&candidate).expect("same canonical order")
    );
}

#[test]
fn companion_drawings_nested_paint_order_is_stable_and_preserves_sibling_layers() {
    let mut outer = rectangle("outer", 0.0, 0.0, 500.0, 400.0);
    outer.stroke_style = CanvasStrokeStyle::Dashed;
    outer.z_index = 500;
    let mut inner = rectangle("inner", 20.0, 20.0, 300.0, 300.0);
    inner.stroke_style = CanvasStrokeStyle::Dashed;
    inner.z_index = 200;
    let mut a = rectangle("a", 40.0, 40.0, 100.0, 100.0);
    a.z_index = 5;
    let mut b = rectangle("b", 40.0, 40.0, 100.0, 100.0);
    b.z_index = 5;
    let mut front = rectangle("front", 40.0, 40.0, 100.0, 100.0);
    front.z_index = 6;
    let mut candidate = scene(vec![front, b, outer, a, inner]);
    update_containers(&mut candidate);
    assert_eq!(
        paint_order(&candidate)
            .iter()
            .map(|element| element.id.as_str())
            .collect::<Vec<_>>(),
        ["outer", "inner", "a", "b", "front"]
    );
    assert_eq!(
        hit_test(&candidate, [80.0, 80.0], 0.0).as_deref(),
        Some("front")
    );
    let first = scene_json(&candidate).expect("valid hierarchy");
    candidate.elements.reverse();
    assert_eq!(scene_json(&candidate).expect("valid reversed input"), first);
    for element in &candidate.elements {
        let mut id = element.container_id.as_deref();
        let mut seen = std::collections::HashSet::from([element.id.as_str()]);
        while let Some(parent) = id {
            assert!(seen.insert(parent), "derived containment is acyclic");
            id = candidate
                .elements
                .iter()
                .find(|element| element.id == parent)
                .and_then(|element| element.container_id.as_deref());
        }
    }
}

#[test]
fn companion_drawings_connector_labels_clear_horizontal_strokes_and_fit_svg_bounds() {
    for line in [false, true] {
        let mut connector = arrow("labeled-edge", [120.0, 100.0], [340.0, 100.0]);
        if line {
            connector.shape = CanvasElementKindV1::Line {
                points: connector.shape.points().expect("connector points"),
            };
        }
        connector.label = "typed calls\nsave".into();
        let label = label_bounds(&connector);
        assert!(
            label.y + label.height < 100.0,
            "the stroke must not cross the label"
        );
        assert!((100.0 - label.y - label.height - 6.0).abs() < 0.001);
        let scene = scene(vec![connector]);
        assert!(scene_bounds(&scene).contains_bounds(label));
        let svg = export_svg(&scene).expect("SVG with a raised connector label");
        let values = svg
            .split("viewBox=\"")
            .nth(1)
            .expect("viewBox")
            .split('"')
            .next()
            .expect("viewBox value")
            .split_whitespace()
            .map(|value| value.parse::<f32>().expect("numeric bounds"))
            .collect::<Vec<_>>();
        let exported = Bounds {
            x: values[0],
            y: values[1],
            width: values[2],
            height: values[3],
        };
        assert!(
            exported.contains_bounds(label),
            "the SVG includes the complete raised label"
        );
        assert_eq!(svg, export_svg(&scene).expect("deterministic SVG"));
        let json = scene_json(&scene).expect("canonical JSON");
        let reopened: CanvasSceneV1 = serde_json::from_str(&json).expect("reopen");
        assert_eq!(json, scene_json(&reopened).expect("unchanged scene"));
    }
}
