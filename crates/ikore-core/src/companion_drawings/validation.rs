//! Bounded input validation and host-owned text redaction.
use super::{
    CANVAS_SCHEMA_VERSION, CanvasElementKindV1, CanvasSceneV1, CanvasStrokeStyle, CanvasViewportV1,
    MAX_COORDINATE, MAX_ELEMENTS, MAX_LABEL_CHARS, MAX_SCENE_BYTES, MAX_TITLE_CHARS, MAX_ZOOM,
    MIN_ZOOM,
};
use crate::persistence_service::PersistenceServiceError as Error;
use std::collections::{HashMap, HashSet};

fn invalid(message: &str) -> Error {
    Error::Validation(message.into())
}

fn safe_text(text: &str) -> bool {
    text.chars().all(|c| {
        (!c.is_control() || matches!(c, '\n' | '\r' | '\t')) && c != '\u{fffe}' && c != '\u{ffff}'
    })
}

fn clean_text(text: &str) -> Result<String, Error> {
    if !safe_text(text) {
        return Err(invalid(
            "Drawing text contains unsupported control characters",
        ));
    }
    let redacted = crate::secret_redactor::redact_secrets(text)
        .map_err(|_| invalid("Drawing text could not be sanitized; nothing was saved"))?;
    Ok(redacted.text.replace("\r\n", "\n").replace('\r', "\n"))
}

pub fn sanitize_title(title: &str) -> Result<String, Error> {
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > MAX_TITLE_CHARS
        || title.contains(['\n', '\r', '\t'])
    {
        return Err(invalid(
            "Drawing title must be one line containing 1–120 characters",
        ));
    }
    let title = clean_text(title)?;
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid("Sanitized drawing title exceeds 120 characters"));
    }
    Ok(title)
}

pub fn validate_viewport(viewport: &CanvasViewportV1) -> Result<(), Error> {
    if !viewport.pan_x.is_finite()
        || !viewport.pan_y.is_finite()
        || !viewport.zoom.is_finite()
        || viewport.pan_x.abs() > MAX_COORDINATE * MAX_ZOOM
        || viewport.pan_y.abs() > MAX_COORDINATE * MAX_ZOOM
        || !(MIN_ZOOM..=MAX_ZOOM).contains(&viewport.zoom)
    {
        return Err(invalid("Drawing viewport is outside the supported range"));
    }
    Ok(())
}

/// Normalize a valid candidate without silently repairing broken references.
pub fn normalize_scene(scene: &CanvasSceneV1) -> Result<CanvasSceneV1, Error> {
    if scene.schema_version != CANVAS_SCHEMA_VERSION {
        return Err(invalid("Unsupported drawing schema version"));
    }
    if scene.elements.len() > MAX_ELEMENTS {
        return Err(invalid("A drawing supports at most 1000 elements"));
    }
    let mut ids = HashMap::new();
    for element in &scene.elements {
        if element.id.is_empty()
            || element.id.len() > 128
            || !element
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid("Drawing element ID is invalid"));
        }
        if ids.insert(element.id.as_str(), element).is_some() {
            return Err(invalid("Drawing element IDs must be unique"));
        }
        if element.label.chars().count() > MAX_LABEL_CHARS {
            return Err(invalid("Drawing labels support at most 4000 characters"));
        }
        if ![element.x, element.y, element.width, element.height]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE)
        {
            return Err(invalid(
                "Drawing coordinates must be finite and within the supported range",
            ));
        }
        if let Some(points) = element.shape.points() {
            if !points
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE)
            {
                return Err(invalid("Drawing connector points are invalid"));
            }
            if (points[1][0] - points[0][0]).hypot(points[1][1] - points[0][1]) < 0.001 {
                return Err(invalid("Drawing connectors must have a nonzero length"));
            }
        } else if element.width.abs() < 0.001 || element.height.abs() < 0.001 {
            return Err(invalid("Drawing shapes must have positive dimensions"));
        }
        if let CanvasElementKindV1::Text { font_size } = element.shape
            && (!font_size.is_finite() || !(8.0..=96.0).contains(&font_size))
        {
            return Err(invalid("Drawing text size must be between 8 and 96 units"));
        }
        if element
            .label_font_size
            .is_some_and(|size| !size.is_finite() || !(8.0..=96.0).contains(&size))
        {
            return Err(invalid("Drawing label size must be between 8 and 96 units"));
        }
    }
    for element in &scene.elements {
        if let Some(container_id) = &element.container_id {
            let Some(container) = ids.get(container_id.as_str()) else {
                return Err(invalid("Drawing container does not exist"));
            };
            if container_id == &element.id
                || !matches!(container.shape, CanvasElementKindV1::Rectangle { .. })
                || container.stroke_style != CanvasStrokeStyle::Dashed
                || element.shape.is_connector()
            {
                return Err(invalid("Drawing container reference is invalid"));
            }
            let mut seen = HashSet::from([element.id.as_str()]);
            let mut next = Some(container_id.as_str());
            while let Some(id) = next {
                if !seen.insert(id) {
                    return Err(invalid("Drawing containers must not form a cycle"));
                }
                next = ids
                    .get(id)
                    .and_then(|parent| parent.container_id.as_deref());
            }
        }
        if let CanvasElementKindV1::Arrow {
            start_binding,
            end_binding,
            ..
        } = &element.shape
        {
            for binding in [start_binding, end_binding].into_iter().flatten() {
                let Some(target) = ids.get(binding.element_id.as_str()) else {
                    return Err(invalid("Drawing connection target does not exist"));
                };
                if binding.element_id == element.id
                    || !target.shape.is_binding_target()
                    || !binding
                        .anchor
                        .iter()
                        .all(|v| v.is_finite() && (-1.0..=1.0).contains(v))
                    || binding.anchor[0].abs() + binding.anchor[1].abs() < 0.0001
                    || !binding.gap.is_finite()
                    || !(0.0..=1000.0).contains(&binding.gap)
                {
                    return Err(invalid("Drawing connection anchor is invalid"));
                }
            }
        }
    }
    let mut normalized = scene.clone();
    for element in &mut normalized.elements {
        element.label = clean_text(&element.label)?;
        if element.label.chars().count() > MAX_LABEL_CHARS {
            return Err(invalid("Sanitized drawing label exceeds 4000 characters"));
        }
        if let Some(points) = super::geometry::connector_world_points(element) {
            super::geometry::set_connector_points(element, points);
        } else {
            if element.width < 0.0 {
                element.x += element.width;
                element.width = -element.width;
            }
            if element.height < 0.0 {
                element.y += element.height;
                element.height = -element.height;
            }
            if matches!(element.shape, CanvasElementKindV1::Text { .. }) {
                let label = super::geometry::label_bounds(element);
                element.width = label.width.max(super::geometry::label_font_size(element));
                element.height = label.height;
            }
        }
        if [
            element.x,
            element.y,
            element.x + element.width,
            element.y + element.height,
        ]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > MAX_COORDINATE)
        {
            return Err(invalid(
                "Normalized drawing bounds exceed the supported range",
            ));
        }
    }
    super::geometry::update_bindings(&mut normalized);
    super::geometry::update_containers(&mut normalized);
    for element in &normalized.elements {
        if [
            element.x,
            element.y,
            element.x + element.width,
            element.y + element.height,
        ]
        .iter()
        .any(|v| !v.is_finite() || v.abs() > MAX_COORDINATE)
        {
            return Err(invalid(
                "Resolved drawing bounds exceed the supported range",
            ));
        }
        if let Some([start, end]) = super::geometry::connector_world_points(element)
            && (end[0] - start[0]).hypot(end[1] - start[1]) < 0.001
        {
            return Err(invalid("Resolved drawing connector has a zero length"));
        }
    }
    normalized.elements = super::geometry::paint_order(&normalized)
        .into_iter()
        .cloned()
        .collect();
    super::semantics::normalize_document(&mut normalized)?;
    let json =
        serde_json::to_string(&normalized).map_err(|_| invalid("Drawing serialization failed"))?;
    if json.len() > MAX_SCENE_BYTES {
        return Err(invalid("Drawing scene exceeds the 4 MiB limit"));
    }
    Ok(normalized)
}

pub fn scene_json(scene: &CanvasSceneV1) -> Result<String, Error> {
    let normalized = normalize_scene(scene)?;
    let json = serde_json::to_string_pretty(&normalized)
        .map_err(|_| invalid("Drawing serialization failed"))?;
    if json.len() > MAX_SCENE_BYTES {
        return Err(invalid("Drawing export exceeds the 4 MiB limit"));
    }
    Ok(json + "\n")
}
