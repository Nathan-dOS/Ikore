//! What the canvas paints itself: the links between stages and the outline
//! of a node while it sways. Nodes at rest are plain elements, with sharp
//! text; only a moving node becomes a bubble.

use std::f32::consts::TAU;

use gpui::{Bounds, PathBuilder, Pixels, Point, Rgba, Window, point, px};

use super::constants::{BUBBLE_POINTS, WOBBLE_RADIUS};
use super::physics::{Mode, Vec2};
use crate::theme;

/// A swaying node's outline, in canvas-local screen pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Bubble {
    pub(super) center: Vec2,
    /// Half the node's width and height, zoomed.
    pub(super) half: Vec2,
    /// The stretch along the motion and the motion's direction.
    pub(super) stretch: (f32, Vec2),
    pub(super) modes: [Mode; 2],
    pub(super) zoom: f32,
    pub(super) fill: Rgba,
    pub(super) stroke: Rgba,
}

fn to_point(bounds: Bounds<Pixels>, at: Vec2) -> Point<Pixels> {
    point(bounds.origin.x + px(at.x), bounds.origin.y + px(at.y))
}

/// From the right edge of each stage to the left edge of the next, as a
/// curve that bends when the nodes move; the ends never change.
pub(super) fn links(
    bounds: Bounds<Pixels>,
    links: &[(Vec2, Vec2)],
    zoom: f32,
    window: &mut Window,
) {
    let half = super::constants::NODE_WIDTH * zoom / 2.0;
    for (from, to) in links {
        let start = Vec2::new(from.x + half, from.y);
        let end = Vec2::new(to.x - half, to.y);
        let reach = ((end.x - start.x).abs() / 2.0).max(16.0 * zoom);
        let mut path = PathBuilder::stroke(px((1.5 * zoom).max(1.0)));
        path.move_to(to_point(bounds, start));
        path.cubic_bezier_to(
            to_point(bounds, end),
            to_point(bounds, Vec2::new(start.x + reach, start.y)),
            to_point(bounds, Vec2::new(end.x - reach, end.y)),
        );
        if let Ok(path) = path.build() {
            window.paint_path(path, theme::border_strong());
        }
    }
}

/// A point of the node's superellipse at angle `theta`, around the origin.
fn superellipse(theta: f32, half: Vec2) -> Vec2 {
    let (sin, cos) = theta.sin_cos();
    let shape = |value: f32| value.signum() * value.abs().sqrt();
    Vec2::new(half.x * shape(cos), half.y * shape(sin))
}

/// The outline: the node's superellipse, pushed in and out by the sway's
/// modes, stretched along the motion and squeezed across it.
pub(super) fn outline(bubble: &Bubble) -> Vec<Vec2> {
    let (factor, direction) = bubble.stretch;
    let across = Vec2::new(-direction.y, direction.x);
    (0..BUBBLE_POINTS)
        .map(|index| {
            let theta = TAU * index as f32 / BUBBLE_POINTS as f32;
            let base = superellipse(theta, bubble.half);
            let sway: f32 = bubble
                .modes
                .iter()
                .enumerate()
                .map(|(order, mode)| {
                    mode.amplitude * ((order as f32 + 2.0) * theta + mode.phase).sin()
                })
                .sum();
            let radial = base.normalized() * (WOBBLE_RADIUS * bubble.zoom * sway);
            let local = base + radial;
            let shaped = if direction == Vec2::ZERO {
                local
            } else {
                let along = local.x * direction.x + local.y * direction.y;
                let side = local.x * across.x + local.y * across.y;
                direction * (along * factor) + across * (side / factor)
            };
            bubble.center + shaped
        })
        .collect()
}

pub(super) fn bubbles(bounds: Bounds<Pixels>, bubbles: &[Bubble], window: &mut Window) {
    for bubble in bubbles {
        let points = outline(bubble);
        let Some(first) = points.first().copied() else {
            continue;
        };
        let mut fill = PathBuilder::fill();
        let mut stroke = PathBuilder::stroke(px(1.0));
        for builder in [&mut fill, &mut stroke] {
            builder.move_to(to_point(bounds, first));
            for point in points.iter().skip(1) {
                builder.line_to(to_point(bounds, *point));
            }
            builder.close();
        }
        if let Ok(path) = fill.build() {
            window.paint_path(path, bubble.fill);
        }
        if let Ok(path) = stroke.build() {
            window.paint_path(path, bubble.stroke);
        }
    }
}
