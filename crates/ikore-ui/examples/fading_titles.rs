#![allow(
    clippy::unwrap_used,
    reason = "Manual visual fixtures fail immediately when isolated setup is invalid"
)]
//! Isolated native visual fixture: `cargo run -p ikore-ui --example fading_titles`.
//! Uses no application database, project, credentials or provider connections.
#[path = "../src/fading_title.rs"]
mod fading_title;
#[allow(dead_code)]
#[path = "../src/theme.rs"]
mod theme;

use fading_title::FadingTitle;
use gpui::{
    App, Application, Bounds, Context, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};

struct Fixture;

impl Render for Fixture {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("fixture")
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .min_w_0()
            .overflow_y_scroll()
            .bg(theme::surface())
            .text_color(theme::text())
            .font_family(theme::FONT_UI)
            .text_size(px(12.0))
            .line_height(px(18.0))
            .p_3()
            .flex()
            .flex_col()
            .gap_4()
            .child("Single-line titles — native GPUI visual fixture")
            .child(div().w_full().min_w_0().p_3().bg(theme::surface_raised())
                .child(FadingTitle::new("Resize the window: this full-width title fades only when it exceeds the available width.", theme::surface_raised())))
            .children([180.0, 292.0, 420.0].into_iter().map(|width| {
                div().w_full().min_w_0().flex().gap_4().children([false, true].into_iter().map(move |selected| {
                    let background = if selected { theme::accent_soft() } else { theme::surface_raised() };
                    div().w(px(width)).min_w_0().max_w_full().p_3().rounded_md().bg(background)
                        .flex().flex_col().gap_2()
                        .child(format!("{} px · {}", width, if selected { "Selected" } else { "Normal" }))
                        .child(FadingTitle::new("Short title", background))
                        .child(FadingTitle::new("Review architecture decisions and implement all remaining integration scenarios", background))
                        .child(FadingTitle::new("LongUnbrokenTitle".repeat(12), background))
                        .child(FadingTitle::new("Café 👩‍💻 e\u{301}\nUnicode and explicit line breaks remain on one visual line", background))
                }))
            }))
            .child(div().w_full().min_w_0().p_3().bg(theme::surface_raised())
                .child(FadingTitle::new("This full-width title responds to resizing the window and fades only once its shaped line exceeds the available width.", theme::surface_raised())))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(980.0), px(760.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("HVC Fading Titles QA".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    cx.observe_window_bounds(window, |_, _, cx| cx.notify())
                        .detach();
                    Fixture
                })
            },
        )
        .unwrap();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
