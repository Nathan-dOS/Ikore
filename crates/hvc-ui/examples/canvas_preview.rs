//! Disposable native Canvas walkthrough. Uses no personal database or model requests.
//! Run with `cargo run --locked --offline -p hvc-ui --example canvas_preview`.
//! Add -- --elements 100 or -- --elements 500 for deterministic mixed scenes.
//! Example: `cargo run --locked --offline -p hvc-ui --example canvas_preview -- --elements 500`
//! The caller may set `HVC_BENCHMARK_UI_REPORT` to use the existing native callback probe.
//! Arguments and fixture construction never modify the process environment.
#![allow(
    clippy::unwrap_used,
    reason = "Visual fixture fails immediately on invalid setup"
)]

use hvc_core::companion_drawings::{
    ArrowBindingV1, CanvasElementKindV1 as Kind, CanvasElementV1 as Element, CanvasSceneV1,
    CanvasSemanticColor as Color, CanvasStrokeStyle, CanvasStrokeWidth, CanvasViewportV1,
    MAX_ELEMENTS, MAX_ZOOM, MIN_ZOOM, geometry, normalize_scene,
};
use hvc_core::persistence_service::{EnsureProjectForWorkspaceRequest, SaveUiSessionRequest};
use hvc_test_support::CoreTestHarness;

fn main() {
    let requested = match parse_elements(std::env::args().skip(1)) {
        Ok(requested) => requested,
        Err(error) => {
            eprintln!("Canvas preview: {error}\nUsage: canvas_preview [--elements 1..1000]");
            std::process::exit(2);
        }
    };
    let scene = requested.map_or_else(default_scene, |count| mixed_scene(count).unwrap());
    let viewport = requested.map_or(
        CanvasViewportV1 {
            pan_x: 10.0,
            pan_y: 40.0,
            zoom: 0.7,
        },
        |_| fit_viewport(&scene),
    );
    report_fixture(&scene, &viewport, requested.is_some());
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let workspace = std::fs::canonicalize(harness.workspace_dir())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let project = core
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.clone(),
            preferred_name: Some("Canvas isolated walkthrough".into()),
        })
        .unwrap()
        .project;
    let service = core.drawings();
    let drawing = service.create(&project.id, "System architecture").unwrap();
    service
        .save(
            &project.id,
            &drawing.id,
            drawing.revision,
            &drawing.title,
            &scene,
            &viewport,
        )
        .unwrap();
    service.sync_project(&project.id).unwrap();
    core.persistence()
        .save_ui_session(SaveUiSessionRequest {
            project_id: project.id,
            workspace_root: workspace,
            active_tab_path: None,
            workspace_view_mode: "editor".into(),
            active_view: "canvas".into(),
            sidebar_collapsed: true,
            context_drawer_open: false,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: None,
            tabs: Vec::new(),
            auto_open_review: false,
        })
        .unwrap();
    let handle = core.runtime_handle().clone();
    let _entered = handle.enter();
    gpui::Application::new()
        .with_assets(hvc_ui::HvcAssets)
        .run(move |cx| hvc_ui::open_main_window(core, cx).unwrap());
    drop(harness);
}

fn default_scene() -> CanvasSceneV1 {
    let mut ui = Element::new(Kind::Rectangle { round: true }, 60., 100., 170., 100.);
    ui.label = "Native UI\nGPUI".into();
    ui.stroke_color = Color::Blue;
    let mut engine = Element::new(Kind::Rectangle { round: true }, 350., 100., 170., 100.);
    engine.label = "Domain core\nRust".into();
    engine.stroke_color = Color::Green;
    let mut db = Element::new(Kind::Ellipse, 650., 100., 160., 100.);
    db.label = "SQLite".into();
    db.stroke_color = Color::Amber;
    let arrow = |from: &Element, to: &Element| {
        Element::new(
            Kind::Arrow {
                points: [[0., 0.], [120., 0.]],
                bidirectional: false,
                start_binding: Some(ArrowBindingV1 {
                    element_id: from.id.clone(),
                    anchor: [1., 0.],
                    gap: 5.,
                }),
                end_binding: Some(ArrowBindingV1 {
                    element_id: to.id.clone(),
                    anchor: [-1., 0.],
                    gap: 5.,
                }),
            },
            from.x + from.width,
            from.y + from.height / 2.,
            120.,
            0.,
        )
    };
    let mut arrows = vec![arrow(&ui, &engine), arrow(&engine, &db)];
    arrows[0].label = "typed calls".into();
    arrows[1].label = "save".into();
    let mut boundary = Element::new(Kind::Rectangle { round: false }, 20., 35., 830., 240.);
    boundary.label = String::new();
    boundary.stroke_style = CanvasStrokeStyle::Dashed;
    boundary.stroke_color = Color::Blue;
    boundary.z_index = 99;
    let mut title = Element::new(Kind::Text { font_size: 22. }, 50., 45., 500., 30.);
    title.label = "Architecture · local and native".into();
    let mut elements = vec![ui, engine, db, boundary, title];
    elements.extend(arrows);
    CanvasSceneV1 {
        elements,
        ..Default::default()
    }
}

fn parse_elements(arguments: impl IntoIterator<Item = String>) -> Result<Option<usize>, String> {
    let mut arguments = arguments.into_iter();
    let mut count = None;
    while let Some(argument) = arguments.next() {
        if argument != "--elements" {
            return Err("unsupported argument; only --elements is accepted".into());
        }
        if count.is_some() {
            return Err("--elements may be supplied only once".into());
        }
        let value = arguments.next().ok_or("--elements requires a count")?;
        let parsed = value
            .parse::<usize>()
            .map_err(|_| "element count must be an integer")?;
        if !(1..=MAX_ELEMENTS).contains(&parsed) {
            return Err(format!(
                "element count must be between 1 and {MAX_ELEMENTS}"
            ));
        }
        count = Some(parsed);
    }
    Ok(count)
}

fn fixture_element(index: usize, shape: Kind, x: f32, y: f32, width: f32, height: f32) -> Element {
    let mut element = Element::new(shape, x, y, width, height);
    element.id = format!("fixture-{index:04}");
    element.z_index = index as i32;
    element.stroke_color = Color::ALL[index % Color::ALL.len()];
    element.stroke_width = match index % 3 {
        0 => CanvasStrokeWidth::Thin,
        1 => CanvasStrokeWidth::Medium,
        _ => CanvasStrokeWidth::Bold,
    };
    element
}

/// Nine-element cells: four nodes, three bound arrows, a boundary and a title.
/// A partial final cell only references nodes already included in the scene.
fn mixed_scene(count: usize) -> Result<CanvasSceneV1, String> {
    if !(1..=MAX_ELEMENTS).contains(&count) {
        return Err("fixture size is outside the supported range".into());
    }
    let groups = count.div_ceil(9);
    let columns = ((groups as f32).sqrt().ceil() as usize).clamp(1, 8);
    let mut elements = Vec::with_capacity(count);
    for group in 0..groups {
        let x = (group % columns) as f32 * 820.0;
        let y = (group / columns) as f32 * 280.0;
        let base = group * 9;
        let mut cell = Vec::with_capacity(9);
        for node in 0..4 {
            let shape = match node {
                1 => Kind::Diamond,
                2 => Kind::Ellipse,
                _ => Kind::Rectangle { round: node == 0 },
            };
            let mut element = fixture_element(
                base + node,
                shape,
                x + 40.0 + node as f32 * 185.0,
                y + 100.0,
                130.0,
                80.0,
            );
            element.label = format!(
                "Node {:03}\n{}",
                group * 4 + node + 1,
                ["Native UI", "Decision", "Storage", "Worker"][node]
            );
            element.fill_enabled = (group + node).is_multiple_of(2);
            cell.push(element);
        }
        for edge in 0..3 {
            let from = &cell[edge];
            let to = &cell[edge + 1];
            let start = [from.x + from.width, from.y + from.height / 2.0];
            let end = [to.x, to.y + to.height / 2.0];
            let mut arrow = fixture_element(
                base + 4 + edge,
                Kind::Arrow {
                    points: [[0.0, 0.0], [end[0] - start[0], end[1] - start[1]]],
                    bidirectional: edge == 2 && group.is_multiple_of(2),
                    start_binding: Some(ArrowBindingV1 {
                        element_id: from.id.clone(),
                        anchor: [1.0, 0.0],
                        gap: 0.0,
                    }),
                    end_binding: Some(ArrowBindingV1 {
                        element_id: to.id.clone(),
                        anchor: [-1.0, 0.0],
                        gap: 0.0,
                    }),
                },
                start[0],
                start[1],
                end[0] - start[0],
                end[1] - start[1],
            );
            arrow.label = ["call", "event", "sync"][edge].into();
            arrow.stroke_style = [
                CanvasStrokeStyle::Solid,
                CanvasStrokeStyle::Dashed,
                CanvasStrokeStyle::Dotted,
            ][edge];
            cell.push(arrow);
        }
        let mut boundary = fixture_element(
            base + 7,
            Kind::Rectangle { round: false },
            x + 10.0,
            y + 40.0,
            770.0,
            190.0,
        );
        boundary.stroke_style = CanvasStrokeStyle::Dashed;
        // Deliberately created later: the containment painter must still keep it behind children.
        boundary.z_index = 50_000 + group as i32;
        cell.push(boundary);
        let mut title = fixture_element(
            base + 8,
            Kind::Text { font_size: 18.0 },
            x + 28.0,
            y + 50.0,
            300.0,
            24.0,
        );
        title.label = format!("Boundary {:02} · native fixture", group + 1);
        cell.push(title);
        elements.extend(cell.into_iter().take(count - elements.len()));
    }
    normalize_scene(&CanvasSceneV1 {
        elements,
        ..Default::default()
    })
    .map_err(|error| error.to_string())
}

/// Fit the synthetic scene into an 800×500 logical-pixel drawing region.
/// The regular Canvas Fit action can recompute against the actual resized panel.
fn fit_viewport(scene: &CanvasSceneV1) -> CanvasViewportV1 {
    let bounds = geometry::scene_bounds(scene).expanded(24.0);
    let zoom = (800.0 / bounds.width)
        .min(500.0 / bounds.height)
        .clamp(MIN_ZOOM, MAX_ZOOM)
        .min(1.0);
    let centre = bounds.centre();
    CanvasViewportV1 {
        pan_x: 400.0 - centre[0] * zoom,
        pan_y: 250.0 - centre[1] * zoom,
        zoom,
    }
}

fn report_fixture(scene: &CanvasSceneV1, viewport: &CanvasViewportV1, deterministic: bool) {
    let boundaries = scene
        .elements
        .iter()
        .filter(|element| {
            matches!(element.shape, Kind::Rectangle { .. })
                && element.stroke_style == CanvasStrokeStyle::Dashed
        })
        .count();
    let arrows = scene
        .elements
        .iter()
        .filter(|element| matches!(element.shape, Kind::Arrow { .. }))
        .count();
    let texts = scene
        .elements
        .iter()
        .filter(|element| matches!(element.shape, Kind::Text { .. }))
        .count();
    let labels = scene
        .elements
        .iter()
        .filter(|element| !element.label.is_empty())
        .count();
    let shapes = scene.elements.len() - boundaries - arrows - texts;
    println!(
        "Canvas fixture: elements={} shapes={shapes} arrows={arrows} boundaries={boundaries} texts={texts} labels={labels} deterministic={deterministic} zoom={:.3} pan=({:.1},{:.1})",
        scene.elements.len(),
        viewport.zoom,
        viewport.pan_x,
        viewport.pan_y
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Option<usize>, String> {
        parse_elements(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn fixture_arguments_preserve_default_and_bound_requested_counts() {
        assert_eq!(parse(&[]), Ok(None));
        for count in [1, 100, 500, 1_000] {
            assert_eq!(parse(&["--elements", &count.to_string()]), Ok(Some(count)));
        }
        for arguments in [
            vec!["--elements"],
            vec!["--elements", "0"],
            vec!["--elements", "1001"],
            vec!["--elements", "invalid"],
            vec!["--elements", "-1"],
            vec!["--elements", "100", "--elements", "500"],
            vec!["--unknown"],
        ] {
            assert!(
                parse(&arguments).is_err(),
                "unsupported fixture arguments are rejected"
            );
        }
        assert_eq!(default_scene().elements.len(), 7);
    }

    #[test]
    fn fixture_generation_is_exact_deterministic_mixed_and_fits() {
        for count in [1, 7, 9, 100, 500, 1_000] {
            let scene = mixed_scene(count).expect("valid mixed scene");
            assert_eq!(scene.elements.len(), count);
            assert_eq!(scene, mixed_scene(count).expect("identical scene"));
            let viewport = fit_viewport(&scene);
            hvc_core::companion_drawings::validate_viewport(&viewport)
                .expect("valid fitted camera");
            let bounds = geometry::scene_bounds(&scene);
            let top_left = geometry::world_to_screen(&viewport, [bounds.x, bounds.y]);
            let bottom_right = geometry::world_to_screen(
                &viewport,
                [bounds.x + bounds.width, bounds.y + bounds.height],
            );
            assert!(top_left[0] >= 0.0 && top_left[1] >= 0.0);
            assert!(bottom_right[0] <= 800.0 && bottom_right[1] <= 500.0);
            if count >= 9 {
                assert!(
                    scene
                        .elements
                        .iter()
                        .any(|element| matches!(element.shape, Kind::Diamond))
                );
                assert!(
                    scene
                        .elements
                        .iter()
                        .any(|element| matches!(element.shape, Kind::Ellipse))
                );
                assert!(
                    scene
                        .elements
                        .iter()
                        .any(|element| matches!(element.shape, Kind::Arrow { .. }))
                );
                assert!(
                    scene
                        .elements
                        .iter()
                        .any(|element| element.container_id.is_some())
                );
                assert!(
                    scene
                        .elements
                        .iter()
                        .any(|element| matches!(element.shape, Kind::Text { .. }))
                );
            }
        }
        assert!(mixed_scene(0).is_err());
        assert!(mixed_scene(MAX_ELEMENTS + 1).is_err());
    }
}
