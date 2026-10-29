//! Bounded, world-anchored dots submitted as one non-overlapping GPUI layer.
//!
//! A separate scene ordering query for each dot is expensive: GPUI's bounds tree
//! has to account for every earlier dot before the shapes can be painted. Within
//! `paint_layer`, dots share one order; their quads still use the native batch.
use gpui::{Bounds, Pixels, Window, fill, point, px, rgb, size};
use hvc_core::companion_drawings::{CanvasBackgroundMode, CanvasViewportV1};

const WORLD_SPACING: f32 = 24.;
const MIN_SCREEN_SPACING: f32 = 24.;
const MAX_DOTS: usize = 4096;

#[derive(Debug)]
struct GridLayout {
    spacing: f32,
    start_x: f32,
    start_y: f32,
    columns: usize,
    rows: usize,
}

impl GridLayout {
    fn new(width: f32, height: f32, view: &CanvasViewportV1) -> Option<Self> {
        if ![width, height, view.zoom, view.pan_x, view.pan_y]
            .into_iter()
            .all(f32::is_finite)
            || width <= 0.
            || height <= 0.
            || view.zoom <= 0.
        {
            return None;
        }
        let mut spacing = WORLD_SPACING * view.zoom;
        loop {
            if !spacing.is_finite() || spacing <= 0. {
                return None;
            }
            let start_x = view.pan_x.rem_euclid(spacing);
            let start_y = view.pan_y.rem_euclid(spacing);
            let columns = ((width - start_x) / spacing).ceil().max(0.) as usize;
            let rows = ((height - start_y) / spacing).ceil().max(0.) as usize;
            if spacing >= MIN_SCREEN_SPACING && columns.saturating_mul(rows) <= MAX_DOTS {
                return Some(Self {
                    spacing,
                    start_x,
                    start_y,
                    columns,
                    rows,
                });
            }
            // Skip alternate world grid lines, retaining a world-origin anchor
            // when zoomed out or when the viewport would otherwise be too dense.
            spacing *= 2.;
        }
    }
}

pub(crate) fn paint_grid(
    bounds: Bounds<Pixels>,
    view: &CanvasViewportV1,
    mode: CanvasBackgroundMode,
    window: &mut Window,
) {
    let Some(grid) = GridLayout::new(bounds.size.width.into(), bounds.size.height.into(), view)
    else {
        return;
    };
    let color = rgb(match mode {
        CanvasBackgroundMode::Dark => 0x1d2937,
        CanvasBackgroundMode::Light => 0xd8dde5,
    });
    // paint_layer intersects the layer with the existing content mask. Dots do
    // not overlap, so sharing an order is safe; later shapes remain above them.
    // This is batching, not a raster cache: no texture or stale pan/zoom state.
    window.paint_layer(bounds, |window| {
        for row in 0..grid.rows {
            let y = grid.start_y + row as f32 * grid.spacing;
            for column in 0..grid.columns {
                let x = grid.start_x + column as f32 * grid.spacing;
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + px(x), bounds.top() + px(y)),
                        size(px(1.), px(1.)),
                    ),
                    color,
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_grid_retains_native_density_at_normal_zoom() {
        let grid = GridLayout::new(1280., 820., &CanvasViewportV1::default()).unwrap();
        assert_eq!(grid.spacing, 24.);
        assert_eq!((grid.columns, grid.rows), (54, 35));
        assert_eq!(grid.columns * grid.rows, 1890);
    }

    #[test]
    fn canvas_grid_stays_world_anchored_when_panning_in_either_direction() {
        let view = CanvasViewportV1 {
            pan_x: -17.,
            pan_y: 35.,
            zoom: 0.5,
        };
        let grid = GridLayout::new(1280., 820., &view).unwrap();
        assert_eq!(grid.spacing, 24.);
        assert_eq!((grid.start_x, grid.start_y), (7., 11.));
        let shifted = GridLayout::new(
            1280.,
            820.,
            &CanvasViewportV1 {
                pan_x: view.pan_x + grid.spacing * 2.,
                pan_y: view.pan_y - grid.spacing * 3.,
                ..view
            },
        )
        .unwrap();
        assert_eq!(grid.start_x, shifted.start_x);
        assert_eq!(grid.start_y, shifted.start_y);
        // Each visible dot maps onto an integer multiple of the base world grid.
        assert_eq!(
            ((grid.start_x - view.pan_x) / view.zoom) % WORLD_SPACING,
            0.
        );
        assert_eq!(
            ((grid.start_y - view.pan_y) / view.zoom) % WORLD_SPACING,
            0.
        );
    }

    #[test]
    fn canvas_grid_bounds_work_for_zoom_and_extreme_viewports() {
        for zoom in [0.1, 0.25, 0.5, 1., 2., 4.] {
            for (width, height) in [(1., 1.), (1280., 820.), (3840., 2160.), (32_768., 32_768.)] {
                let view = CanvasViewportV1 {
                    pan_x: -1_000_000.,
                    pan_y: 999_999.,
                    zoom,
                };
                let grid = GridLayout::new(width, height, &view).unwrap();
                assert!(grid.spacing >= MIN_SCREEN_SPACING);
                assert!(grid.columns * grid.rows <= MAX_DOTS);
                assert!(grid.start_x >= 0. && grid.start_x < grid.spacing);
                assert!(grid.start_y >= 0. && grid.start_y < grid.spacing);
                if grid.columns > 0 {
                    assert!(grid.start_x + (grid.columns - 1) as f32 * grid.spacing < width);
                }
                if grid.rows > 0 {
                    assert!(grid.start_y + (grid.rows - 1) as f32 * grid.spacing < height);
                }
            }
        }
    }

    #[test]
    fn canvas_grid_rejects_empty_and_non_finite_geometry() {
        let view = CanvasViewportV1::default();
        for (width, height) in [
            (0., 820.),
            (1280., -1.),
            (f32::NAN, 820.),
            (1280., f32::INFINITY),
        ] {
            assert!(GridLayout::new(width, height, &view).is_none());
        }
        for invalid in [0., -1., f32::NAN, f32::INFINITY] {
            assert!(
                GridLayout::new(
                    1280.,
                    820.,
                    &CanvasViewportV1 {
                        zoom: invalid,
                        ..view
                    }
                )
                .is_none()
            );
        }
        assert!(
            GridLayout::new(
                1280.,
                820.,
                &CanvasViewportV1 {
                    pan_x: f32::NAN,
                    ..view
                }
            )
            .is_none()
        );
        assert!(
            GridLayout::new(
                1280.,
                820.,
                &CanvasViewportV1 {
                    pan_y: f32::INFINITY,
                    ..view
                }
            )
            .is_none()
        );
    }
}
