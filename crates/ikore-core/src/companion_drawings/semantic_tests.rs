use super::semantics::*;
use super::{ArrowBindingV1, CanvasElementKindV1 as Shape, CanvasElementV1, CanvasSceneV1};

fn entry(id: &str, kind: CanvasSemanticKindV1) -> CanvasSemanticElementV1 {
    CanvasSemanticElementV1 {
        element_id: id.into(),
        kind,
        participant_id: None,
        lane_id: None,
        source_id: None,
        target_id: None,
    }
}

fn node(id: &str, kind: CanvasSemanticKindV1) -> (CanvasElementV1, CanvasSemanticElementV1) {
    use CanvasSemanticKindV1::*;
    let shape = match kind {
        StartEvent | EndEvent => Shape::Ellipse,
        ExclusiveGateway | ParallelGateway => Shape::Diamond,
        Annotation => Shape::Text { font_size: 16.0 },
        _ => Shape::Rectangle { round: false },
    };
    let mut element = CanvasElementV1::new(shape, 20.0, 30.0, 100.0, 60.0);
    element.id = id.into();
    element.label = id.into();
    (element, entry(id, kind))
}

fn owned_node(
    id: &str,
    kind: CanvasSemanticKindV1,
    participant: &str,
    lane: Option<&str>,
) -> (CanvasElementV1, CanvasSemanticElementV1) {
    let (element, mut semantic) = node(id, kind);
    semantic.participant_id = Some(participant.into());
    semantic.lane_id = lane.map(str::to_owned);
    (element, semantic)
}

fn edge(
    id: &str,
    kind: CanvasSemanticKindV1,
    source: &str,
    target: &str,
) -> (CanvasElementV1, CanvasSemanticElementV1) {
    let points = [[0.0, 0.0], [100.0, 0.0]];
    let shape = if kind == CanvasSemanticKindV1::Association {
        Shape::Line { points }
    } else {
        Shape::Arrow {
            points,
            bidirectional: false,
            start_binding: Some(ArrowBindingV1 {
                element_id: source.into(),
                anchor: [1.0, 0.0],
                gap: 0.0,
            }),
            end_binding: Some(ArrowBindingV1 {
                element_id: target.into(),
                anchor: [-1.0, 0.0],
                gap: 0.0,
            }),
        }
    };
    let mut element = CanvasElementV1::new(shape, 10.0, 20.0, 100.0, 1.0);
    element.id = id.into();
    element.label = id.into();
    let mut semantic = entry(id, kind);
    semantic.source_id = Some(source.into());
    semantic.target_id = Some(target.into());
    (element, semantic)
}

fn scene(
    profile: CanvasProfileV1,
    pairs: Vec<(CanvasElementV1, CanvasSemanticElementV1)>,
) -> CanvasSceneV1 {
    let (elements, semantics) = pairs.into_iter().unzip();
    CanvasSceneV1 {
        elements,
        document: Some(CanvasDocumentV1 {
            schema_version: 1,
            profile,
            language: "pt-BR".into(),
            narrative: "Architecture explanation".into(),
            provenance: None,
            elements: semantics,
        }),
        ..Default::default()
    }
}

fn process() -> CanvasSceneV1 {
    use CanvasSemanticKindV1::*;
    scene(
        CanvasProfileV1::Bpmn,
        vec![
            node("pool", Participant),
            owned_node("lane_a", Lane, "pool", None),
            owned_node("lane_b", Lane, "pool", None),
            owned_node("start", StartEvent, "pool", Some("lane_a")),
            owned_node("task", UserTask, "pool", Some("lane_b")),
            owned_node("end", EndEvent, "pool", None),
            edge("flow_a", SequenceFlow, "start", "task"),
            edge("flow_b", SequenceFlow, "task", "end"),
        ],
    )
}

#[test]
fn semantic_svg_preserves_profile_narrative_and_bpmn_markers_without_inference() {
    use CanvasSemanticKindV1::*;
    let mut drawing = scene(
        CanvasProfileV1::Bpmn,
        vec![
            node("pool", Participant),
            owned_node("lane", Lane, "pool", None),
            owned_node("user", UserTask, "pool", Some("lane")),
            owned_node("service", ServiceTask, "pool", None),
            owned_node("start", StartEvent, "pool", None),
            owned_node("end", EndEvent, "pool", None),
            owned_node("xor", ExclusiveGateway, "pool", None),
            owned_node("parallel", ParallelGateway, "pool", None),
        ],
    );
    drawing.document.as_mut().unwrap().narrative = "A < B & C".into();
    let svg = super::export_svg(&drawing).unwrap();
    assert!(svg.contains("<desc>A &lt; B &amp; C</desc>"));
    for marker in [
        "participant",
        "lane",
        "user_task",
        "service_task",
        "start_event",
        "end_event",
        "exclusive_gateway",
        "parallel_gateway",
    ] {
        assert!(
            svg.contains(&format!("data-hvc-kind=\"{marker}\"")),
            "{marker}"
        );
    }
    assert!(svg.contains(">X</text>"));
    assert!(svg.contains(">+</text>"));
    assert!(svg.contains("data-hvc-end-ring"));
    let mut legacy = drawing;
    legacy.document = None;
    let svg = super::export_svg(&legacy).unwrap();
    assert!(!svg.contains("data-hvc-kind"));
    assert!(!svg.contains("data-hvc-end-ring"));
}

#[test]
fn semantic_svg_connection_style_follows_explicit_type_and_does_not_change_source() {
    use CanvasSemanticKindV1::*;
    let drawing = scene(
        CanvasProfileV1::Bpmn,
        vec![
            node("a", Participant),
            node("b", Participant),
            edge("message", MessageFlow, "a", "b"),
        ],
    );
    let original = drawing.clone();
    let svg = super::export_svg(&drawing).unwrap();
    let connection = svg
        .split("data-hvc-kind=\"message_flow\"")
        .nth(1)
        .unwrap()
        .split("</g>")
        .next()
        .unwrap();
    assert!(connection.contains("stroke-dasharray=\"8 6\""));
    assert_eq!(drawing, original);
    let association = scene(
        CanvasProfileV1::Bpmn,
        vec![
            node("pool", Participant),
            node("note", Annotation),
            edge("explanation", Association, "note", "pool"),
        ],
    );
    let svg = super::export_svg(&association).unwrap();
    let connection = svg
        .split("data-hvc-kind=\"association\"")
        .nth(1)
        .unwrap()
        .split("</g>")
        .next()
        .unwrap();
    assert!(
        connection.contains("stroke-dasharray=\"1 5\""),
        "Associations match the native dotted distinction"
    );
}

fn add(scene: &mut CanvasSceneV1, pair: (CanvasElementV1, CanvasSemanticElementV1)) {
    scene.elements.push(pair.0);
    scene.document.as_mut().unwrap().elements.push(pair.1);
}

#[test]
fn legacy_scenes_do_not_gain_semantics_from_shapes_or_containment() {
    let mut legacy = CanvasSceneV1::default();
    legacy
        .elements
        .push(node("start", CanvasSemanticKindV1::StartEvent).0);
    assert!(validate_document(&legacy).unwrap().is_empty());
    assert!(normalize_document(&mut legacy).unwrap().is_empty());
    assert!(legacy.document.is_none());
    let serialized = serde_json::to_string(&legacy).unwrap();
    assert!(!serialized.contains("document"));
    let loaded: CanvasSceneV1 = serde_json::from_str(&serialized).unwrap();
    assert_eq!(loaded, legacy);
}

#[test]
fn architecture_shapes_are_free_of_bpmn_constraints() {
    use CanvasSemanticKindV1::*;
    for shape in [
        Shape::Rectangle { round: false },
        Shape::Diamond,
        Shape::Ellipse,
    ] {
        let mut left = node("ui", Component);
        left.0.shape = shape;
        let architecture = scene(
            CanvasProfileV1::Architecture,
            vec![
                left,
                node("core", Component),
                edge("calls", Relationship, "ui", "core"),
            ],
        );
        assert!(validate_document(&architecture).unwrap().is_empty());
    }
}

#[test]
fn architecture_line_relationship_keeps_explicit_endpoints() {
    use CanvasSemanticKindV1::*;
    let mut relation = edge("depends", Relationship, "ui", "core");
    relation.0.shape = Shape::Line {
        points: [[0.0, 0.0], [100.0, 0.0]],
    };
    let drawing = scene(
        CanvasProfileV1::Architecture,
        vec![node("ui", Component), node("core", Component), relation],
    );
    assert!(validate_document(&drawing).is_ok());
}

#[test]
fn process_sequence_flows_cross_lanes_with_explicit_same_pool_membership() {
    assert!(validate_document(&process()).unwrap().is_empty());
}

#[test]
fn semantic_ownership_does_not_follow_geometric_containment_or_position() {
    let mut drawing = process();
    let old_document = drawing.document.clone();
    drawing.elements[4].x = -10_000.0;
    drawing.elements[4].container_id = None;
    normalize_document(&mut drawing).unwrap();
    let mut before = old_document.unwrap();
    before
        .elements
        .sort_by(|a, b| a.element_id.cmp(&b.element_id));
    assert_eq!(drawing.document.as_ref().unwrap(), &before);
}

#[test]
fn sequence_flows_reject_cross_pool_and_ineligible_endpoints() {
    use CanvasSemanticKindV1::*;
    for (source, target) in [
        ("task", "remote"),
        ("pool", "task"),
        ("lane_a", "task"),
        ("note", "task"),
    ] {
        let mut drawing = process();
        add(&mut drawing, node("other_pool", Participant));
        add(&mut drawing, owned_node("remote", Task, "other_pool", None));
        add(&mut drawing, node("note", Annotation));
        add(&mut drawing, edge("bad_flow", SequenceFlow, source, target));
        let error = validate_document(&drawing).unwrap_err().to_string();
        assert!(error.contains("bad_flow"));
        assert!(error.contains("sequence"));
    }
}

#[test]
fn message_flows_require_separate_pools_and_supported_endpoint_types() {
    use CanvasSemanticKindV1::*;
    for (source, target, valid) in [
        ("task", "remote", true),
        ("pool", "remote", true),
        ("pool", "other_pool", true),
        ("task", "pool", false),
        ("gateway", "remote", false),
        ("start", "remote", false),
        ("end", "remote", false),
        ("lane_a", "remote", false),
        ("note", "remote", false),
    ] {
        let mut drawing = process();
        add(&mut drawing, node("other_pool", Participant));
        add(
            &mut drawing,
            owned_node("remote", ServiceTask, "other_pool", None),
        );
        add(
            &mut drawing,
            owned_node("gateway", ExclusiveGateway, "pool", None),
        );
        add(&mut drawing, node("note", Annotation));
        add(&mut drawing, edge("message", MessageFlow, source, target));
        assert_eq!(
            validate_document(&drawing).is_ok(),
            valid,
            "{source} -> {target}"
        );
    }
}

#[test]
fn start_and_end_direction_rules_are_hard_errors() {
    use CanvasSemanticKindV1::*;
    for (source, target) in [("task", "start"), ("end", "task")] {
        let mut drawing = process();
        add(&mut drawing, edge("bad_flow", SequenceFlow, source, target));
        assert!(validate_document(&drawing).is_err());
    }
}

#[test]
fn multiple_start_and_end_events_are_supported() {
    use CanvasSemanticKindV1::*;
    let mut drawing = process();
    add(
        &mut drawing,
        owned_node("start_two", StartEvent, "pool", None),
    );
    add(&mut drawing, owned_node("end_two", EndEvent, "pool", None));
    add(
        &mut drawing,
        edge("flow_c", SequenceFlow, "start_two", "end_two"),
    );
    assert!(validate_document(&drawing).unwrap().is_empty());
}

#[test]
fn incomplete_process_is_a_valid_draft_with_diagnostics() {
    use CanvasSemanticKindV1::*;
    let draft = scene(
        CanvasProfileV1::Bpmn,
        vec![
            node("pool", Participant),
            owned_node("work", Task, "pool", None),
        ],
    );
    let warnings = validate_document(&draft).unwrap();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code == "bpmn_missing_start")
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code == "bpmn_missing_end")
    );
    assert!(warnings.iter().all(|warning| !warning.message.is_empty()));
}

#[test]
fn empty_semantic_document_is_a_valid_draft() {
    let warnings = validate_document(&scene(CanvasProfileV1::Bpmn, vec![])).unwrap();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code == "empty_document")
    );
}

#[test]
fn every_visual_element_has_one_semantic_entry_and_references_are_closed() {
    for mutation in 0..5 {
        let mut drawing = process();
        match mutation {
            0 => {
                drawing.document.as_mut().unwrap().elements.pop();
            }
            1 => {
                let repeated = drawing.document.as_ref().unwrap().elements[0].clone();
                drawing.document.as_mut().unwrap().elements.push(repeated);
            }
            2 => {
                drawing.document.as_mut().unwrap().elements[0].element_id = "missing".into();
            }
            3 => {
                drawing.elements[0].id = drawing.elements[1].id.clone();
            }
            _ => {
                drawing.document.as_mut().unwrap().elements[6].source_id = Some("missing".into());
            }
        }
        assert!(validate_document(&drawing).is_err(), "mutation {mutation}");
    }
}

#[test]
fn binding_and_semantic_endpoints_cannot_disagree() {
    let mut drawing = process();
    if let Shape::Arrow { start_binding, .. } = &mut drawing.elements[6].shape {
        start_binding.as_mut().unwrap().element_id = "end".into();
    }
    assert!(validate_document(&drawing).is_err());
    if let Shape::Arrow { start_binding, .. } = &mut drawing.elements[6].shape {
        *start_binding = None;
    }
    assert!(validate_document(&drawing).is_err());
}

#[test]
fn profile_flow_shapes_and_direction_are_validated() {
    let mut drawing = process();
    drawing.elements[3].shape = Shape::Diamond;
    assert!(validate_document(&drawing).is_err());
    let mut drawing = process();
    if let Shape::Arrow { bidirectional, .. } = &mut drawing.elements[6].shape {
        *bidirectional = true;
    }
    assert!(validate_document(&drawing).is_err());
    let mut drawing = process();
    drawing.document.as_mut().unwrap().profile = CanvasProfileV1::Architecture;
    assert!(validate_document(&drawing).is_err());
}

#[test]
fn ownership_and_lanes_cannot_form_cycles_or_point_to_wrong_kinds() {
    for mutation in 0..6 {
        let mut drawing = process();
        let document = drawing.document.as_mut().unwrap();
        match mutation {
            0 => document.elements[0].participant_id = Some("pool".into()),
            1 => document.elements[1].participant_id = Some("lane_b".into()),
            2 => document.elements[4].lane_id = Some("task".into()),
            3 => document.elements[4].participant_id = None,
            4 => document.elements[4].lane_id = Some("missing".into()),
            _ => document.elements[1].lane_id = Some("lane_b".into()),
        }
        assert!(validate_document(&drawing).is_err(), "mutation {mutation}");
    }
    let mut drawing = process();
    add(
        &mut drawing,
        node("other_pool", CanvasSemanticKindV1::Participant),
    );
    drawing.document.as_mut().unwrap().elements[1].participant_id = Some("other_pool".into());
    assert!(validate_document(&drawing).is_err());
}

#[test]
fn annotation_associations_are_not_control_flows() {
    use CanvasSemanticKindV1::*;
    let mut drawing = process();
    add(&mut drawing, node("note", Annotation));
    add(&mut drawing, edge("explains", Association, "note", "task"));
    assert!(validate_document(&drawing).is_ok());
    drawing
        .document
        .as_mut()
        .unwrap()
        .elements
        .last_mut()
        .unwrap()
        .source_id = Some("start".into());
    assert!(validate_document(&drawing).is_err());
}

#[test]
fn semantic_schema_is_closed_and_versioned() {
    let value = serde_json::to_value(process()).unwrap();
    for location in ["document", "element", "provenance"] {
        let mut candidate = value.clone();
        match location {
            "document" => candidate["document"]["unknown"] = true.into(),
            "element" => candidate["document"]["elements"][0]["unknown"] = true.into(),
            _ => {
                candidate["document"]["provenance"] = serde_json::json!({"source_id":"source", "source_revision":1,"source_sha256":"a".repeat(64),"unknown":true})
            }
        }
        assert!(serde_json::from_value::<CanvasSceneV1>(candidate).is_err());
    }
    let mut candidate = value.clone();
    candidate["document"]["elements"][0]["kind"] = "script_task".into();
    assert!(serde_json::from_value::<CanvasSceneV1>(candidate).is_err());
    let mut drawing = process();
    drawing.document.as_mut().unwrap().schema_version = 2;
    assert!(validate_document(&drawing).is_err());
    let roundtrip: CanvasSceneV1 = serde_json::from_value(value).unwrap();
    assert_eq!(roundtrip, process());
}

#[test]
fn semantic_metadata_bounds_and_provenance_identity_are_validated() {
    for invalid_language in ["", "pt--BR", "../pt", "en\nUS", &"a".repeat(36)] {
        let mut drawing = process();
        drawing.document.as_mut().unwrap().language = invalid_language.into();
        assert!(validate_document(&drawing).is_err());
    }
    for invalid_source in ["../source", "source/path", "", &"a".repeat(129)] {
        let mut drawing = process();
        drawing.document.as_mut().unwrap().provenance = Some(CanvasProvenanceV1 {
            source_id: invalid_source.into(),
            source_revision: Some(1),
            source_sha256: "a".repeat(64),
        });
        assert!(validate_document(&drawing).is_err());
    }
    let mut drawing = process();
    drawing.document.as_mut().unwrap().narrative = "x".repeat(super::MAX_LABEL_CHARS + 1);
    assert!(validate_document(&drawing).is_err());
    let mut drawing = process();
    let repeated = drawing.document.as_ref().unwrap().elements[0].clone();
    drawing.document.as_mut().unwrap().elements = vec![repeated; super::MAX_ELEMENTS + 1];
    assert!(validate_document(&drawing).is_err());
}

#[test]
fn narrative_sanitization_and_canonical_order_are_stable_and_unicode_safe() {
    let mut drawing = process();
    drawing.document.as_mut().unwrap().narrative = "Explica\u{00e7}\u{00e3}o\r\n\u{67b6}\u{69cb}\rAPI_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz0123456789".into();
    drawing.document.as_mut().unwrap().provenance = Some(CanvasProvenanceV1 {
        source_id: "source_1".into(),
        source_revision: Some(1),
        source_sha256: "AB".repeat(32),
    });
    normalize_document(&mut drawing).unwrap();
    let normalized = drawing.clone();
    let document = drawing.document.as_ref().unwrap();
    assert!(document.narrative.contains("\u{67b6}\u{69cb}"));
    assert!(
        !document
            .narrative
            .contains("sk-proj-abcdefghijklmnopqrstuvwxyz0123456789")
    );
    assert!(!document.narrative.contains('\r'));
    assert_eq!(
        document.provenance.as_ref().unwrap().source_sha256,
        "ab".repeat(32)
    );
    normalize_document(&mut drawing).unwrap();
    assert_eq!(drawing, normalized);
}

#[test]
fn failed_semantic_normalization_keeps_original_state_unchanged() {
    let mut drawing = process();
    drawing.document.as_mut().unwrap().narrative =
        "Bearer eyJhbGciOiJIUzI1NiJ9.abcdefghijklmnopqrstuvwxyz.abcdefghijklmnopqrstuvwxyz".into();
    drawing.document.as_mut().unwrap().elements[4].lane_id = Some("missing".into());
    let before = drawing.clone();
    assert!(normalize_document(&mut drawing).is_err());
    assert_eq!(drawing, before);
}

#[test]
fn secret_shaped_opaque_ids_are_rejected_without_echoing_identity() {
    let sensitive_id = "sk-proj-syntheticfixture0123456789";
    for field in [
        "visual",
        "semantic",
        "participant",
        "lane",
        "source",
        "target",
        "provenance",
    ] {
        let mut drawing = process();
        match field {
            "visual" => drawing.elements[0].id = sensitive_id.into(),
            "semantic" => {
                drawing.document.as_mut().unwrap().elements[0].element_id = sensitive_id.into()
            }
            "participant" => {
                drawing.document.as_mut().unwrap().elements[4].participant_id =
                    Some(sensitive_id.into())
            }
            "lane" => {
                drawing.document.as_mut().unwrap().elements[4].lane_id = Some(sensitive_id.into())
            }
            "source" => {
                drawing.document.as_mut().unwrap().elements[6].source_id = Some(sensitive_id.into())
            }
            "target" => {
                drawing.document.as_mut().unwrap().elements[6].target_id = Some(sensitive_id.into())
            }
            _ => {
                drawing.document.as_mut().unwrap().provenance = Some(CanvasProvenanceV1 {
                    source_id: sensitive_id.into(),
                    source_revision: Some(1),
                    source_sha256: "a".repeat(64),
                })
            }
        }
        let error = validate_document(&drawing).unwrap_err().to_string();
        assert!(!error.contains(sensitive_id), "{field}");
        assert!(error.len() < 512);
        let before = drawing.clone();
        let error = normalize_document(&mut drawing).unwrap_err().to_string();
        assert!(!error.contains(sensitive_id), "{field}");
        assert_eq!(drawing, before);
    }
}

#[test]
fn every_supported_process_kind_roundtrips_with_explicit_typing() {
    use CanvasSemanticKindV1::*;
    let mut drawing = process();
    for (id, kind) in [
        ("generic", Task),
        ("service", ServiceTask),
        ("exclusive", ExclusiveGateway),
        ("parallel", ParallelGateway),
    ] {
        add(&mut drawing, owned_node(id, kind, "pool", None));
    }
    add(&mut drawing, node("remote_pool", Participant));
    add(
        &mut drawing,
        edge("message", MessageFlow, "service", "remote_pool"),
    );
    add(&mut drawing, node("note", Annotation));
    add(
        &mut drawing,
        edge("association", Association, "note", "parallel"),
    );
    assert!(validate_document(&drawing).is_ok());
    let serialized = serde_json::to_string(&drawing).unwrap();
    let restored: CanvasSceneV1 = serde_json::from_str(&serialized).unwrap();
    assert_eq!(drawing, restored);
}

#[test]
fn malformed_source_fingerprints_and_unsupported_narrative_controls_fail_closed() {
    for fingerprint in ["short".into(), "g".repeat(64), "a".repeat(65)] {
        let mut drawing = process();
        drawing.document.as_mut().unwrap().provenance = Some(CanvasProvenanceV1 {
            source_id: "source".into(),
            source_revision: None,
            source_sha256: fingerprint,
        });
        assert!(validate_document(&drawing).is_err());
    }
    let mut drawing = process();
    drawing.document.as_mut().unwrap().narrative.push('\0');
    assert!(normalize_document(&mut drawing).is_err());
}
