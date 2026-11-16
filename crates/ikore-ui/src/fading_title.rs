//! A single shaped line clipped to its allocated width, with an overflow-only fade.
use gpui::{
    App, Bounds, ContentMask, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, ShapedLine, SharedString, Style, Window, fill,
    linear_color_stop, linear_gradient, point, px, relative, size,
};

pub(crate) struct FadingTitle {
    text: SharedString,
    background: Hsla,
}

impl FadingTitle {
    pub(crate) fn new(text: impl Into<SharedString>, background: impl Into<Hsla>) -> Self {
        let text = text.into();
        // Presentation only: persisted titles and detail headings retain the full value.
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ").into();
        Self {
            text,
            background: background.into(),
        }
    }
}

pub(crate) struct TitlePaintState {
    line: ShapedLine,
    line_height: Pixels,
    bounds: Bounds<Pixels>,
    fade: Option<Bounds<Pixels>>,
}

impl IntoElement for FadingTitle {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for FadingTitle {
    type RequestLayoutState = ();
    type PrepaintState = TitlePaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = window.line_height().into();
        style.min_size.width = px(0.0).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> TitlePaintState {
        let style = window.text_style();
        let line = window.text_system().shape_line(
            self.text.clone(),
            style.font_size.to_pixels(window.rem_size()),
            &[style.to_run(self.text.len())],
            None,
        );
        let fade = (line.width > bounds.size.width && bounds.size.width > px(0.0)).then(|| {
            let width = px(24.0).min(bounds.size.width);
            Bounds::new(
                point(bounds.right() - width, bounds.top()),
                size(width, bounds.size.height),
            )
        });
        TitlePaintState {
            line,
            line_height: window.line_height(),
            bounds,
            fade,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        state: &mut TitlePaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(
            Some(ContentMask {
                bounds: state.bounds,
            }),
            |window| {
                let _ = state
                    .line
                    .paint(state.bounds.origin, state.line_height, window, cx);
                if let Some(fade) = state.fade {
                    window.paint_quad(fill(
                        fade,
                        linear_gradient(
                            90.0,
                            linear_color_stop(self.background.opacity(0.0), 0.0),
                            linear_color_stop(self.background, 1.0),
                        ),
                    ));
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, div};

    struct Harness;
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn title_shapes_unicode_on_one_line_and_updates_fade_with_width(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, _| Harness);
        for background in [crate::theme::surface_raised(), crate::theme::accent_soft()] {
            for text in [
                "A short title".to_string(),
                "LongUnbrokenTitle".repeat(30),
                "Caf\u{e9} 👩‍💻 e\u{301}\nSecond line\r\nThird line".to_string(),
            ] {
                let mut heights = Vec::new();
                for width in [12.0, 180.0, 420.0, 8000.0, 180.0] {
                    let (_, state) = visual.draw(
                        point(px(0.0), px(0.0)),
                        size(px(width), px(200.0)),
                        |_, _| FadingTitle::new(text.to_string(), background),
                    );
                    heights.push(state.bounds.size.height);
                    assert_eq!(state.bounds.size.height, state.line_height);
                    assert_eq!(state.fade.is_some(), state.line.width > px(width));
                    if let Some(fade) = state.fade {
                        assert_eq!(fade.right(), state.bounds.right());
                        assert_eq!(fade.size.width, px(24.0).min(px(width)));
                        assert!(fade.left() >= state.bounds.left());
                    }
                    if width == 8000.0 {
                        assert!(state.fade.is_none());
                    }
                }
                assert!(heights.windows(2).all(|pair| pair[0] == pair[1]));
            }
        }
    }
}
