//! Small, deterministic vector paths shared by GPUI and SVG. No renderer or I/O types.
use super::{
    CanvasConnectorCurve as Curve, CanvasElementKindV1 as Kind, CanvasElementV1 as Element,
    CanvasStrokeRendering as Rendering,
    geometry::{self, Bounds},
};

pub type Point = [f32; 2];

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Move(Point),
    Line(Point),
    Quadratic(Point, Point),    // control, end
    Cubic(Point, Point, Point), // controls, end
    Close,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct VectorPath {
    pub commands: Vec<Command>,
    pub closed: bool,
}

impl VectorPath {
    /// The control hull conservatively contains every Bézier segment.
    pub fn control_bounds(&self) -> Option<Bounds> {
        self.commands
            .iter()
            .flat_map(|command| {
                match command {
                    Command::Move(p) | Command::Line(p) => [Some(*p), None, None],
                    Command::Quadratic(a, b) => [Some(*a), Some(*b), None],
                    Command::Cubic(a, b, c) => [Some(*a), Some(*b), Some(*c)],
                    Command::Close => [None, None, None],
                }
                .into_iter()
                .flatten()
            })
            .map(|p| Bounds {
                x: p[0],
                y: p[1],
                width: 0.,
                height: 0.,
            })
            .reduce(Bounds::union)
    }
}

fn mix(a: Point, b: Point, t: f32) -> Point {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn variation(element: &Element, index: usize) -> f32 {
    // Stable across sessions/backends. Never sample randomness while painting.
    let hash = element.id.bytes().fold(2_166_136_261_u32, |h, byte| {
        (h ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    let value = hash.wrapping_add((index as u32).wrapping_mul(2_654_435_761));
    0.65 + (value % 101) as f32 / 100. * 0.7
}

pub fn connector_control(element: &Element) -> Option<Point> {
    let [a, b] = geometry::connector_world_points(element)?;
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length = dx.hypot(dy);
    if length < 0.001 {
        return None;
    }
    let bend = match element.connector_curve {
        Curve::CurveLeft => -(length * 0.45).min(160.),
        Curve::CurveRight => (length * 0.45).min(160.),
        Curve::Straight if element.stroke_rendering == Rendering::Sketch => {
            (length * 0.035).min(3.) * variation(element, 0)
        }
        Curve::Straight => return None,
    };
    let mid = mix(a, b, 0.5);
    Some([mid[0] - dy / length * bend, mid[1] + dx / length * bend])
}

pub fn connector_at(element: &Element, t: f32) -> Option<Point> {
    let [a, b] = geometry::connector_world_points(element)?;
    Some(if let Some(c) = connector_control(element) {
        mix(mix(a, c, t), mix(c, b, t), t)
    } else {
        mix(a, b, t)
    })
}

/// A point behind the tip along its exact quadratic tangent.
pub fn connector_tangent_from(element: &Element, at_start: bool) -> Option<Point> {
    let points = geometry::connector_world_points(element)?;
    Some(connector_control(element).unwrap_or(points[usize::from(at_start)]))
}

fn side(path: &mut VectorPath, a: Point, b: Point, amount: f32) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let length = dx.hypot(dy).max(0.001);
    let amount = amount.min(length * 0.04);
    let c1 = mix(a, b, 0.32);
    let c2 = mix(a, b, 0.72);
    path.commands.push(Command::Cubic(
        [c1[0] - dy / length * amount, c1[1] + dx / length * amount],
        [
            c2[0] + dy / length * amount * 0.65,
            c2[1] - dx / length * amount * 0.65,
        ],
        b,
    ));
}

pub fn element_path(element: &Element) -> VectorPath {
    let mut path = VectorPath::default();
    if let Some([a, b]) = geometry::connector_world_points(element) {
        path.commands.push(Command::Move(a));
        path.commands
            .push(if let Some(control) = connector_control(element) {
                Command::Quadratic(control, b)
            } else {
                Command::Line(b)
            });
        return path;
    }
    let (x, y, w, h) = (element.x, element.y, element.width, element.height);
    let sketch = element.stroke_rendering == Rendering::Sketch;
    let amplitude = if sketch {
        (w.min(h) * 0.025).min(1.8)
    } else {
        0.
    };
    match element.shape {
        Kind::Text { .. } => return path,
        Kind::Ellipse => {
            let k = 0.552_284_8;
            let rx = w / 2.;
            let ry = h / 2.;
            let cx = x + rx;
            let cy = y + ry;
            let a = amplitude * variation(element, 1);
            path.commands = vec![
                Command::Move([cx, y]),
                Command::Cubic([cx + rx * k, y + a], [x + w, cy - ry * k], [x + w, cy]),
                Command::Cubic([x + w - a, cy + ry * k], [cx + rx * k, y + h], [cx, y + h]),
                Command::Cubic([cx - rx * k, y + h - a], [x, cy + ry * k], [x, cy]),
                Command::Cubic([x + a, cy - ry * k], [cx - rx * k, y], [cx, y]),
            ];
        }
        Kind::Rectangle { round: true } => {
            let r = 12_f32.min(w / 4.).min(h / 4.);
            let edges = [
                ([x + r, y], [x + w - r, y], [x + w, y], [x + w, y + r]),
                (
                    [x + w, y + r],
                    [x + w, y + h - r],
                    [x + w, y + h],
                    [x + w - r, y + h],
                ),
                (
                    [x + w - r, y + h],
                    [x + r, y + h],
                    [x, y + h],
                    [x, y + h - r],
                ),
                ([x, y + h - r], [x, y + r], [x, y], [x + r, y]),
            ];
            path.commands.push(Command::Move(edges[0].0));
            for (index, (a, b, corner, end)) in edges.into_iter().enumerate() {
                if sketch {
                    side(&mut path, a, b, amplitude * variation(element, index));
                } else {
                    path.commands.push(Command::Line(b));
                }
                path.commands.push(Command::Quadratic(corner, end));
            }
        }
        _ => {
            let points = if matches!(element.shape, Kind::Diamond) {
                [
                    [x + w / 2., y],
                    [x + w, y + h / 2.],
                    [x + w / 2., y + h],
                    [x, y + h / 2.],
                ]
            } else {
                [[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
            };
            path.commands.push(Command::Move(points[0]));
            for index in 0..4 {
                let (a, b) = (points[index], points[(index + 1) % 4]);
                if sketch {
                    side(&mut path, a, b, amplitude * variation(element, index));
                } else {
                    path.commands.push(Command::Line(b));
                }
            }
        }
    }
    path.commands.push(Command::Close);
    path.closed = true;
    path
}

pub fn arrowhead_path(element: &Element, at_start: bool) -> VectorPath {
    let Some(points) = geometry::connector_world_points(element) else {
        return VectorPath::default();
    };
    let tip = points[usize::from(!at_start)];
    let from = connector_tangent_from(element, at_start).unwrap_or(points[usize::from(at_start)]);
    let dx = tip[0] - from[0];
    let dy = tip[1] - from[1];
    let length = dx.hypot(dy);
    if length < 0.001 {
        return VectorPath::default();
    }
    let total_length = (points[1][0] - points[0][0]).hypot(points[1][1] - points[0][1]);
    let head = (element.stroke_width.units() * 3.5)
        .max(10.)
        .min(total_length / 2.);
    let (ux, uy) = (dx / length, dy / length);
    let base = [tip[0] - ux * head, tip[1] - uy * head];
    let a = [base[0] - uy * head * 0.45, base[1] + ux * head * 0.45];
    let b = [base[0] + uy * head * 0.45, base[1] - ux * head * 0.45];
    if element.stroke_rendering == Rendering::Sketch {
        VectorPath {
            commands: vec![
                Command::Move(a),
                Command::Quadratic(mix(a, tip, 0.55), tip),
                Command::Quadratic(mix(tip, b, 0.45), b),
            ],
            closed: false,
        }
    } else {
        VectorPath {
            commands: vec![
                Command::Move(tip),
                Command::Line(a),
                Command::Line(b),
                Command::Close,
            ],
            closed: true,
        }
    }
}
