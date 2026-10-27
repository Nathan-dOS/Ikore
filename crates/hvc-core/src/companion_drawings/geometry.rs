//! Pure geometry shared by the editor, persistence validation and SVG export.
use super::{
    ArrowBindingV1, CanvasElementKindV1, CanvasElementV1, CanvasSceneV1, CanvasStrokeStyle,
    CanvasViewportV1, MAX_ZOOM, MIN_ZOOM,
};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Bounds {
    pub fn centre(self) -> [f32; 2] {
        [self.x + self.width / 2.0, self.y + self.height / 2.0]
    }

    pub fn contains(self, point: [f32; 2]) -> bool {
        point[0] >= self.x
            && point[0] <= self.x + self.width
            && point[1] >= self.y
            && point[1] <= self.y + self.height
    }

    pub fn intersects(self, other: Self) -> bool {
        self.x <= other.x + other.width
            && self.x + self.width >= other.x
            && self.y <= other.y + other.height
            && self.y + self.height >= other.y
    }

    pub fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }

    pub fn expanded(self, margin: f32) -> Self {
        Self {
            x: self.x - margin,
            y: self.y - margin,
            width: self.width + margin * 2.0,
            height: self.height + margin * 2.0,
        }
    }

    pub fn contains_bounds(self, other: Self) -> bool {
        self.contains([other.x, other.y])
            && self.contains([other.x + other.width, other.y + other.height])
    }
}

pub fn element_bounds(element: &CanvasElementV1) -> Bounds {
    Bounds {
        x: element.x,
        y: element.y,
        width: element.width,
        height: element.height,
    }
}

pub fn label_font_size(element: &CanvasElementV1) -> f32 {
    match element.shape {
        CanvasElementKindV1::Text { font_size } => font_size,
        _ => 16.0,
    }
}

/// Explicit newlines are the same layout units in both rendering backends.
pub fn label_lines(element: &CanvasElementV1) -> Vec<&str> {
    element.label.split('\n').collect()
}

pub fn label_bounds(element: &CanvasElementV1) -> Bounds {
    let font_size = label_font_size(element);
    let lines = label_lines(element);
    let width = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as f32
        * font_size;
    let height = lines.len() as f32 * font_size * 1.3;
    if matches!(element.shape, CanvasElementKindV1::Text { .. }) {
        Bounds {
            x: element.x,
            y: element.y,
            width,
            height,
        }
    } else {
        let centre = element_bounds(element).centre();
        // Keep connector strokes below their labels in both native and SVG output.
        let top = if element.shape.is_connector() {
            centre[1] - height - 6.0
        } else {
            centre[1] - height / 2.0
        };
        Bounds {
            x: centre[0] - width / 2.0,
            y: top,
            width,
            height,
        }
    }
}

pub fn scene_bounds(scene: &CanvasSceneV1) -> Bounds {
    scene
        .elements
        .iter()
        .map(|element| {
            let shape = element_bounds(element).expanded(element.stroke_width.units() / 2.0 + 14.0);
            if element.label.is_empty() {
                shape
            } else {
                shape.union(label_bounds(element))
            }
        })
        .reduce(Bounds::union)
        .unwrap_or(Bounds {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 400.0,
        })
}

pub fn world_to_screen(viewport: &CanvasViewportV1, point: [f32; 2]) -> [f32; 2] {
    [
        point[0] * viewport.zoom + viewport.pan_x,
        point[1] * viewport.zoom + viewport.pan_y,
    ]
}

pub fn screen_to_world(viewport: &CanvasViewportV1, point: [f32; 2]) -> [f32; 2] {
    [
        (point[0] - viewport.pan_x) / viewport.zoom,
        (point[1] - viewport.pan_y) / viewport.zoom,
    ]
}

pub fn zoom_at(viewport: &mut CanvasViewportV1, screen_point: [f32; 2], zoom: f32) {
    if !zoom.is_finite() || !screen_point.iter().all(|v| v.is_finite()) {
        return;
    }
    let world = screen_to_world(viewport, screen_point);
    viewport.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    viewport.pan_x = screen_point[0] - world[0] * viewport.zoom;
    viewport.pan_y = screen_point[1] - world[1] * viewport.zoom;
}

pub fn connector_world_points(element: &CanvasElementV1) -> Option<[[f32; 2]; 2]> {
    element
        .shape
        .points()
        .map(|points| points.map(|p| [p[0] + element.x, p[1] + element.y]))
}

pub fn set_connector_points(element: &mut CanvasElementV1, points: [[f32; 2]; 2]) {
    if !element.shape.is_connector() {
        return;
    }
    element.x = points[0][0].min(points[1][0]);
    element.y = points[0][1].min(points[1][1]);
    element.width = (points[1][0] - points[0][0]).abs();
    element.height = (points[1][1] - points[0][1]).abs();
    let local = points.map(|p| [p[0] - element.x, p[1] - element.y]);
    match &mut element.shape {
        CanvasElementKindV1::Arrow { points, .. } | CanvasElementKindV1::Line { points } => {
            *points = local
        }
        _ => {}
    }
}

pub fn distance_to_segment(point: [f32; 2], start: [f32; 2], end: [f32; 2]) -> f32 {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let denominator = dx * dx + dy * dy;
    let t = if denominator <= f32::EPSILON {
        0.0
    } else {
        (((point[0] - start[0]) * dx + (point[1] - start[1]) * dy) / denominator).clamp(0.0, 1.0)
    };
    (point[0] - (start[0] + t * dx)).hypot(point[1] - (start[1] + t * dy))
}

pub fn hit_element(element: &CanvasElementV1, point: [f32; 2], tolerance: f32) -> bool {
    if let Some([start, end]) = connector_world_points(element) {
        return distance_to_segment(point, start, end)
            <= tolerance.max(element.stroke_width.units() / 2.0);
    }
    let bounds = element_bounds(element).expanded(tolerance);
    if !bounds.contains(point) {
        return false;
    }
    let centre = bounds.centre();
    let dx = (point[0] - centre[0]) / (bounds.width / 2.0).max(0.001);
    let dy = (point[1] - centre[1]) / (bounds.height / 2.0).max(0.001);
    match element.shape {
        CanvasElementKindV1::Diamond => dx.abs() + dy.abs() <= 1.0,
        CanvasElementKindV1::Ellipse => dx * dx + dy * dy <= 1.0,
        _ => true,
    }
}

/// Boundaries paint behind their descendants regardless of creation order.
/// Depth is resolved once per element; sibling layers retain their explicit z order.
pub fn paint_order(scene: &CanvasSceneV1) -> Vec<&CanvasElementV1> {
    let indices = scene
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| (element.id.as_str(), index))
        .collect::<std::collections::HashMap<_, _>>();
    let parents = scene
        .elements
        .iter()
        .map(|element| {
            element
                .container_id
                .as_deref()
                .and_then(|id| indices.get(id).copied())
        })
        .collect::<Vec<_>>();
    let mut depths = vec![None; scene.elements.len()];
    for start in 0..scene.elements.len() {
        if depths[start].is_some() {
            continue;
        }
        let mut chain = Vec::new();
        let mut visiting = std::collections::HashSet::new();
        let mut next = Some(start);
        let mut depth = 0_usize;
        while let Some(index) = next {
            if let Some(known) = depths[index] {
                depth = known + 1;
                break;
            }
            if !visiting.insert(index) {
                // Invalid previews remain bounded; persisted cycles are rejected by validation.
                for index in &chain {
                    depths[*index] = Some(0);
                }
                chain.clear();
                break;
            }
            chain.push(index);
            next = parents[index];
        }
        for index in chain.into_iter().rev() {
            depths[index] = Some(depth);
            depth += 1;
        }
    }
    let mut ordered = scene.elements.iter().enumerate().collect::<Vec<_>>();
    ordered.sort_by(|(ai, a), (bi, b)| {
        (depths[*ai].unwrap_or(0), a.z_index, &a.id).cmp(&(
            depths[*bi].unwrap_or(0),
            b.z_index,
            &b.id,
        ))
    });
    ordered.into_iter().map(|(_, element)| element).collect()
}

pub fn hit_test(scene: &CanvasSceneV1, point: [f32; 2], tolerance: f32) -> Option<String> {
    paint_order(scene)
        .into_iter()
        .rev()
        .find(|element| hit_element(element, point, tolerance))
        .map(|element| element.id.clone())
}

fn border_anchor(target: &CanvasElementV1, anchor: [f32; 2]) -> [f32; 2] {
    let mut direction = anchor;
    if direction[0].abs() + direction[1].abs() < 0.0001 {
        direction = [1.0, 0.0];
    }
    let scale = match target.shape {
        CanvasElementKindV1::Ellipse => direction[0].hypot(direction[1]),
        CanvasElementKindV1::Diamond => direction[0].abs() + direction[1].abs(),
        _ => direction[0].abs().max(direction[1].abs()),
    };
    [direction[0] / scale, direction[1] / scale]
}

pub fn binding_point(target: &CanvasElementV1, binding: &ArrowBindingV1) -> [f32; 2] {
    let anchor = border_anchor(target, binding.anchor);
    let centre = element_bounds(target).centre();
    let offset = [
        anchor[0] * target.width / 2.0,
        anchor[1] * target.height / 2.0,
    ];
    let length = offset[0].hypot(offset[1]).max(0.0001);
    [
        centre[0] + offset[0] + offset[0] / length * binding.gap,
        centre[1] + offset[1] + offset[1] / length * binding.gap,
    ]
}

pub fn snap_binding(
    scene: &CanvasSceneV1,
    point: [f32; 2],
    tolerance: f32,
    exclude_id: Option<&str>,
) -> Option<ArrowBindingV1> {
    scene
        .elements
        .iter()
        .filter(|target| target.shape.is_binding_target() && Some(target.id.as_str()) != exclude_id)
        .filter_map(|target| {
            let centre = element_bounds(target).centre();
            let anchor = border_anchor(
                target,
                [
                    (point[0] - centre[0]) / (target.width / 2.0).max(0.001),
                    (point[1] - centre[1]) / (target.height / 2.0).max(0.001),
                ],
            );
            let binding = ArrowBindingV1 {
                element_id: target.id.clone(),
                anchor,
                gap: 0.0,
            };
            let border = binding_point(target, &binding);
            let distance = (point[0] - border[0]).hypot(point[1] - border[1]);
            (distance <= tolerance).then_some((distance, target.z_index, binding))
        })
        .min_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.element_id.cmp(&b.2.element_id))
        })
        .map(|(_, _, binding)| binding)
}

pub fn update_bindings(scene: &mut CanvasSceneV1) {
    let targets = scene
        .elements
        .iter()
        .filter(|element| element.shape.is_binding_target())
        .map(|element| (element.id.clone(), element.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    for element in &mut scene.elements {
        let Some(mut endpoints) = connector_world_points(element) else {
            continue;
        };
        if let CanvasElementKindV1::Arrow {
            start_binding,
            end_binding,
            ..
        } = &mut element.shape
        {
            for (endpoint, binding) in endpoints.iter_mut().zip([start_binding, end_binding]) {
                if let Some(current) = binding {
                    if let Some(target) = targets.get(&current.element_id) {
                        *endpoint = binding_point(target, current);
                    } else {
                        *binding = None;
                    }
                }
            }
            set_connector_points(element, endpoints);
        }
    }
}

pub fn update_containers(scene: &mut CanvasSceneV1) {
    let containers = scene
        .elements
        .iter()
        .filter(|element| {
            matches!(element.shape, CanvasElementKindV1::Rectangle { .. })
                && element.stroke_style == CanvasStrokeStyle::Dashed
        })
        .map(|element| (element.id.clone(), element_bounds(element)))
        .collect::<Vec<_>>();
    for element in &mut scene.elements {
        let bounds = element_bounds(element);
        element.container_id = if element.shape.is_connector() {
            None
        } else {
            containers
                .iter()
                .filter(|(id, outer)| {
                    id != &element.id
                        && outer.width * outer.height > bounds.width * bounds.height
                        && outer.contains_bounds(bounds)
                })
                .min_by(|a, b| {
                    (a.1.width * a.1.height)
                        .total_cmp(&(b.1.width * b.1.height))
                        .then_with(|| a.0.cmp(&b.0))
                })
                .map(|(id, _)| id.clone())
        };
    }
}

/// Clears references only; deletion of the element itself belongs to the editor command.
pub fn detach_element(scene: &mut CanvasSceneV1, id: &str) {
    for element in &mut scene.elements {
        if element.container_id.as_deref() == Some(id) {
            element.container_id = None;
        }
        if let CanvasElementKindV1::Arrow {
            start_binding,
            end_binding,
            ..
        } = &mut element.shape
        {
            for binding in [start_binding, end_binding] {
                if binding
                    .as_ref()
                    .is_some_and(|binding| binding.element_id == id)
                {
                    *binding = None;
                }
            }
        }
    }
}
