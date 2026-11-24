//! The canvas's pure parts (feature 25, M1): layout and physics.

use super::constants::{
    BOX_PADDING, FREE_FORM_LIMIT, GHOST_KEY, LOOSE_COLUMNS, NODE_HEIGHT, NODE_WIDTH, REST_OFFSET,
    STAGE_GAP,
};
use super::layout::{Direction, LayoutInput, NodeKind, layout};
use super::physics::{BodySpec, Physics, Vec2};

fn stages() -> Vec<String> {
    [
        "intake_router",
        "task_specifier",
        "contract_agent",
        "backend_agent",
        "test_agent",
        "integrator",
    ]
    .into_iter()
    .map(|role| format!("role:{role}"))
    .collect()
}

fn input(free_forms: usize, archived: usize) -> LayoutInput {
    let mut loose = [
        "feature_agent",
        "visualizer",
        "learning_agent",
        "notes_scribe",
        "documentation_reconciler",
    ]
    .into_iter()
    .map(|role| format!("role:{role}"))
    .collect::<Vec<_>>();
    loose.extend((0..free_forms).map(|index| format!("slot:free-{index}")));
    LayoutInput {
        stages: stages(),
        loose,
        ghost: free_forms < FREE_FORM_LIMIT,
        archived,
        pipelines: Vec::new(),
    }
}

/// Feature 25, M2: each pipeline is a box of its own below the default one,
/// its steps on a row linked left to right, and the arrows reach them.
#[test]
fn pipelines_get_a_box_each_below_the_default_one() {
    use super::layout::PipelineBoxInput;
    let mut with_pipelines = input(2, 0);
    with_pipelines.pipelines = vec![
        PipelineBoxInput {
            id: "p1".into(),
            steps: vec![
                "pipeline:p1/a".into(),
                "pipeline:p1/b".into(),
                "pipeline:p1/c".into(),
            ],
        },
        PipelineBoxInput {
            id: "p2".into(),
            steps: vec!["pipeline:p2/only".into()],
        },
    ];
    let canvas = layout(&with_pipelines);
    assert_eq!(canvas.boxes.len(), 3);
    let (default, first, second) = (&canvas.boxes[0], &canvas.boxes[1], &canvas.boxes[2]);
    assert!(!default.pipeline && first.pipeline && second.pipeline);
    assert_eq!((first.id.as_str(), second.id.as_str()), ("p1", "p2"));
    assert!(first.rect.y > default.rect.bottom());
    assert!(second.rect.y > first.rect.bottom());
    let steps = canvas
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == NodeKind::Step)
        .collect::<Vec<_>>();
    assert_eq!(steps.len(), 4);
    for (_, node) in &steps {
        let frame = &canvas.boxes[node.box_index];
        assert!(
            frame.rect.contains(node.home),
            "{} inside its box",
            node.key
        );
    }
    // Links stay inside a box: a -> b -> c, and none to the second box.
    let index = |key: &str| canvas.index_of(key).unwrap();
    let links = canvas
        .edges
        .iter()
        .map(|edge| (edge.from, edge.to))
        .collect::<Vec<_>>();
    assert!(links.contains(&(index("pipeline:p1/a"), index("pipeline:p1/b"))));
    assert!(links.contains(&(index("pipeline:p1/b"), index("pipeline:p1/c"))));
    assert!(
        !links
            .iter()
            .any(|(from, to)| *from == index("pipeline:p1/c") || *to == index("pipeline:p2/only"))
    );
    // The arrows walk from the last row of the default box into the boxes.
    let a = index("pipeline:p1/a");
    assert_eq!(
        canvas.neighbour(a, Direction::Right),
        Some(index("pipeline:p1/b"))
    );
    assert_eq!(
        canvas.neighbour(a, Direction::Down),
        Some(index("pipeline:p2/only"))
    );
    assert!(canvas.neighbour(a, Direction::Up).is_some());
    assert!(canvas.bounds().unwrap().contains(second.rect));
}

#[test]
fn the_same_agents_always_get_the_same_positions() {
    assert_eq!(layout(&input(3, 1)), layout(&input(3, 1)));
}

#[test]
fn the_six_stages_sit_on_one_row_with_five_links() {
    let canvas = layout(&input(0, 0));
    let stages = canvas
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Stage)
        .collect::<Vec<_>>();
    assert_eq!(stages.len(), 6);
    for (index, stage) in stages.iter().enumerate() {
        assert_eq!(
            stage.home.x,
            BOX_PADDING + index as f32 * (NODE_WIDTH + STAGE_GAP)
        );
        assert_eq!(stage.home.y, stages[0].home.y);
        assert_eq!(stage.row, 0);
    }
    assert_eq!(canvas.edges.len(), 5);
    for (index, edge) in canvas.edges.iter().enumerate() {
        assert_eq!((edge.from, edge.to), (index, index + 1));
    }
}

#[test]
fn loose_nodes_follow_support_then_free_form_then_the_ghost_in_six_columns() {
    let canvas = layout(&input(3, 0));
    let loose = canvas
        .nodes
        .iter()
        .filter(|node| node.kind != NodeKind::Stage)
        .collect::<Vec<_>>();
    let keys = loose
        .iter()
        .map(|node| node.key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(keys[0], "role:feature_agent");
    assert_eq!(keys[4], "role:documentation_reconciler");
    assert_eq!(&keys[5..8], ["slot:free-0", "slot:free-1", "slot:free-2"]);
    assert_eq!(keys[8], GHOST_KEY);
    assert_eq!(loose[8].kind, NodeKind::Ghost);
    // Six to a row, left to right; the seventh starts the next row.
    assert_eq!(loose[5].column, 5);
    assert_eq!(loose[6].column, 0);
    assert_eq!(loose[6].row, loose[0].row + 1);
    assert!(loose[6].home.y > loose[0].home.y);
    assert!(
        loose
            .iter()
            .all(|node| node.home.y > canvas.nodes[0].home.bottom())
    );
    assert!(loose.iter().all(|node| node.column < LOOSE_COLUMNS));
}

#[test]
fn a_role_in_neither_list_goes_to_the_loose_area() {
    let mut agents = input(0, 0);
    agents.loose.insert(5, "role:a_newer_role".into());
    let canvas = layout(&agents);
    let node = &canvas.nodes[canvas.index_of("role:a_newer_role").unwrap()];
    assert_eq!(node.kind, NodeKind::Loose);
    assert!(
        canvas
            .edges
            .iter()
            .all(|edge| canvas.nodes[edge.from].kind == NodeKind::Stage)
    );
}

#[test]
fn the_ghost_shows_only_below_the_free_form_limit() {
    assert!(
        layout(&input(FREE_FORM_LIMIT - 1, 0))
            .index_of(GHOST_KEY)
            .is_some()
    );
    assert!(
        layout(&input(FREE_FORM_LIMIT, 0))
            .index_of(GHOST_KEY)
            .is_none()
    );
}

#[test]
fn the_box_holds_every_node_and_the_archived_footer() {
    for free_forms in [0, 3, FREE_FORM_LIMIT] {
        let canvas = layout(&input(free_forms, 2));
        let frame = canvas.boxes[0].rect;
        assert!(canvas.nodes.iter().all(|node| frame.contains(node.home)));
        let footer = canvas.boxes[0].footer.unwrap();
        assert!(frame.contains(footer));
        assert!(
            canvas
                .nodes
                .iter()
                .all(|node| footer.y >= node.home.bottom())
        );
        assert_eq!(canvas.bounds(), Some(frame));
    }
    assert!(layout(&input(1, 0)).boxes[0].footer.is_none());
}

#[test]
fn arrows_walk_the_row_and_jump_between_rows() {
    let canvas = layout(&input(3, 0));
    let code = canvas.index_of("role:backend_agent").unwrap();
    let contract = canvas.index_of("role:contract_agent").unwrap();
    let test = canvas.index_of("role:test_agent").unwrap();
    assert_eq!(canvas.neighbour(code, Direction::Left), Some(contract));
    assert_eq!(canvas.neighbour(code, Direction::Right), Some(test));
    assert_eq!(canvas.neighbour(0, Direction::Left), None);
    assert_eq!(canvas.neighbour(code, Direction::Up), None);
    let below = canvas.neighbour(code, Direction::Down).unwrap();
    assert_eq!(canvas.nodes[below].row, 1);
    assert_eq!(canvas.nodes[below].column, 3);
    assert_eq!(canvas.neighbour(below, Direction::Up), Some(code));
    // The ghost ends the last row; the arrows never land on it.
    let last = canvas.index_of("slot:free-2").unwrap();
    assert_eq!(canvas.neighbour(last, Direction::Right), None);
}

/// Six linked stages and four loose nodes in box 0; two nodes in box 1.
fn physics() -> Physics {
    let mut bodies = (0..6)
        .map(|index| BodySpec {
            home: Vec2::new(94.0 + index as f32 * (NODE_WIDTH + STAGE_GAP), 78.0),
            group: 0,
        })
        .collect::<Vec<_>>();
    bodies.extend((0..4).map(|index| BodySpec {
        home: Vec2::new(94.0 + index as f32 * (NODE_WIDTH + 14.0), 170.0),
        group: 0,
    }));
    bodies.extend((0..2).map(|index| BodySpec {
        home: Vec2::new(
            94.0 + index as f32 * (NODE_WIDTH + 14.0),
            400.0 + NODE_HEIGHT,
        ),
        group: 1,
    }));
    let links = (1..6).map(|to| (to - 1, to)).collect::<Vec<_>>();
    Physics::new(&bodies, &links)
}

fn run(physics: &mut Physics, seconds: f32) {
    for _ in 0..(seconds * 60.0).round() as usize {
        physics.step(1.0 / 60.0);
    }
}

#[test]
fn a_node_pushed_100_px_away_is_home_within_two_seconds() {
    let mut physics = physics();
    physics.displace(3, Vec2::new(100.0, 0.0));
    assert!(!physics.is_at_rest());
    run(&mut physics, 2.0);
    for index in 0..physics.len() {
        assert!(physics.offset(index).length() < REST_OFFSET, "node {index}");
    }
}

#[test]
fn irregular_frames_never_produce_nan_or_infinity() {
    let mut physics = physics();
    let mut seed: u32 = 7;
    let mut next = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32
    };
    for step in 0..10_000 {
        match step % 400 {
            0 => {
                physics.begin_drag(step / 400 % 10, Vec2::new(100.0, 80.0), false);
            }
            1..=150 => physics.drag_to(Vec2::new(100.0 + 600.0 * next(), 80.0 + 300.0 * next())),
            151 => physics.end_drag(),
            _ => {}
        }
        physics.step(0.05 * next());
        for index in 0..physics.len() {
            assert!(physics.position(index).is_finite(), "step {step}");
            assert!(physics.velocity(index).is_finite(), "step {step}");
        }
    }
}

#[test]
fn dragging_a_stage_moves_its_neighbours_more_than_theirs_and_no_other_box() {
    let mut physics = physics();
    let grab = physics.position(2);
    assert!(physics.begin_drag(2, grab, false));
    let mut most = [0.0_f32; 12];
    for frame in 0..60 {
        let reach = (frame as f32 / 30.0).min(1.0) * 90.0;
        physics.drag_to(grab + Vec2::new(0.0, reach));
        physics.step(1.0 / 60.0);
        for (index, value) in most.iter_mut().enumerate() {
            *value = value.max(physics.offset(index).length());
        }
    }
    assert!(most[1] > most[0], "{most:?}");
    assert!(most[3] > most[4], "{most:?}");
    assert!(most[4] > most[5], "{most:?}");
    assert_eq!(most[10], 0.0);
    assert_eq!(most[11], 0.0);
}

#[test]
fn after_the_release_the_energy_never_grows() {
    let mut physics = physics();
    let grab = physics.position(3);
    physics.begin_drag(3, grab, false);
    for frame in 0..40 {
        physics.drag_to(grab + Vec2::new(frame as f32 * 6.0, frame as f32 * 3.0));
        physics.step(1.0 / 60.0);
    }
    physics.end_drag();
    while physics.ring_in_flight() {
        physics.step(1.0 / 60.0);
    }
    let mut previous = physics.energy();
    for _ in 0..12 {
        run(&mut physics, 0.25);
        let energy = physics.energy();
        assert!(energy <= previous * 1.05, "{energy} after {previous}");
        previous = energy;
    }
}

#[test]
fn everything_is_back_within_two_seconds_of_a_release() {
    let mut physics = physics();
    let grab = physics.position(8);
    physics.begin_drag(8, grab, false);
    for frame in 0..30 {
        physics.drag_to(grab + Vec2::new(frame as f32 * 9.0, 0.0));
        physics.step(1.0 / 60.0);
    }
    physics.end_drag();
    run(&mut physics, 2.0);
    assert!(physics.is_at_rest());
}

#[test]
fn at_rest_a_step_changes_nothing() {
    let mut physics = physics();
    assert!(physics.is_at_rest());
    let before = physics.clone();
    physics.step(0.5);
    assert_eq!(physics, before);
    physics.displace(0, Vec2::new(20.0, 0.0));
    run(&mut physics, 3.0);
    assert!(physics.is_at_rest());
    let settled = physics.clone();
    physics.step(1.0 / 60.0);
    assert_eq!(physics, settled);
    assert!(!physics.is_moving(0));
}

#[test]
fn reduced_motion_never_starts_a_drag() {
    let mut physics = physics();
    assert!(!physics.begin_drag(1, Vec2::new(10.0, 10.0), true));
    assert_eq!(physics.dragging(), None);
    physics.drag_to(Vec2::new(500.0, 500.0));
    assert!(physics.is_at_rest());
}

#[test]
fn a_moving_node_stretches_along_its_motion() {
    let mut physics = physics();
    assert_eq!(physics.stretch(7).0, 1.0);
    physics.displace(7, Vec2::new(120.0, 0.0));
    physics.step(1.0 / 30.0);
    let (factor, direction) = physics.stretch(7);
    assert!(factor > 1.0 && factor <= 1.16);
    assert!(direction.x < 0.0, "heading home, to the left");
}
