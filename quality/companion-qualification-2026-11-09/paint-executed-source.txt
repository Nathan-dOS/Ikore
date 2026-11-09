use crate::{
    App, Bounds, Half, Hsla, LineLayout, Pixels, Point, Result, SharedString, StrikethroughStyle,
    TextAlign, UnderlineStyle, Window, WrapBoundary, WrappedLineLayout, black, fill, point, px,
    size,
};
use derive_more::{Deref, DerefMut};
use smallvec::SmallVec;
use std::sync::Arc;

/// Set the text decoration for a run of text.
#[derive(Debug, Clone)]
pub struct DecorationRun {
    /// The length of the run in utf-8 bytes.
    pub len: u32,

    /// The color for this run
    pub color: Hsla,

    /// The background color for this run
    pub background_color: Option<Hsla>,

    /// The underline style for this run
    pub underline: Option<UnderlineStyle>,

    /// The strikethrough style for this run
    pub strikethrough: Option<StrikethroughStyle>,
}

/// A line of text that has been shaped and decorated.
#[derive(Clone, Default, Debug, Deref, DerefMut)]
pub struct ShapedLine {
    #[deref]
    #[deref_mut]
    pub(crate) layout: Arc<LineLayout>,
    /// The text that was shaped for this line.
    pub text: SharedString,
    pub(crate) decoration_runs: SmallVec<[DecorationRun; 32]>,
}

impl ShapedLine {
    /// The length of the line in utf-8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.layout.len
    }

    /// Override the len, useful if you're rendering text a
    /// as text b (e.g. rendering invisibles).
    pub fn with_len(mut self, len: usize) -> Self {
        let layout = self.layout.as_ref();
        self.layout = Arc::new(LineLayout {
            font_size: layout.font_size,
            width: layout.width,
            ascent: layout.ascent,
            descent: layout.descent,
            runs: layout.runs.clone(),
            len,
        });
        self
    }

    /// Paint the line of text to the window.
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        paint_line(
            origin,
            &self.layout,
            line_height,
            TextAlign::default(),
            None,
            &self.decoration_runs,
            &[],
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint the background of the line to the window.
    pub fn paint_background(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        paint_line_background(
            origin,
            &self.layout,
            line_height,
            TextAlign::default(),
            None,
            &self.decoration_runs,
            &[],
            window,
            cx,
        )?;

        Ok(())
    }
}

/// A line of text that has been shaped, decorated, and wrapped by the text layout system.
#[derive(Clone, Default, Debug, Deref, DerefMut)]
pub struct WrappedLine {
    #[deref]
    #[deref_mut]
    pub(crate) layout: Arc<WrappedLineLayout>,
    /// The text that was shaped for this line.
    pub text: SharedString,
    pub(crate) decoration_runs: SmallVec<[DecorationRun; 32]>,
}

impl WrappedLine {
    /// The length of the underlying, unwrapped layout, in utf-8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.layout.len()
    }

    /// Paint this line of text to the window.
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        bounds: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let align_width = match bounds {
            Some(bounds) => Some(bounds.size.width),
            None => self.layout.wrap_width,
        };

        paint_line(
            origin,
            &self.layout.unwrapped_layout,
            line_height,
            align,
            align_width,
            &self.decoration_runs,
            &self.wrap_boundaries,
            window,
            cx,
        )?;

        Ok(())
    }

    /// Paint the background of line of text to the window.
    pub fn paint_background(
        &self,
        origin: Point<Pixels>,
        line_height: Pixels,
        align: TextAlign,
        bounds: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<()> {
        let align_width = match bounds {
            Some(bounds) => Some(bounds.size.width),
            None => self.layout.wrap_width,
        };

        paint_line_background(
            origin,
            &self.layout.unwrapped_layout,
            line_height,
            align,
            align_width,
            &self.decoration_runs,
            &self.wrap_boundaries,
            window,
            cx,
        )?;

        Ok(())
    }
}

#[cfg(test)]
std::thread_local! {
    static HVC009_GLYPH_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// HVC-GPUI-009. Keep this helper private to text_system::line.
// This bounds the existing max_glyph_bounds test, not physical raster bounds.
fn glyphs_outside_vertical_mask(
    origin_y: Pixels,
    layout: &LineLayout,
    line_height: Pixels,
    wrap_count: usize,
    mask: &Bounds<Pixels>,
    text_system: &crate::TextSystem,
) -> bool {
    let mask_top = f32::from(mask.top());
    let mask_bottom = f32::from(mask.bottom());
    let mut row_y = origin_y;
    if !f32::from(row_y).is_finite()
        || !f32::from(line_height).is_finite()
        || !mask_top.is_finite()
        || !mask_bottom.is_finite()
        || mask_bottom < mask_top
    {
        return false;
    }

    let mut top = f32::from(row_y);
    let mut bottom_row = top;
    // Match glyph_origin.y += line_height. Multiplication can round to a
    // smaller interval than repeated f32 additions for subpixel line heights.
    for _ in 0..wrap_count {
        row_y += line_height;
        let y = f32::from(row_y);
        if !y.is_finite() {
            return false;
        }
        top = top.min(y);
        bottom_row = bottom_row.max(y);
    }

    let mut max_height = 0.0_f32;
    for run in &layout.runs {
        let height = f32::from(
            text_system
                .bounding_box(run.font_id, layout.font_size)
                .size
                .height,
        );
        if !height.is_finite() || height < 0.0 {
            return false;
        }
        max_height = max_height.max(height);
    }
    let bottom = bottom_row + max_height;
    if !bottom.is_finite() {
        return false;
    }

    // Bounds::intersects uses strict comparisons at the edges.
    bottom <= mask_top || top >= mask_bottom
}

fn paint_line(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    decoration_runs: &[DecorationRun],
    wrap_boundaries: &[WrapBoundary],
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let line_bounds = Bounds::new(
        origin,
        size(
            layout.width,
            line_height * (wrap_boundaries.len() as f32 + 1.),
        ),
    );
    window.paint_layer(line_bounds, |window| {
        // HVC-GPUI-009: keep the layer/order/replay protocol, but avoid glyph
        // traversal only when the existing per-glyph visibility test cannot pass.
        if decoration_runs
            .iter()
            .all(|run| run.underline.is_none() && run.strikethrough.is_none())
            && glyphs_outside_vertical_mask(
                origin.y,
                layout,
                line_height,
                wrap_boundaries.len(),
                &window.content_mask().bounds,
                cx.text_system(),
            )
        {
            return Ok(());
        }
        let padding_top = (line_height - layout.ascent - layout.descent) / 2.;
        let baseline_offset = point(px(0.), padding_top + layout.ascent);
        let mut decoration_runs = decoration_runs.iter();
        let mut wraps = wrap_boundaries.iter().peekable();
        let mut run_end = 0;
        let mut color = black();
        let mut current_underline: Option<(Point<Pixels>, UnderlineStyle)> = None;
        let mut current_strikethrough: Option<(Point<Pixels>, StrikethroughStyle)> = None;
        let text_system = cx.text_system().clone();
        let mut glyph_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                px(0.0),
                &align,
                layout,
                wraps.peek(),
            ),
            origin.y,
        );
        let mut prev_glyph_position = Point::default();
        let mut max_glyph_size = size(px(0.), px(0.));
        let mut first_glyph_x = origin.x;
        for (run_ix, run) in layout.runs.iter().enumerate() {
            max_glyph_size = text_system.bounding_box(run.font_id, layout.font_size).size;

            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                #[cfg(test)]
                HVC009_GLYPH_VISITS.with(|count| count.set(count.get() + 1));
                glyph_origin.x += glyph.position.x - prev_glyph_position.x;
                if glyph_ix == 0 && run_ix == 0 {
                    first_glyph_x = glyph_origin.x;
                }

                if wraps.peek() == Some(&&WrapBoundary { run_ix, glyph_ix }) {
                    wraps.next();
                    if let Some((underline_origin, underline_style)) = current_underline.as_mut() {
                        if glyph_origin.x == underline_origin.x {
                            underline_origin.x -= max_glyph_size.width.half();
                        };
                        window.paint_underline(
                            *underline_origin,
                            glyph_origin.x - underline_origin.x,
                            underline_style,
                        );
                        underline_origin.x = origin.x;
                        underline_origin.y += line_height;
                    }
                    if let Some((strikethrough_origin, strikethrough_style)) =
                        current_strikethrough.as_mut()
                    {
                        if glyph_origin.x == strikethrough_origin.x {
                            strikethrough_origin.x -= max_glyph_size.width.half();
                        };
                        window.paint_strikethrough(
                            *strikethrough_origin,
                            glyph_origin.x - strikethrough_origin.x,
                            strikethrough_style,
                        );
                        strikethrough_origin.x = origin.x;
                        strikethrough_origin.y += line_height;
                    }

                    glyph_origin.x = aligned_origin_x(
                        origin,
                        align_width.unwrap_or(layout.width),
                        glyph.position.x,
                        &align,
                        layout,
                        wraps.peek(),
                    );
                    glyph_origin.y += line_height;
                }
                prev_glyph_position = glyph.position;

                let mut finished_underline: Option<(Point<Pixels>, UnderlineStyle)> = None;
                let mut finished_strikethrough: Option<(Point<Pixels>, StrikethroughStyle)> = None;
                if glyph.index >= run_end {
                    let mut style_run = decoration_runs.next();

                    // ignore style runs that apply to a partial glyph
                    while let Some(run) = style_run {
                        if glyph.index < run_end + (run.len as usize) {
                            break;
                        }
                        run_end += run.len as usize;
                        style_run = decoration_runs.next();
                    }

                    if let Some(style_run) = style_run {
                        if let Some((_, underline_style)) = &mut current_underline
                            && style_run.underline.as_ref() != Some(underline_style)
                        {
                            finished_underline = current_underline.take();
                        }
                        if let Some(run_underline) = style_run.underline.as_ref() {
                            current_underline.get_or_insert((
                                point(
                                    glyph_origin.x,
                                    glyph_origin.y + baseline_offset.y + (layout.descent * 0.618),
                                ),
                                UnderlineStyle {
                                    color: Some(run_underline.color.unwrap_or(style_run.color)),
                                    thickness: run_underline.thickness,
                                    wavy: run_underline.wavy,
                                },
                            ));
                        }
                        if let Some((_, strikethrough_style)) = &mut current_strikethrough
                            && style_run.strikethrough.as_ref() != Some(strikethrough_style)
                        {
                            finished_strikethrough = current_strikethrough.take();
                        }
                        if let Some(run_strikethrough) = style_run.strikethrough.as_ref() {
                            current_strikethrough.get_or_insert((
                                point(
                                    glyph_origin.x,
                                    glyph_origin.y
                                        + (((layout.ascent * 0.5) + baseline_offset.y) * 0.5),
                                ),
                                StrikethroughStyle {
                                    color: Some(run_strikethrough.color.unwrap_or(style_run.color)),
                                    thickness: run_strikethrough.thickness,
                                },
                            ));
                        }

                        run_end += style_run.len as usize;
                        color = style_run.color;
                    } else {
                        run_end = layout.len;
                        finished_underline = current_underline.take();
                        finished_strikethrough = current_strikethrough.take();
                    }
                }

                if let Some((mut underline_origin, underline_style)) = finished_underline {
                    if underline_origin.x == glyph_origin.x {
                        underline_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_underline(
                        underline_origin,
                        glyph_origin.x - underline_origin.x,
                        &underline_style,
                    );
                }

                if let Some((mut strikethrough_origin, strikethrough_style)) =
                    finished_strikethrough
                {
                    if strikethrough_origin.x == glyph_origin.x {
                        strikethrough_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_strikethrough(
                        strikethrough_origin,
                        glyph_origin.x - strikethrough_origin.x,
                        &strikethrough_style,
                    );
                }

                let max_glyph_bounds = Bounds {
                    origin: glyph_origin,
                    size: max_glyph_size,
                };

                let content_mask = window.content_mask();
                if max_glyph_bounds.intersects(&content_mask.bounds) {
                    if glyph.is_emoji {
                        window.paint_emoji(
                            glyph_origin + baseline_offset,
                            run.font_id,
                            glyph.id,
                            layout.font_size,
                        )?;
                    } else {
                        window.paint_glyph(
                            glyph_origin + baseline_offset,
                            run.font_id,
                            glyph.id,
                            layout.font_size,
                            color,
                        )?;
                    }
                }
            }
        }

        let mut last_line_end_x = first_glyph_x + layout.width;
        if let Some(boundary) = wrap_boundaries.last() {
            let run = &layout.runs[boundary.run_ix];
            let glyph = &run.glyphs[boundary.glyph_ix];
            last_line_end_x -= glyph.position.x;
        }

        if let Some((mut underline_start, underline_style)) = current_underline.take() {
            if last_line_end_x == underline_start.x {
                underline_start.x -= max_glyph_size.width.half()
            };
            window.paint_underline(
                underline_start,
                last_line_end_x - underline_start.x,
                &underline_style,
            );
        }

        if let Some((mut strikethrough_start, strikethrough_style)) = current_strikethrough.take() {
            if last_line_end_x == strikethrough_start.x {
                strikethrough_start.x -= max_glyph_size.width.half()
            };
            window.paint_strikethrough(
                strikethrough_start,
                last_line_end_x - strikethrough_start.x,
                &strikethrough_style,
            );
        }

        Ok(())
    })
}

fn paint_line_background(
    origin: Point<Pixels>,
    layout: &LineLayout,
    line_height: Pixels,
    align: TextAlign,
    align_width: Option<Pixels>,
    decoration_runs: &[DecorationRun],
    wrap_boundaries: &[WrapBoundary],
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let line_bounds = Bounds::new(
        origin,
        size(
            layout.width,
            line_height * (wrap_boundaries.len() as f32 + 1.),
        ),
    );
    window.paint_layer(line_bounds, |window| {
        // Keep the empty layer's ordering and replay operations, but avoid
        // querying font metrics or walking glyphs when no background can paint.
        if decoration_runs
            .iter()
            .all(|run| run.background_color.is_none())
        {
            return Ok(());
        }

        let mut decoration_runs = decoration_runs.iter();
        let mut wraps = wrap_boundaries.iter().peekable();
        let mut run_end = 0;
        let mut current_background: Option<(Point<Pixels>, Hsla)> = None;
        let text_system = cx.text_system().clone();
        let mut glyph_origin = point(
            aligned_origin_x(
                origin,
                align_width.unwrap_or(layout.width),
                px(0.0),
                &align,
                layout,
                wraps.peek(),
            ),
            origin.y,
        );
        let mut prev_glyph_position = Point::default();
        let mut max_glyph_size = size(px(0.), px(0.));
        for (run_ix, run) in layout.runs.iter().enumerate() {
            max_glyph_size = text_system.bounding_box(run.font_id, layout.font_size).size;

            for (glyph_ix, glyph) in run.glyphs.iter().enumerate() {
                glyph_origin.x += glyph.position.x - prev_glyph_position.x;

                if wraps.peek() == Some(&&WrapBoundary { run_ix, glyph_ix }) {
                    wraps.next();
                    if let Some((background_origin, background_color)) = current_background.as_mut()
                    {
                        if glyph_origin.x == background_origin.x {
                            background_origin.x -= max_glyph_size.width.half()
                        }
                        window.paint_quad(fill(
                            Bounds {
                                origin: *background_origin,
                                size: size(glyph_origin.x - background_origin.x, line_height),
                            },
                            *background_color,
                        ));
                        background_origin.x = origin.x;
                        background_origin.y += line_height;
                    }

                    glyph_origin.x = aligned_origin_x(
                        origin,
                        align_width.unwrap_or(layout.width),
                        glyph.position.x,
                        &align,
                        layout,
                        wraps.peek(),
                    );
                    glyph_origin.y += line_height;
                }
                prev_glyph_position = glyph.position;

                let mut finished_background: Option<(Point<Pixels>, Hsla)> = None;
                if glyph.index >= run_end {
                    let mut style_run = decoration_runs.next();

                    // ignore style runs that apply to a partial glyph
                    while let Some(run) = style_run {
                        if glyph.index < run_end + (run.len as usize) {
                            break;
                        }
                        run_end += run.len as usize;
                        style_run = decoration_runs.next();
                    }

                    if let Some(style_run) = style_run {
                        if let Some((_, background_color)) = &mut current_background
                            && style_run.background_color.as_ref() != Some(background_color)
                        {
                            finished_background = current_background.take();
                        }
                        if let Some(run_background) = style_run.background_color {
                            current_background.get_or_insert((
                                point(glyph_origin.x, glyph_origin.y),
                                run_background,
                            ));
                        }
                        run_end += style_run.len as usize;
                    } else {
                        run_end = layout.len;
                        finished_background = current_background.take();
                    }
                }

                if let Some((mut background_origin, background_color)) = finished_background {
                    let mut width = glyph_origin.x - background_origin.x;
                    if background_origin.x == glyph_origin.x {
                        background_origin.x -= max_glyph_size.width.half();
                    };
                    window.paint_quad(fill(
                        Bounds {
                            origin: background_origin,
                            size: size(width, line_height),
                        },
                        background_color,
                    ));
                }
            }
        }

        let mut last_line_end_x = origin.x + layout.width;
        if let Some(boundary) = wrap_boundaries.last() {
            let run = &layout.runs[boundary.run_ix];
            let glyph = &run.glyphs[boundary.glyph_ix];
            last_line_end_x -= glyph.position.x;
        }

        if let Some((mut background_origin, background_color)) = current_background.take() {
            if last_line_end_x == background_origin.x {
                background_origin.x -= max_glyph_size.width.half()
            };
            window.paint_quad(fill(
                Bounds {
                    origin: background_origin,
                    size: size(last_line_end_x - background_origin.x, line_height),
                },
                background_color,
            ));
        }

        Ok(())
    })
}

fn aligned_origin_x(
    origin: Point<Pixels>,
    align_width: Pixels,
    last_glyph_x: Pixels,
    align: &TextAlign,
    layout: &LineLayout,
    wrap_boundary: Option<&&WrapBoundary>,
) -> Pixels {
    let end_of_line = if let Some(WrapBoundary { run_ix, glyph_ix }) = wrap_boundary {
        layout.runs[*run_ix].glyphs[*glyph_ix].position.x
    } else {
        layout.width
    };

    let line_width = end_of_line - last_glyph_x;

    match align {
        TextAlign::Left => origin.x,
        TextAlign::Center => (origin.x * 2.0 + align_width - line_width) / 2.0,
        TextAlign::Right => origin.x + align_width - line_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AvailableSpace, Background, FontId, GlyphId, PaintOperation, Primitive, Scene, ShapedGlyph,
        ShapedRun, TestAppContext, TextRun, canvas, font, rgb,
    };

    fn paint_in_window(
        cx: &mut TestAppContext,
        paint: impl FnOnce(&mut Window, &mut App) + 'static,
    ) {
        cx.add_empty_window().draw(
            Point::default(),
            size(
                AvailableSpace::Definite(px(500.)),
                AvailableSpace::Definite(px(400.)),
            ),
            move |_, _| canvas(|_, _, _| (), move |_, (), window, cx| paint(window, cx)),
        );
    }

    fn run(len: usize, background_color: Option<Hsla>) -> TextRun {
        TextRun {
            len,
            font: font(".SystemUIFont"),
            color: rgb(0x112233).into(),
            background_color,
            underline: None,
            strikethrough: None,
        }
    }

    fn synthetic_line(font_id: FontId) -> LineLayout {
        LineLayout {
            font_size: px(16.),
            width: px(10.),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![ShapedRun {
                font_id,
                glyphs: vec![ShapedGlyph {
                    id: GlyphId(1),
                    position: Point::default(),
                    index: 0,
                    is_emoji: false,
                }],
            }],
            len: 1,
        }
    }

    fn decoration(background_color: Option<Hsla>) -> DecorationRun {
        DecorationRun {
            len: 1,
            color: black(),
            background_color,
            underline: None,
            strikethrough: None,
        }
    }

    fn operation_kinds(scene: &Scene) -> Vec<&'static str> {
        scene
            .paint_operations
            .iter()
            .map(|operation| match operation {
                PaintOperation::StartLayer(_) => "start",
                PaintOperation::EndLayer => "end",
                PaintOperation::Primitive(Primitive::Quad(_)) => "quad",
                PaintOperation::Primitive(Primitive::Underline(_)) => "underline",
                PaintOperation::Primitive(_) => "other",
            })
            .collect()
    }

    fn assert_bounds_close(
        actual: Bounds<crate::ScaledPixels>,
        expected: Bounds<crate::ScaledPixels>,
    ) {
        for (actual, expected) in [
            (actual.origin.x.0, expected.origin.x.0),
            (actual.origin.y.0, expected.origin.y.0),
            (actual.size.width.0, expected.size.width.0),
            (actual.size.height.0, expected.size.height.0),
        ] {
            assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
        }
    }

    #[crate::test]
    fn background_without_colors_skips_font_metrics_but_preserves_layers(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            for (index, decorations) in [vec![], vec![decoration(None)]].into_iter().enumerate() {
                // The test text system accepts synthetic IDs. Its metrics cache exposes
                // whether background painting unnecessarily consulted the font backend.
                let font_id = FontId(500_000 + index);
                assert!(!cx.text_system().font_metrics.read().contains_key(&font_id));
                let operations = window.next_frame.scene.len();
                let quads = window.next_frame.scene.quads.len();
                paint_line_background(
                    point(px(40.), px(40.)),
                    &synthetic_line(font_id),
                    px(24.),
                    TextAlign::Left,
                    None,
                    &decorations,
                    &[],
                    window,
                    cx,
                )
                .unwrap();
                assert!(
                    !cx.text_system().font_metrics.read().contains_key(&font_id),
                    "a line without background colors must not request font metrics"
                );
                assert_eq!(window.next_frame.scene.quads.len(), quads);
                assert_eq!(window.next_frame.scene.len(), operations + 2);
                assert!(matches!(
                    window.next_frame.scene.paint_operations[operations],
                    PaintOperation::StartLayer(_)
                ));
                assert!(matches!(
                    window.next_frame.scene.paint_operations[operations + 1],
                    PaintOperation::EndLayer
                ));
            }
        });
    }

    #[crate::test]
    fn colored_and_transparent_backgrounds_still_measure_and_paint(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            for (index, color) in [rgb(0x44aa66).into(), Hsla::transparent_black()]
                .into_iter()
                .enumerate()
            {
                let font_id = FontId(600_000 + index);
                let start = window.next_frame.scene.quads.len();
                paint_line_background(
                    point(px(40.), px(40.)),
                    &synthetic_line(font_id),
                    px(24.),
                    TextAlign::Left,
                    None,
                    &[decoration(Some(color))],
                    &[],
                    window,
                    cx,
                )
                .unwrap();
                assert!(cx.text_system().font_metrics.read().contains_key(&font_id));
                let painted = &window.next_frame.scene.quads[start..];
                assert_eq!(painted.len(), 1);
                assert_eq!(painted[0].background, Background::from(color));
                assert_bounds_close(
                    painted[0].bounds,
                    Bounds::new(point(px(40.), px(40.)), size(px(10.), px(24.)))
                        .scale(window.scale_factor()),
                );
            }
        });
    }

    #[crate::test]
    fn mixed_background_runs_keep_exact_colored_ranges(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            let red: Hsla = rgb(0xaa3344).into();
            let blue: Hsla = rgb(0x3355aa).into();
            let transparent = Hsla::transparent_black();
            let line = window.text_system().shape_line(
                "abCDxyEFzz".into(),
                px(16.),
                &[
                    run(2, None),
                    run(2, Some(red)),
                    run(2, None),
                    run(2, Some(transparent)),
                    run(2, Some(blue)),
                ],
                None,
            );
            let start = window.next_frame.scene.quads.len();
            let origin = point(px(40.), px(40.));
            line.paint_background(origin, px(24.), window, cx).unwrap();
            let quads = &window.next_frame.scene.quads[start..];
            assert_eq!(quads.len(), 3);
            for (quad, (range, color)) in
                quads
                    .iter()
                    .zip([(2..4, red), (6..8, transparent), (8..10, blue)])
            {
                assert_eq!(quad.background, Background::from(color));
                assert_bounds_close(
                    quad.bounds,
                    Bounds::new(
                        origin + point(line.x_for_index(range.start), px(0.)),
                        size(
                            line.x_for_index(range.end) - line.x_for_index(range.start),
                            px(24.),
                        ),
                    )
                    .scale(window.scale_factor()),
                );
            }
        });
    }

    #[crate::test]
    fn wrapped_unicode_backgrounds_keep_each_line_segment(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            let text = "office e\u{301} \u{1f600} \u{4e2d}\u{6587} marker";
            let color: Hsla = rgb(0x338855).into();
            let lines = window
                .text_system()
                .shape_text(
                    text.into(),
                    px(16.),
                    &[run(text.len(), Some(color))],
                    Some(px(54.)),
                    None,
                )
                .unwrap();
            assert_eq!(lines.len(), 1);
            let line = &lines[0];
            assert!(!line.wrap_boundaries.is_empty());
            assert_eq!(line.text.as_ref(), text);
            let mut ends: Vec<_> = line
                .wrap_boundaries
                .iter()
                .map(|boundary| {
                    line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix]
                        .position
                        .x
                })
                .collect();
            ends.push(line.unwrapped_layout.width);
            let start = window.next_frame.scene.quads.len();
            let origin = point(px(40.), px(40.));
            line.paint_background(origin, px(24.), TextAlign::Left, None, window, cx)
                .unwrap();
            let quads = &window.next_frame.scene.quads[start..];
            assert_eq!(quads.len(), ends.len());
            let mut previous_end = px(0.);
            for (index, (quad, end)) in quads.iter().zip(ends).enumerate() {
                assert_eq!(quad.background, Background::from(color));
                assert_bounds_close(
                    quad.bounds,
                    Bounds::new(
                        origin + point(px(0.), px(24.) * index as f32),
                        size(end - previous_end, px(24.)),
                    )
                    .scale(window.scale_factor()),
                );
                previous_end = end;
            }
        });
    }

    #[crate::test]
    fn plain_background_preserves_foreground_underline_and_strikethrough(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            let mut text_run = run(5, None);
            text_run.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(rgb(0xaa3344).into()),
                wavy: false,
            });
            text_run.strikethrough = Some(StrikethroughStyle {
                thickness: px(2.),
                color: Some(rgb(0x3355aa).into()),
            });
            let font_size = px(19.123);
            let line =
                window
                    .text_system()
                    .shape_line("plain".into(), font_size, &[text_run], None);
            let origin = point(px(40.), px(40.));
            let before = window.next_frame.scene.underlines.len();
            line.paint_background(origin, px(28.), window, cx).unwrap();
            assert_eq!(window.next_frame.scene.underlines.len(), before);
            line.paint(origin, px(28.), window, cx).unwrap();
            let underlines = &window.next_frame.scene.underlines[before..];
            assert_eq!(underlines.len(), 2);
            assert!(
                underlines
                    .iter()
                    .all(|underline| underline.bounds.size.width
                        > px(0.).scale(window.scale_factor()))
            );
            assert!(
                cx.text_system()
                    .raster_bounds
                    .read()
                    .keys()
                    .any(|params| params.font_size == font_size)
            );
        });
    }

    #[crate::test]
    fn empty_background_layers_preserve_overlapping_order_and_scene_replay(
        cx: &mut TestAppContext,
    ) {
        paint_in_window(cx, |window, cx| {
            let red: Hsla = rgb(0xaa3344).into();
            let green: Hsla = rgb(0x338855).into();
            let blue: Hsla = rgb(0x3355aa).into();
            let bounds = Bounds::new(point(px(40.), px(40.)), size(px(100.), px(48.)));
            window.paint_layer(bounds, |window| window.paint_quad(fill(bounds, red)));
            let line =
                window
                    .text_system()
                    .shape_line("layer".into(), px(16.), &[run(5, None)], None);
            let start = window.next_frame.scene.len();
            line.paint_background(bounds.origin, px(24.), window, cx)
                .unwrap();
            assert_eq!(window.next_frame.scene.len(), start + 2);
            let mut highlighted_run = run(5, Some(green));
            highlighted_run.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(green),
                wavy: false,
            });
            let highlighted =
                window
                    .text_system()
                    .shape_line("layer".into(), px(16.), &[highlighted_run], None);
            highlighted
                .paint_background(bounds.origin, px(24.), window, cx)
                .unwrap();
            highlighted
                .paint(bounds.origin, px(24.), window, cx)
                .unwrap();
            window.paint_layer(bounds, |window| window.paint_quad(fill(bounds, blue)));
            let source = &window.next_frame.scene;
            let quads = &source.quads[source.quads.len() - 3..];
            assert!(quads[0].order < quads[1].order && quads[1].order < quads[2].order);
            assert_eq!(quads[0].background, Background::from(red));
            assert_eq!(quads[1].background, Background::from(green));
            assert_eq!(quads[2].background, Background::from(blue));
            let foreground = source.underlines.last().unwrap();
            assert!(quads[1].order < foreground.order && foreground.order < quads[2].order);
            let mut replay = Scene::default();
            replay.replay(0..source.len(), source);
            replay.finish();
            assert_eq!(operation_kinds(&replay), operation_kinds(source));
            assert_eq!(replay.quads.len(), source.quads.len());
            for (actual, expected) in replay.quads.iter().zip(&source.quads) {
                assert_eq!(
                    (actual.order, actual.bounds, actual.background),
                    (expected.order, expected.bounds, expected.background)
                );
            }
            assert_eq!(replay.underlines.len(), source.underlines.len());
            for (actual, expected) in replay.underlines.iter().zip(&source.underlines) {
                assert_eq!(
                    (actual.order, actual.bounds, actual.color),
                    (expected.order, expected.bounds, expected.color)
                );
            }
            let last_order = replay.quads.last().unwrap().order;
            replay.insert_primitive(quads[2].clone());
            assert!(
                replay.quads.last().unwrap().order > last_order,
                "replaying every layer must leave the stack balanced"
            );
        });
    }

    // Uses HVC-GPUI-008's private paint_in_window/synthetic_line/decoration helpers.
    fn vertical_font_height(cx: &App, font_id: FontId, height: f32) {
        let text_system = cx.text_system();
        let mut metrics = text_system.read_metrics(font_id, |metrics| *metrics);
        // The synthetic layouts below use font_size = 16px.
        metrics.bounding_box.size.height = height * metrics.units_per_em as f32 / 16.0;
        text_system.font_metrics.write().insert(font_id, metrics);
    }

    fn vertical_visits(
        geometry: (Point<Pixels>, Pixels),
        layout: &LineLayout,
        decorations: &[DecorationRun],
        wraps: &[WrapBoundary],
        mask: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> usize {
        HVC009_GLYPH_VISITS.with(|count| count.set(0));
        window.with_content_mask(Some(crate::ContentMask { bounds: mask }), |window| {
            paint_line(
                geometry.0,
                layout,
                geometry.1,
                TextAlign::Left,
                None,
                decorations,
                wraps,
                window,
                cx,
            )
            .unwrap();
        });
        HVC009_GLYPH_VISITS.with(|count| count.get())
    }

    #[crate::test]
    fn vertical_culling_skips_plain_visits_preserving_empty_layers_and_replay(
        cx: &mut TestAppContext,
    ) {
        paint_in_window(cx, |window, cx| {
            let font_id = FontId(710_000);
            vertical_font_height(cx, font_id, 8.0);
            let layout = synthetic_line(font_id);
            let mask = Bounds::new(point(px(0.0), px(100.0)), size(px(500.0), px(10.0)));
            // line_bounds spans 40..140, but every glyph guard spans only 40..48.
            let colored_bounds = Bounds::new(point(px(40.0), px(100.0)), size(px(10.0), px(10.0)));
            window.with_content_mask(Some(crate::ContentMask { bounds: mask }), |window| {
                window.paint_layer(colored_bounds, |window| {
                    window.paint_quad(fill(colored_bounds, rgb(0xaa3344)))
                });
            });
            let start = window.next_frame.scene.len();
            assert_eq!(
                vertical_visits(
                    (point(px(40.0), px(40.0)), px(100.0)),
                    &layout,
                    &[decoration(None)],
                    &[],
                    mask,
                    window,
                    cx
                ),
                0
            );
            assert_eq!(
                window.next_frame.scene.len(),
                start + 2,
                "empty layer must survive the skip"
            );
            assert!(matches!(
                window.next_frame.scene.paint_operations[start],
                PaintOperation::StartLayer(_)
            ));
            assert!(matches!(
                window.next_frame.scene.paint_operations[start + 1],
                PaintOperation::EndLayer
            ));
            window.with_content_mask(Some(crate::ContentMask { bounds: mask }), |window| {
                window.paint_layer(colored_bounds, |window| {
                    window.paint_quad(fill(colored_bounds, rgb(0x3355aa)))
                });
            });
            let source = &window.next_frame.scene;
            let mut replay = Scene::default();
            replay.replay(0..source.len(), source);
            replay.finish();
            assert_eq!(operation_kinds(&replay), operation_kinds(source));
            assert_eq!(replay.quads.len(), source.quads.len());
            for (actual, expected) in replay.quads.iter().zip(&source.quads) {
                assert_eq!(
                    (actual.order, actual.bounds, actual.background),
                    (expected.order, expected.bounds, expected.background)
                );
            }
            assert!(
                source.quads[source.quads.len() - 2].order < source.quads.last().unwrap().order
            );
            let last_order = replay.quads.last().unwrap().order;
            replay.insert_primitive(source.quads.last().unwrap().clone());
            assert!(
                replay.quads.last().unwrap().order > last_order,
                "layer stack must remain balanced"
            );
        });
    }

    #[crate::test]
    fn vertical_culling_preserves_overhang_and_subpixel_boundary_intersections(
        cx: &mut TestAppContext,
    ) {
        paint_in_window(cx, |window, cx| {
            let font_id = FontId(710_001);
            vertical_font_height(cx, font_id, 40.0);
            let layout = synthetic_line(font_id);
            let mask = Bounds::new(point(px(0.0), px(100.0)), size(px(500.0), px(20.0)));
            // A line_bounds-only rejection incorrectly discards the first two cases.
            for (origin_y, expected_visits) in [
                (80.0, 1),
                (60.25, 1),
                (60.0, 0),
                (119.75, 1),
                (120.0, 0),
                (450.0, 0),
            ] {
                assert_eq!(
                    vertical_visits(
                        (point(px(40.0), px(origin_y)), px(8.0)),
                        &layout,
                        &[decoration(None)],
                        &[],
                        mask,
                        window,
                        cx
                    ),
                    expected_visits,
                    "origin {origin_y}"
                );
            }
        });
    }

    #[crate::test]
    fn vertical_culling_keeps_partial_wrapped_emoji_and_the_tallest_font(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            let short = FontId(710_002);
            let tall = FontId(710_003);
            vertical_font_height(cx, short, 8.0);
            vertical_font_height(cx, tall, 40.0);
            let mut layout = synthetic_line(short);
            layout.width = px(24.0);
            layout.len = 5;
            layout.runs.push(ShapedRun {
                font_id: tall,
                glyphs: vec![ShapedGlyph {
                    id: GlyphId(2),
                    position: point(px(12.0), px(0.0)),
                    index: 1,
                    is_emoji: true,
                }],
            });
            let wraps = [WrapBoundary {
                run_ix: 1,
                glyph_ix: 0,
            }];
            let mut decorated = decoration(None);
            decorated.len = 5;
            let partial = Bounds::new(point(px(0.0), px(100.0)), size(px(500.0), px(10.0)));
            // Row zero: 80..88; tall emoji row: 95..135. The second row remains eligible.
            assert_eq!(
                vertical_visits(
                    (point(px(40.0), px(80.0)), px(15.0)),
                    &layout,
                    &[decorated.clone()],
                    &wraps,
                    partial,
                    window,
                    cx
                ),
                2
            );
            let outside = Bounds::new(point(px(0.0), px(200.0)), size(px(500.0), px(10.0)));
            assert_eq!(
                vertical_visits(
                    (point(px(40.0), px(80.0)), px(15.0)),
                    &layout,
                    &[decorated],
                    &wraps,
                    outside,
                    window,
                    cx
                ),
                0
            );
        });
    }

    #[crate::test]
    fn vertical_culling_never_skips_any_some_underline_or_strikethrough(cx: &mut TestAppContext) {
        paint_in_window(cx, |window, cx| {
            let font_id = FontId(710_004);
            vertical_font_height(cx, font_id, 8.0);
            let layout = synthetic_line(font_id);
            let mask = Bounds::new(point(px(0.0), px(100.0)), size(px(500.0), px(20.0)));
            let mut underline = decoration(None);
            underline.underline = Some(UnderlineStyle {
                thickness: px(0.0),
                color: Some(Hsla::transparent_black()),
                wavy: false,
            });
            let mut strike = decoration(None);
            strike.strikethrough = Some(StrikethroughStyle {
                thickness: px(0.0),
                color: Some(Hsla::transparent_black()),
            });
            let mut zero_len = underline.clone();
            zero_len.len = 0;
            for decorations in [
                vec![underline],
                vec![strike],
                vec![zero_len, decoration(None)],
            ] {
                assert_eq!(
                    vertical_visits(
                        (point(px(40.0), px(450.0)), px(24.0)),
                        &layout,
                        &decorations,
                        &[],
                        mask,
                        window,
                        cx
                    ),
                    1
                );
            }
        });
    }

    #[crate::test]
    fn vertical_envelope_falls_back_for_non_finite_or_invalid_heights(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let id = FontId(710_005);
            vertical_font_height(cx, id, 8.0);
            let layout = synthetic_line(id);
            let mask = Bounds::new(point(px(0.0), px(100.0)), size(px(500.0), px(20.0)));
            for origin in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert!(!glyphs_outside_vertical_mask(
                    px(origin),
                    &layout,
                    px(24.0),
                    0,
                    &mask,
                    cx.text_system()
                ));
            }
            for height in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert!(!glyphs_outside_vertical_mask(
                    px(450.0),
                    &layout,
                    px(height),
                    1,
                    &mask,
                    cx.text_system()
                ));
            }
            for (mask_top, mask_height) in
                [(f32::NAN, 20.0), (100.0, f32::INFINITY), (100.0, -20.0)]
            {
                let invalid_mask = Bounds::new(
                    point(px(0.0), px(mask_top)),
                    size(px(500.0), px(mask_height)),
                );
                assert!(!glyphs_outside_vertical_mask(
                    px(450.0),
                    &layout,
                    px(24.0),
                    0,
                    &invalid_mask,
                    cx.text_system()
                ));
            }
            for font_height in [f32::NAN, f32::INFINITY, -8.0] {
                vertical_font_height(cx, id, font_height);
                assert!(!glyphs_outside_vertical_mask(
                    px(450.0),
                    &layout,
                    px(24.0),
                    0,
                    &mask,
                    cx.text_system()
                ));
            }
            vertical_font_height(cx, id, 8.0);
            assert!(!glyphs_outside_vertical_mask(
                px(f32::MAX),
                &layout,
                px(f32::MAX),
                2,
                &mask,
                cx.text_system()
            ));
        });
    }

    #[crate::test]
    fn vertical_envelope_never_culls_a_row_accepted_by_the_old_glyph_guard(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            let id = FontId(710_006);
            let layout = synthetic_line(id);
            for font_height in [0.0,0.125,8.0,40.0] {
                vertical_font_height(cx,id,font_height);
                let actual_height = cx.text_system().bounding_box(id,layout.font_size).size.height;
                for origin in [-500.0,-40.0,60.0,60.25,80.0,120.0,450.0,10_000_000.0] {
                    for line_height in [-0.25,0.0,0.1,0.125,0.6,15.0,24.0] {
                        for wraps in [0,1,3,32] {
                            for (mask_top,mask_height) in [(100.0,20.0),(10_000_000.0,4.0),(10_000_028.0,4.0)] {
                                let mask = Bounds::new(point(px(0.0),px(mask_top)),size(px(500.0),px(mask_height)));
                                if glyphs_outside_vertical_mask(px(origin),&layout,px(line_height),wraps,&mask,cx.text_system()) {
                                    let mut row_y = px(origin);
                                    for _ in 0..=wraps {
                                        assert!(!(row_y<mask.bottom() && row_y+actual_height>mask.top()),"origin={origin}, line={line_height}, wraps={wraps}, font={font_height}, mask={mask_top}");
                                        row_y+=px(line_height);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}
