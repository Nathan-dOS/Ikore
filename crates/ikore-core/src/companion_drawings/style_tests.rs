use super::*;

fn connector(curve: CanvasConnectorCurve) -> CanvasElementV1 {
    let mut element = CanvasElementV1::new(
        CanvasElementKindV1::Arrow {
            points: [[0., 0.], [240., 0.]],
            bidirectional: true,
            start_binding: None,
            end_binding: None,
        },
        20.,
        100.,
        240.,
        0.,
    );
    element.id = "stable-arrow".into();
    element.connector_curve = curve;
    element
}

#[test]
fn companion_drawings_legacy_visual_defaults_keep_canonical_json_unchanged() {
    let element = connector(CanvasConnectorCurve::Straight);
    let value = serde_json::to_value(&element).expect("legacy-compatible element");
    assert!(value.get("stroke_rendering").is_none());
    assert!(value.get("connector_curve").is_none());
    let decoded: CanvasElementV1 = serde_json::from_value(value).expect("old element");
    assert_eq!(decoded.stroke_rendering, CanvasStrokeRendering::Clean);
    assert_eq!(decoded.connector_curve, CanvasConnectorCurve::Straight);
    assert_eq!(decoded, element);
}

#[test]
fn companion_drawings_curves_share_hit_testing_label_position_and_export_bounds() {
    for curve in [
        CanvasConnectorCurve::CurveLeft,
        CanvasConnectorCurve::CurveRight,
    ] {
        let mut element = connector(curve);
        element.label = "curve".into();
        let middle = visual_geometry::connector_at(&element, 0.5).expect("midpoint");
        assert!((middle[1] - 100.).abs() > 20.);
        assert!(geometry::hit_element(&element, middle, 1.));
        assert!(!geometry::hit_element(&element, [140., 100.], 3.));
        let label = geometry::label_bounds(&element);
        assert!((label.centre()[0] - middle[0]).abs() < 0.001);
        assert!((label.y + label.height + 6. - middle[1]).abs() < 0.001);
        assert!(geometry::ink_bounds(&element).contains(middle));
        let scene = CanvasSceneV1 {
            elements: vec![element],
            ..Default::default()
        };
        let svg = export_svg(&scene).expect("curved SVG");
        assert!(svg.contains(" Q "), "SVG uses the same quadratic curve");
        assert_eq!(svg, export_svg(&scene).expect("deterministic curve"));
    }
}

#[test]
fn companion_drawings_sketch_paths_are_stable_bounded_and_exported_for_every_shape() {
    for shape in [
        CanvasElementKindV1::Rectangle { round: false },
        CanvasElementKindV1::Rectangle { round: true },
        CanvasElementKindV1::Diamond,
        CanvasElementKindV1::Ellipse,
        CanvasElementKindV1::Line {
            points: [[0., 0.], [120., 90.]],
        },
    ] {
        let mut element = CanvasElementV1::new(shape, 20., 30., 120., 90.);
        element.id = "stable-shape".into();
        element.stroke_rendering = CanvasStrokeRendering::Sketch;
        let first = visual_geometry::element_path(&element);
        assert_eq!(first, visual_geometry::element_path(&element));
        assert!(first.commands.len() <= 16);
        assert!(first.control_bounds().expect("bounds").width < 150.);
        let scene = CanvasSceneV1 {
            elements: vec![element],
            ..Default::default()
        };
        let json = scene_json(&scene).expect("sketch scene");
        assert!(json.contains("\"stroke_rendering\": \"sketch\""));
        let reopened: CanvasSceneV1 = serde_json::from_str(&json).expect("reopen sketch");
        assert_eq!(json, scene_json(&reopened).expect("stable reopen"));
        assert!(export_svg(&scene).expect("sketch SVG").contains("<path d="));
    }
}

#[test]
fn companion_drawings_curved_bindings_follow_targets_and_arrowheads_use_tangents() {
    let mut target = CanvasElementV1::new(CanvasElementKindV1::Ellipse, 260., 60., 80., 80.);
    target.id = "target".into();
    let mut element = connector(CanvasConnectorCurve::CurveLeft);
    element.stroke_rendering = CanvasStrokeRendering::Sketch;
    if let CanvasElementKindV1::Arrow { end_binding, .. } = &mut element.shape {
        *end_binding = Some(ArrowBindingV1 {
            element_id: target.id.clone(),
            anchor: [-1., 0.],
            gap: 0.,
        });
    }
    target.x += 100.;
    let scene = normalize_scene(&CanvasSceneV1 {
        elements: vec![target, element],
        ..Default::default()
    })
    .expect("resolved curve");
    let element = scene
        .elements
        .iter()
        .find(|e| e.id == "stable-arrow")
        .expect("arrow");
    let points = geometry::connector_world_points(element).expect("endpoints");
    assert_eq!(points[1], [360., 100.]);
    assert_ne!(
        visual_geometry::connector_tangent_from(element, false).expect("tangent"),
        points[0]
    );
    assert!(!visual_geometry::arrowhead_path(element, false).closed);
    assert!(!visual_geometry::arrowhead_path(element, true).closed);
    let svg = export_svg(&scene).expect("open sketch heads");
    assert!(svg.matches("fill=\"none\"").count() >= 3);
}

#[test]
fn companion_drawings_rejects_unknown_visual_variants() {
    let mut value =
        serde_json::to_value(connector(CanvasConnectorCurve::Straight)).expect("element");
    value["connector_curve"] = "unbounded_route".into();
    assert!(serde_json::from_value::<CanvasElementV1>(value).is_err());
}

#[test]
fn companion_drawings_curves_cover_vertical_diagonal_reversed_and_tiny_endpoints() {
    for points in [
        [[40., 0.], [40., 240.]],
        [[40., 240.], [40., 0.]],
        [[240., 200.], [0., 0.]],
        [[0., 0.], [0.01, 0.01]],
    ] {
        for curve in [
            CanvasConnectorCurve::CurveLeft,
            CanvasConnectorCurve::CurveRight,
        ] {
            let mut element = connector(curve);
            geometry::set_connector_points(&mut element, points);
            element.stroke_rendering = CanvasStrokeRendering::Sketch;
            element.label = "edge\nsecond line".into();
            let middle = visual_geometry::connector_at(&element, 0.5).expect("middle");
            assert!(middle.into_iter().all(f32::is_finite));
            assert!(geometry::hit_element(&element, middle, 0.5));
            let label = geometry::label_bounds(&element);
            assert!((label.centre()[0] - middle[0]).abs() < 0.001);
            let scene = CanvasSceneV1 {
                elements: vec![element.clone()],
                ..Default::default()
            };
            let bounds = geometry::scene_bounds(&scene);
            assert!(bounds.contains_bounds(label));
            for at_start in [false, true] {
                let head_bounds = visual_geometry::arrowhead_path(&element, at_start)
                    .control_bounds()
                    .expect("head bounds");
                assert!(bounds.contains_bounds(head_bounds));
            }
            let svg = export_svg(&scene).expect("orientation SVG");
            let label_x: f32 = svg
                .split("<tspan x=\"")
                .nth(1)
                .expect("label x")
                .split('"')
                .next()
                .expect("number")
                .parse()
                .expect("numeric label x");
            assert!(
                (label_x - label.centre()[0]).abs() <= 0.001,
                "SVG/native labels use the actual curve midpoint x"
            );
        }
    }
}
