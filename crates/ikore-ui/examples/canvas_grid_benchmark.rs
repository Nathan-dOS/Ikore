//! Isolated native grid benchmark; opens no application database or project.
//!
//! `cargo run --release -p ikore-ui --example canvas_grid_benchmark -- --mode optimized --zoom 1 --report target/grid-optimized.json`
//! Repeat with `--mode legacy` / `--mode off`, keeping every other option equal.
//! Callback intervals are not compositor presentation times or input latency.
#![allow(
    clippy::unwrap_used,
    reason = "Disposable benchmark setup and test assertions"
)]

#[path = "../src/canvas/grid.rs"]
mod grid;

use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Instant};

use gpui::{
    Application, Bounds, Context, Pixels, Render, Window, WindowBounds, WindowOptions, canvas, div,
    fill, point, prelude::*, px, rgb, size,
};
use ikore_core::companion_drawings::{CanvasBackgroundMode, CanvasViewportV1};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Legacy,
    Optimized,
    Off,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Optimized => "optimized",
            Self::Off => "off",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Config {
    mode: Mode,
    zoom: f32,
    width: u32,
    height: u32,
    samples: usize,
    warmup: usize,
    animated_pan: bool,
    report: PathBuf,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Config, String> {
    let mut config = Config {
        mode: Mode::Optimized,
        zoom: 1.,
        width: 1280,
        height: 820,
        samples: 400,
        warmup: 100,
        animated_pan: true,
        report: PathBuf::new(),
    };
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("Missing value for {flag}"))?;
        match flag.as_str() {
            "--mode" => {
                config.mode = match value.as_str() {
                    "legacy" => Mode::Legacy,
                    "optimized" => Mode::Optimized,
                    "off" => Mode::Off,
                    _ => return Err("--mode must be legacy, optimized or off".into()),
                };
            }
            "--zoom" => {
                config.zoom = value.parse().map_err(|_| "Invalid --zoom")?;
                if !config.zoom.is_finite() || !(0.1..=4.).contains(&config.zoom) {
                    return Err("--zoom must be finite and within 0.1..4".into());
                }
            }
            "--width" => {
                config.width = value.parse().map_err(|_| "Invalid --width")?;
                if !(320..=7680).contains(&config.width) {
                    return Err("--width must be within 320..7680 logical pixels".into());
                }
            }
            "--height" => {
                config.height = value.parse().map_err(|_| "Invalid --height")?;
                if !(240..=4320).contains(&config.height) {
                    return Err("--height must be within 240..4320 logical pixels".into());
                }
            }
            "--samples" => {
                config.samples = value.parse().map_err(|_| "Invalid --samples")?;
                if !(20..=2000).contains(&config.samples) {
                    return Err("--samples must be within 20..2000".into());
                }
            }
            "--warmup" => {
                config.warmup = value.parse().map_err(|_| "Invalid --warmup")?;
                if !(1..=2000).contains(&config.warmup) {
                    return Err("--warmup must be within 1..2000".into());
                }
            }
            "--pan" => {
                config.animated_pan = match value.as_str() {
                    "animated" => true,
                    "static" => false,
                    _ => return Err("--pan must be animated or static".into()),
                };
            }
            "--report" => {
                if value.is_empty() {
                    return Err("--report cannot be empty".into());
                }
                config.report = value.into();
            }
            _ => return Err(format!("Unknown option {flag}")),
        }
    }
    if config.report.as_os_str().is_empty() {
        return Err("--report is required".into());
    }
    Ok(config)
}

#[derive(Default)]
struct Measurements {
    painted_frames: usize,
    last_callback: Option<Instant>,
    callback_ms: Vec<f64>,
    insertion_ms: Vec<f64>,
    dots: Vec<usize>,
    canvas_size: [f32; 2],
    scale_factor: f32,
}

struct Fixture {
    config: Config,
    measurements: Rc<RefCell<Measurements>>,
}

impl Render for Fixture {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.measurements.borrow().painted_frames;
        if frame == self.config.warmup + self.config.samples {
            let report = report(&self.config, &self.measurements.borrow());
            let path = self.config.report.clone();
            // Report I/O and app shutdown run after this render, outside paint.
            cx.defer(move |cx| {
                if let Err(error) = write_json(&path, &report) {
                    eprintln!("Grid benchmark report failed: {error}");
                    std::process::exit(1);
                }
                println!("Grid benchmark completed: {}", path.display());
                cx.quit();
            });
            return div().size_full().bg(rgb(0x06080b)).into_any_element();
        }
        {
            let mut measurements = self.measurements.borrow_mut();
            let now = Instant::now();
            if let Some(previous) = measurements.last_callback.replace(now)
                && frame >= self.config.warmup
            {
                measurements
                    .callback_ms
                    .push((now - previous).as_secs_f64() * 1000.);
            }
            measurements.scale_factor = window.scale_factor();
        }
        window.request_animation_frame();
        let view = viewport(&self.config, frame);
        let mode = self.config.mode;
        let warmup = self.config.warmup;
        let measurements = self.measurements.clone();
        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(0x06080b))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        let started = Instant::now();
                        match mode {
                            Mode::Legacy => paint_legacy(bounds, &view, window),
                            Mode::Optimized => {
                                grid::paint_grid(bounds, &view, CanvasBackgroundMode::Dark, window);
                            }
                            Mode::Off => {}
                        }
                        let insertion_ms = started.elapsed().as_secs_f64() * 1000.;
                        let mut measurements = measurements.borrow_mut();
                        if measurements.painted_frames >= warmup {
                            measurements.insertion_ms.push(insertion_ms);
                            measurements.dots.push(expected_dot_count(
                                bounds.size.width.into(),
                                bounds.size.height.into(),
                                &view,
                                mode,
                            ));
                        }
                        measurements.canvas_size =
                            [bounds.size.width.into(), bounds.size.height.into()];
                        measurements.painted_frames += 1;
                    },
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}

fn viewport(config: &Config, frame: usize) -> CanvasViewportV1 {
    let offset = if config.animated_pan {
        frame as f32 * 0.7
    } else {
        0.
    };
    CanvasViewportV1 {
        pan_x: -17. + offset,
        pan_y: 35. + offset * 0.5,
        zoom: config.zoom,
    }
}

/// The original production block, before batching/density changes.
fn paint_legacy(bounds: Bounds<Pixels>, view: &CanvasViewportV1, window: &mut Window) {
    let mut spacing = 24. * view.zoom;
    while spacing < 12. {
        spacing *= 2.;
    }
    let start_x = view.pan_x.rem_euclid(spacing);
    let start_y = view.pan_y.rem_euclid(spacing);
    let color = rgb(0x1d2937);
    let mut y = start_y;
    while y < f32::from(bounds.size.height) {
        let mut x = start_x;
        while x < f32::from(bounds.size.width) {
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left() + px(x), bounds.top() + px(y)),
                    size(px(1.), px(1.)),
                ),
                color,
            ));
            x += spacing;
        }
        y += spacing;
    }
}

// Calculated from density rules after the timed section, not a GPU draw counter.
fn expected_dot_count(width: f32, height: f32, view: &CanvasViewportV1, mode: Mode) -> usize {
    if mode == Mode::Off {
        return 0;
    }
    let mut spacing = 24. * view.zoom;
    let minimum = if mode == Mode::Legacy { 12. } else { 24. };
    loop {
        let columns = ((width - view.pan_x.rem_euclid(spacing)) / spacing)
            .ceil()
            .max(0.) as usize;
        let rows = ((height - view.pan_y.rem_euclid(spacing)) / spacing)
            .ceil()
            .max(0.) as usize;
        let dots = columns * rows;
        if spacing >= minimum && (mode == Mode::Legacy || dots <= 4096) {
            return dots;
        }
        spacing *= 2.;
    }
}

fn summary(raw: &[f64]) -> Value {
    let mut sorted = raw.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = |quantile: f64| {
        let index = (quantile * sorted.len() as f64).ceil() as usize;
        sorted.get(index.saturating_sub(1)).copied()
    };
    json!({
        "samples": raw.len(),
        "medianMs": rank(0.5),
        "p95Ms": rank(0.95),
        "p99Ms": rank(0.99),
        "maxMs": sorted.last(),
        "rawMs": raw,
    })
}

fn report(config: &Config, measurements: &Measurements) -> Value {
    json!({
        "schemaVersion": 1,
        "status": "completed",
        "fixture": "canvas_grid_benchmark",
        "mode": config.mode.name(),
        "zoom": config.zoom,
        "pan": if config.animated_pan { "animated" } else { "static" },
        "panStepLogicalPixels": if config.animated_pan { [0.7, 0.35] } else { [0., 0.] },
        "requestedWindowLogicalPixels": [config.width, config.height],
        "canvasLogicalPixels": measurements.canvas_size,
        "scaleFactor": measurements.scale_factor,
        "warmupFrames": config.warmup,
        "paintedFrames": measurements.painted_frames,
        "callbackInterval": {
            "metric": "gpui_animation_frame_callback_interval_ms",
            "scope": "Intervals between this fixture's render callbacks; not compositor presentation, FPS, pan input latency, save latency or full Canvas scene performance",
            "statistics": summary(&measurements.callback_ms),
        },
        "gridInsertionCpu": {
            "metric": "grid_paint_callback_insertion_elapsed_ms",
            "scope": "Instant elapsed around grid quad/layer insertion only; excludes scene finish, GPU and compositor; off includes timer/match overhead",
            "statistics": summary(&measurements.insertion_ms),
        },
        "expectedDots": {
            "method": "Calculated from spacing/count rules after timing; not an instrumented GPU primitive counter",
            "min": measurements.dots.iter().min(),
            "max": measurements.dots.iter().max(),
            "raw": measurements.dots,
        },
    })
}

fn write_json(path: &std::path::Path, report: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(report)?)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!(
                "{error}\nUsage: canvas_grid_benchmark --mode legacy|optimized|off --zoom 0.1..4 --width 1280 --height 820 --samples 400 --warmup 100 --pan animated|static --report PATH"
            );
            std::process::exit(2);
        }
    };
    write_json(
        &config.report,
        &json!({ "status": "configured", "mode": config.mode.name() }),
    )?;
    Application::new().run(move |cx| {
        let bounds = Bounds::centered(
            None,
            size(px(config.width as f32), px(config.height as f32)),
            cx,
        );
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some(
                        format!(
                            "Ikore Grid benchmark · {} · zoom {}",
                            config.mode.name(),
                            config.zoom
                        )
                        .into(),
                    ),
                    ..Default::default()
                }),
                focus: true,
                ..Default::default()
            },
            |window, cx| {
                window.activate_window();
                cx.new(|_| Fixture {
                    config,
                    measurements: Rc::new(RefCell::new(Measurements::default())),
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Config, String> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn benchmark_arguments_keep_matching_scene_controls() {
        let config = parse(&[
            "--mode",
            "legacy",
            "--zoom",
            "0.5",
            "--pan",
            "static",
            "--samples",
            "20",
            "--warmup",
            "1",
            "--width",
            "1280",
            "--height",
            "820",
            "--report",
            "target/grid.json",
        ])
        .unwrap();
        assert_eq!(config.mode, Mode::Legacy);
        assert_eq!(config.zoom, 0.5);
        assert!(!config.animated_pan);
        assert_eq!((config.samples, config.warmup), (20, 1));
        assert_eq!((config.width, config.height), (1280, 820));
        assert_eq!(viewport(&config, 0), viewport(&config, 99));
        let animated = Config {
            animated_pan: true,
            ..config
        };
        assert_eq!(viewport(&animated, 10).pan_x, -10.);
    }

    #[test]
    fn benchmark_arguments_reject_invalid_and_unbounded_inputs() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["--mode"]).is_err());
        for (flag, value) in [
            ("--mode", "other"),
            ("--pan", "other"),
            ("--zoom", "NaN"),
            ("--zoom", "inf"),
            ("--zoom", "0"),
            ("--zoom", "4.1"),
            ("--samples", "0"),
            ("--samples", "2001"),
            ("--warmup", "0"),
            ("--warmup", "2001"),
            ("--width", "0"),
            ("--width", "100000"),
            ("--height", "-1"),
            ("--height", "100000"),
            ("--unknown", "value"),
            ("--report", ""),
        ] {
            assert!(
                parse(&["--report", "target/grid.json", flag, value]).is_err(),
                "{flag} {value}"
            );
        }
    }

    #[test]
    fn benchmark_density_separates_batching_and_zoom_subsampling() {
        let view = CanvasViewportV1::default();
        assert_eq!(expected_dot_count(1280., 820., &view, Mode::Legacy), 1890);
        assert_eq!(
            expected_dot_count(1280., 820., &view, Mode::Optimized),
            1890
        );
        let zoomed = CanvasViewportV1 { zoom: 0.5, ..view };
        assert_eq!(expected_dot_count(1280., 820., &zoomed, Mode::Legacy), 7383);
        assert_eq!(
            expected_dot_count(1280., 820., &zoomed, Mode::Optimized),
            1890
        );
        assert_eq!(expected_dot_count(1280., 820., &zoomed, Mode::Off), 0);
        assert_eq!(summary(&[4., 1., 3., 2.])["p95Ms"], 4.);
    }
}
