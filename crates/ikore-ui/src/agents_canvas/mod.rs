//! The agents canvas (feature 25, M1; D5 to D10). Each pipeline is a box with
//! its agents: the Ikore pipeline's stages linked left to right, and the
//! agents linked to no one in its corner. A running node pulses softly; a
//! dragged one sways like a water bubble while its neighbours answer, and
//! the links never change ends. Clicking a node opens its synthesis and pins
//! the bottom dock on it.

mod constants;
mod layout;
mod paint;
mod physics;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnimationExt, AnyElement, Bounds, BoxShadow, Context, FocusHandle, FontWeight, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, Rgba,
    ScrollWheelEvent, SharedString, canvas, div, point, prelude::*, px,
};

use crate::agents::AgentIdentifier;
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::theme;

use constants::{
    BOX_TITLE_HEIGHT, DRAG_THRESHOLD, FIT_MARGIN, HALO_BLUR, HALO_OPACITY, NODE_HEIGHT,
    NODE_RADIUS, NODE_WIDTH, ZOOM_MAX, ZOOM_MIN,
};
pub(crate) use constants::{FREE_FORM_LIMIT, SYNTHESIS_MIN_WIDTH};
use layout::{CanvasLayout, Direction, LayoutInput, NodeKind, PipelineBoxInput, layout};
use physics::{BodySpec, Physics, Vec2};

/// How a node looks (the feature's table of states).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeState {
    Idle,
    Running,
    /// The pulsation waits on the developer at this stage's checkpoint.
    Waiting,
    /// Failed, awaiting a fallback, or with a pending fallback.
    Attention,
    Done,
}

/// What a node shows, built by the Agents page from the agents snapshot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CanvasNode {
    pub(crate) agent: AgentIdentifier,
    pub(crate) label: String,
    pub(crate) color: Rgba,
    pub(crate) state: NodeState,
    /// `Running 3:04 · 2 units`, `Waiting on you`, `Done · 2m ago`…
    pub(crate) status: String,
    /// `CLI · claude` or `API · {route}`.
    pub(crate) transport: String,
    /// A human checkpoint follows this stage.
    pub(crate) checkpoint: bool,
}

/// The agents to draw, in their order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CanvasAgents {
    /// The default box's title: `Pipeline · {pulsation} · {stage} · {status}`.
    pub(crate) title: String,
    /// The pipeline's stages, in their order.
    pub(crate) stages: Vec<CanvasNode>,
    /// Support agents, roles in neither list, then free-form agents.
    pub(crate) loose: Vec<CanvasNode>,
    /// `New freeform agent`: shown below the limit, usable when not busy.
    pub(crate) ghost: Option<bool>,
    pub(crate) archived: usize,
    /// The developer's pipelines, a box each (feature 25, M2).
    pub(crate) pipelines: Vec<CanvasPipeline>,
    /// The pipeline whose synthesis is open.
    pub(crate) selected_pipeline: Option<String>,
}

/// A pipeline's box: its title and its steps, in order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CanvasPipeline {
    pub(crate) id: String,
    /// `{name} · v{n} · {status}`.
    pub(crate) title: String,
    pub(crate) state: NodeState,
    pub(crate) steps: Vec<CanvasNode>,
}

/// A press on the canvas, until the button goes up.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Press {
    Node {
        index: usize,
        origin: Point<Pixels>,
        moved: bool,
        dragging: bool,
    },
    Background {
        origin: Point<Pixels>,
        pan: Vec2,
        moved: bool,
    },
}

#[derive(Debug)]
struct Scene {
    input: LayoutInput,
    layout: CanvasLayout,
    physics: Physics,
    /// Screen pixels from the canvas's corner to the canvas origin.
    pan: Vec2,
    zoom: f32,
    /// `Fit` ran once the canvas had a size.
    fitted: bool,
    bounds: Bounds<Pixels>,
    last_frame: Option<Instant>,
    press: Option<Press>,
    /// The node the arrow keys move, by key.
    cursor: Option<String>,
}

impl Scene {
    fn new() -> Self {
        Self {
            input: LayoutInput::default(),
            layout: CanvasLayout::default(),
            physics: Physics::new(&[], &[]),
            pan: Vec2::new(FIT_MARGIN, FIT_MARGIN),
            zoom: 1.0,
            fitted: false,
            bounds: Bounds::default(),
            last_frame: None,
            press: None,
            cursor: None,
        }
    }

    /// A new layout when the agents changed; the physics starts at rest.
    fn sync(&mut self, input: LayoutInput) {
        if input == self.input {
            return;
        }
        self.layout = layout(&input);
        let bodies = self
            .layout
            .nodes
            .iter()
            .map(|node| BodySpec {
                home: node.home.center(),
                group: node.box_index,
            })
            .collect::<Vec<_>>();
        let links = self
            .layout
            .edges
            .iter()
            .map(|edge| (edge.from, edge.to))
            .collect::<Vec<_>>();
        self.physics = Physics::new(&bodies, &links);
        self.input = input;
        self.press = None;
    }

    /// Advances the physics to `now`; a canvas at rest keeps no clock.
    fn advance(&mut self, now: Instant) {
        if self.physics.is_at_rest() {
            self.last_frame = None;
            return;
        }
        let elapsed = self.last_frame.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f32()
        });
        self.last_frame = Some(now);
        self.physics.step(elapsed);
    }

    fn to_world(&self, position: Point<Pixels>) -> Vec2 {
        let x = f32::from(position.x - self.bounds.origin.x);
        let y = f32::from(position.y - self.bounds.origin.y);
        Vec2::new((x - self.pan.x) / self.zoom, (y - self.pan.y) / self.zoom)
    }

    fn to_screen(&self, world: Vec2) -> Vec2 {
        Vec2::new(
            world.x * self.zoom + self.pan.x,
            world.y * self.zoom + self.pan.y,
        )
    }

    /// Frames every box, never closer than zoom 1 nor farther than the
    /// smallest zoom.
    fn fit(&mut self) {
        let width = f32::from(self.bounds.size.width);
        let height = f32::from(self.bounds.size.height);
        let Some(content) = self.layout.bounds() else {
            return;
        };
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let zoom = ((width - 2.0 * FIT_MARGIN) / content.width)
            .min((height - 2.0 * FIT_MARGIN) / content.height)
            .clamp(ZOOM_MIN, 1.0);
        self.zoom = zoom;
        self.pan = Vec2::new(
            ((width - content.width * zoom) / 2.0).max(FIT_MARGIN) - content.x * zoom,
            FIT_MARGIN - content.y * zoom,
        );
        self.fitted = true;
    }

    fn zoom_at(&mut self, factor: f32, around: Point<Pixels>) {
        // The developer frames the canvas from now on.
        self.fitted = true;
        let anchor = self.to_world(around);
        let zoom = (self.zoom * factor).clamp(ZOOM_MIN, ZOOM_MAX);
        let screen = Vec2::new(
            f32::from(around.x - self.bounds.origin.x),
            f32::from(around.y - self.bounds.origin.y),
        );
        self.zoom = zoom;
        self.pan = Vec2::new(screen.x - anchor.x * zoom, screen.y - anchor.y * zoom);
    }
}

/// The canvas's state, kept on `AppModel`. Pan and zoom live only as long
/// as the session, and positions are never saved (E4).
pub(crate) struct AgentsCanvas {
    scene: Rc<RefCell<Scene>>,
    pub(crate) focus: FocusHandle,
}

impl AgentsCanvas {
    pub(crate) fn new(focus: FocusHandle) -> Self {
        Self {
            scene: Rc::new(RefCell::new(Scene::new())),
            focus,
        }
    }

    /// Grabs a node and moves the grip, as a pointer would; the benchmark's
    /// scripted drag.
    pub(crate) fn script_drag(&self, key: &str, by: (f32, f32)) {
        let mut scene = self.scene.borrow_mut();
        let Some(index) = scene.layout.index_of(key) else {
            return;
        };
        let home = scene.layout.nodes[index].home.center();
        if scene.physics.dragging() != Some(index) {
            scene.physics.begin_drag(index, home, false);
        }
        scene.physics.drag_to(home + Vec2::new(by.0, by.1));
    }

    pub(crate) fn script_release(&self) {
        self.scene.borrow_mut().physics.end_drag();
    }
}

fn node_key(agent: &AgentIdentifier) -> String {
    crate::agent_dock::agent_key(agent)
}

fn state_color(state: NodeState) -> Rgba {
    match state {
        NodeState::Idle | NodeState::Done => theme::border_strong(),
        NodeState::Running => theme::accent(),
        NodeState::Waiting => theme::warning(),
        NodeState::Attention => theme::danger(),
    }
}

fn status_color(state: NodeState) -> Rgba {
    match state {
        NodeState::Idle | NodeState::Done => theme::text_muted(),
        NodeState::Running => theme::running(),
        NodeState::Waiting => theme::warning(),
        NodeState::Attention => theme::danger(),
    }
}

fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

/// The canvas: boxes, links, nodes. `selected` is the node with the
/// synthesis open.
pub(crate) fn agents_canvas(
    model: &AppModel,
    agents: CanvasAgents,
    selected: Option<&AgentIdentifier>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let reduced_motion = crate::motion::reduced_motion();
    let input = LayoutInput {
        stages: agents
            .stages
            .iter()
            .map(|node| node_key(&node.agent))
            .collect(),
        loose: agents
            .loose
            .iter()
            .map(|node| node_key(&node.agent))
            .collect(),
        ghost: agents.ghost.is_some(),
        archived: agents.archived,
        pipelines: agents
            .pipelines
            .iter()
            .map(|pipeline| PipelineBoxInput {
                id: pipeline.id.clone(),
                steps: pipeline
                    .steps
                    .iter()
                    .map(|node| node_key(&node.agent))
                    .collect(),
            })
            .collect(),
    };
    let scene_rc = model.agents_canvas.scene.clone();
    let (layout, zoom, pan, moving, positions, cursor) = {
        let mut scene = scene_rc.borrow_mut();
        scene.sync(input);
        if reduced_motion && scene.physics.dragging().is_some() {
            scene.physics.end_drag();
        }
        scene.advance(Instant::now());
        if !scene.fitted {
            scene.fit();
        }
        let positions = (0..scene.layout.nodes.len())
            .map(|index| scene.to_screen(scene.physics.position(index)))
            .collect::<Vec<_>>();
        let moving = (0..scene.layout.nodes.len())
            .map(|index| scene.physics.is_moving(index))
            .collect::<Vec<_>>();
        (
            scene.layout.clone(),
            scene.zoom,
            scene.pan,
            moving,
            positions,
            scene.cursor.clone(),
        )
    };
    let nodes = agents
        .stages
        .iter()
        .chain(&agents.loose)
        .chain(agents.pipelines.iter().flat_map(|pipeline| &pipeline.steps))
        .map(|node| (node_key(&node.agent), node.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let selected_key = selected.map(node_key);

    let mut surface = div()
        .id("agents-canvas")
        .debug_selector(|| "agents-canvas".into())
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .bg(theme::canvas())
        .track_focus(&model.agents_canvas.focus)
        .tab_index(0)
        .key_context("AgentsCanvas")
        .on_key_down(cx.listener(|model, event: &KeyDownEvent, _, cx| {
            if model.agents_canvas_key(event, cx) {
                cx.stop_propagation();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|model, event: &MouseDownEvent, window, cx| {
                window.focus(&model.agents_canvas.focus);
                model.press_agents_background(event.position, cx);
            }),
        )
        .on_mouse_move(cx.listener(|model, event: &MouseMoveEvent, _, cx| {
            model.move_agents_pointer(event.position, cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|model, _, _, cx| model.release_agents_pointer(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|model, _, _, cx| model.release_agents_pointer(cx)),
        )
        .on_scroll_wheel(cx.listener(|model, event: &ScrollWheelEvent, window, cx| {
            model.scroll_agents_canvas(event, window.line_height(), cx);
        }));

    // The boxes, under everything.
    for frame in &layout.boxes {
        let left = frame.rect.x * zoom + pan.x;
        let top = frame.rect.y * zoom + pan.y;
        if frame.pipeline {
            let Some(pipeline) = agents.pipelines.iter().find(|item| item.id == frame.id) else {
                continue;
            };
            surface = surface.child(pipeline_box(
                frame,
                pipeline,
                (left, top, zoom),
                agents.selected_pipeline.as_deref() == Some(frame.id.as_str()),
                cx,
            ));
            continue;
        }
        let box_id = frame.id.clone();
        let selector = format!("agents-box:{box_id}");
        surface = surface.child(
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(frame.rect.width * zoom))
                .h(px(frame.rect.height * zoom))
                .rounded(px(12.0 * zoom))
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .child(
                    div()
                        .h(px(BOX_TITLE_HEIGHT * zoom))
                        .px(px(12.0 * zoom))
                        .flex()
                        .items_center()
                        .gap(px(6.0 * zoom))
                        .border_b_1()
                        .border_color(theme::border())
                        .child(
                            div()
                                .id("agents-box-lock:default")
                                .flex_shrink_0()
                                .tooltip(|_, cx| {
                                    cx.new(|_| {
                                        crate::widgets::Tooltip::new(
                                            "Locked: stages, prompts and gates can't be edited",
                                        )
                                    })
                                    .into()
                                })
                                .child(crate::widgets::icon(
                                    Icon::Lock,
                                    px(12.0 * zoom),
                                    theme::text_muted(),
                                )),
                        )
                        .child(
                            div()
                                .debug_selector(|| "agents-box-title:default".into())
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.0 * zoom))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(agents.title.clone()),
                        ),
                ),
        );
        if let Some(footer) = frame.footer {
            let count = agents.archived;
            surface = surface.child(
                div()
                    .id("agents-box-archived")
                    .debug_selector(|| "agents-box-archived".into())
                    .absolute()
                    .left(px(footer.x * zoom + pan.x))
                    .top(px(footer.y * zoom + pan.y))
                    .h(px(footer.height * zoom))
                    .px(px(6.0 * zoom))
                    .flex()
                    .items_center()
                    .gap(px(4.0 * zoom))
                    .rounded(px(4.0 * zoom))
                    .text_size(px(11.0 * zoom))
                    .text_color(theme::text_muted())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::hover_bg()))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|model, _, window, cx| {
                        window.focus(&model.agents_canvas.focus);
                        model.open_archived_agents(cx);
                    }))
                    .child(crate::widgets::icon(
                        Icon::Archive,
                        px(11.0 * zoom),
                        theme::text_muted(),
                    ))
                    .child(format!("Archived · {count}")),
            );
        }
    }

    // Links and swaying outlines, painted between the boxes and the nodes.
    let links = layout
        .edges
        .iter()
        .map(|edge| (positions[edge.from], positions[edge.to]))
        .collect::<Vec<_>>();
    // The dragged node goes over everything else: its bubble on a layer of
    // its own above the other nodes, then its text.
    let dragged = scene_rc.borrow().physics.dragging();
    let (bubbles, dragged_bubble): (Vec<_>, Vec<_>) = {
        let scene = scene_rc.borrow();
        (0..layout.nodes.len())
            .filter(|index| moving[*index] && layout.nodes[*index].kind != NodeKind::Ghost)
            .map(|index| {
                (index, {
                    let state = nodes
                        .get(&layout.nodes[index].key)
                        .map_or(NodeState::Idle, |node| node.state);
                    paint::Bubble {
                        center: positions[index],
                        half: Vec2::new(NODE_WIDTH * zoom / 2.0, NODE_HEIGHT * zoom / 2.0),
                        stretch: scene.physics.stretch(index),
                        modes: scene.physics.modes(index),
                        zoom,
                        fill: theme::surface_raised(),
                        stroke: state_color(state),
                    }
                })
            })
            .partition(|(index, _)| Some(*index) != dragged)
    };
    let bubbles = bubbles
        .into_iter()
        .map(|(_, bubble)| bubble)
        .collect::<Vec<_>>();
    let dragged_bubble = dragged_bubble
        .into_iter()
        .map(|(_, bubble)| bubble)
        .collect::<Vec<_>>();
    let measured = scene_rc.clone();
    surface = surface.child(
        canvas(
            move |bounds, window, _| {
                let mut scene = measured.borrow_mut();
                let resized = scene.bounds.size != bounds.size;
                scene.bounds = bounds;
                if resized && !scene.fitted {
                    window.request_animation_frame();
                }
                if !scene.physics.is_at_rest() {
                    window.request_animation_frame();
                }
                (links, bubbles, zoom)
            },
            |bounds, (links, bubbles, zoom), window, _| {
                paint::links(bounds, &links, zoom, window);
                paint::bubbles(bounds, &bubbles, window);
            },
        )
        .absolute()
        .size_full(),
    );

    // The nodes, on top; the dragged one last, over its own bubble.
    let order = (0..layout.nodes.len())
        .filter(|index| Some(*index) != dragged)
        .chain(dragged)
        .collect::<Vec<_>>();
    for index in order {
        let node_layout = &layout.nodes[index];
        if Some(index) == dragged && !dragged_bubble.is_empty() {
            let bubble = dragged_bubble.clone();
            surface = surface.child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| paint::bubbles(bounds, &bubble, window),
                )
                .absolute()
                .size_full(),
            );
        }
        let center = positions[index];
        let left = center.x - NODE_WIDTH * zoom / 2.0;
        let top = center.y - NODE_HEIGHT * zoom / 2.0;
        if node_layout.kind == NodeKind::Ghost {
            surface = surface.child(ghost_node(left, top, zoom, agents.ghost == Some(true), cx));
            continue;
        }
        let Some(node) = nodes.get(&node_layout.key) else {
            continue;
        };
        let is_selected = selected_key.as_deref() == Some(node_layout.key.as_str());
        let is_cursor = cursor.as_deref() == Some(node_layout.key.as_str());
        surface = surface.child(node_element(
            index,
            node,
            NodePlacement {
                left,
                top,
                zoom,
                swaying: moving[index],
                selected: is_selected,
                cursor: is_cursor,
                reduced_motion,
            },
            cx,
        ));
    }
    surface.into_any_element()
}

/// A pipeline's box: its title opens the pipeline's synthesis (feature 25,
/// section 4); its steps are nodes like any agent.
fn pipeline_box(
    frame: &layout::BoxLayout,
    pipeline: &CanvasPipeline,
    (left, top, zoom): (f32, f32, f32),
    selected: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = pipeline.id.clone();
    let selector = format!("agents-box:{id}");
    let title_selector = format!("agents-box-title:{id}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(frame.rect.width * zoom))
        .h(px(frame.rect.height * zoom))
        .rounded(px(12.0 * zoom))
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(theme::surface())
        .child(
            div()
                .id(SharedString::from(title_selector.clone()))
                .debug_selector(move || title_selector.clone())
                .h(px(BOX_TITLE_HEIGHT * zoom))
                .px(px(12.0 * zoom))
                .flex()
                .items_center()
                .gap(px(6.0 * zoom))
                .border_b_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover_bg()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |model, _, window, cx| {
                    window.focus(&model.agents_canvas.focus);
                    model.select_canvas_pipeline(id.clone(), cx);
                }))
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(8.0 * zoom))
                        .rounded_full()
                        .bg(state_color(pipeline.state)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0 * zoom))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(pipeline.title.clone()),
                ),
        )
        .into_any_element()
}

#[derive(Debug, Clone, Copy)]
struct NodePlacement {
    left: f32,
    top: f32,
    zoom: f32,
    /// The bubble is painted under it: the node itself draws no shape.
    swaying: bool,
    selected: bool,
    cursor: bool,
    reduced_motion: bool,
}

fn node_element(
    index: usize,
    node: &CanvasNode,
    placement: NodePlacement,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let NodePlacement {
        left,
        top,
        zoom,
        swaying,
        selected,
        cursor,
        reduced_motion,
    } = placement;
    let key = node_key(&node.agent);
    let selector = format!("agents-node:{key}");
    let border = state_color(node.state);
    let running = node.state == NodeState::Running;
    let shape = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(NODE_WIDTH * zoom))
        .h(px(NODE_HEIGHT * zoom))
        .rounded(px(NODE_RADIUS * zoom))
        .px(px(9.0 * zoom))
        .py(px(7.0 * zoom))
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(3.0 * zoom))
        .cursor_pointer()
        .map(|shape| {
            if swaying {
                shape.border_1().border_color(gpui::transparent_black())
            } else if selected {
                shape
                    .border_2()
                    .border_color(theme::accent())
                    .bg(theme::surface_raised())
            } else {
                shape
                    .when(running && reduced_motion, |shape| shape.border_2())
                    .when(!(running && reduced_motion), |shape| shape.border_1())
                    .border_color(border)
                    .bg(if cursor {
                        theme::accent_soft()
                    } else {
                        theme::surface_raised()
                    })
            }
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |model, event: &MouseDownEvent, window, cx| {
                window.focus(&model.agents_canvas.focus);
                model.press_agents_node(index, event.position, cx);
                cx.stop_propagation();
            }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0 * zoom))
                .min_w_0()
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(8.0 * zoom))
                        .rounded_full()
                        .bg(node.color),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.0 * zoom))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(node.label.clone()),
                )
                .when(node.checkpoint, |row| {
                    let id = format!("agents-node-gate:{key}");
                    row.child(
                        div()
                            .id(SharedString::from(id.clone()))
                            .debug_selector(move || id.clone())
                            .flex_shrink_0()
                            .tooltip(|_, cx| {
                                cx.new(|_| crate::widgets::Tooltip::new("Human checkpoint"))
                                    .into()
                            })
                            .child(crate::widgets::icon(
                                Icon::Eye,
                                px(11.0 * zoom),
                                theme::text_muted(),
                            )),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0 * zoom))
                .min_w_0()
                .text_size(px(10.5 * zoom))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(status_color(node.state))
                        .child(node.status.clone()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(70.0 * zoom))
                        .truncate()
                        .text_color(theme::text_subtle())
                        .child(node.transport.clone()),
                ),
        );
    if running && !reduced_motion && !swaying && !selected {
        let accent = theme::accent();
        let id = format!("agents-node-pulse:{key}");
        return shape
            .with_animation(
                SharedString::from(id),
                crate::motion::soft_pulse(),
                move |shape, level| {
                    shape
                        .border_color(with_alpha(accent, level))
                        .shadow(vec![BoxShadow {
                            color: Hsla::from(with_alpha(accent, HALO_OPACITY * level)),
                            offset: point(px(0.0), px(0.0)),
                            blur_radius: px(HALO_BLUR * level * zoom),
                            spread_radius: px(0.0),
                        }])
                },
            )
            .into_any_element();
    }
    shape.into_any_element()
}

fn ghost_node(
    left: f32,
    top: f32,
    zoom: f32,
    enabled: bool,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    div()
        .id("agents-node-ghost:new-free-form")
        .debug_selector(|| "agents-node-ghost:new-free-form".into())
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(NODE_WIDTH * zoom))
        .h(px(NODE_HEIGHT * zoom))
        .rounded(px(NODE_RADIUS * zoom))
        .border_1()
        .border_color(theme::border())
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.0 * zoom))
        .text_size(px(12.0 * zoom))
        .text_color(if enabled {
            theme::text_muted()
        } else {
            theme::text_subtle()
        })
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when(enabled, |ghost| {
            ghost
                .cursor_pointer()
                .hover(|style| style.bg(theme::hover_bg()))
                .on_click(cx.listener(|model, _, _, cx| model.create_free_form_agent(cx)))
        })
        .child(crate::widgets::icon(
            Icon::Plus,
            px(12.0 * zoom),
            theme::text_muted(),
        ))
        .child("New freeform agent")
        .into_any_element()
}

impl AppModel {
    /// A node clicked, or chosen with `Enter`: its synthesis opens and the
    /// dock pins on it, opening if it was hidden.
    pub(crate) fn select_canvas_agent(&mut self, agent: AgentIdentifier, cx: &mut Context<Self>) {
        let same = self.agents.read(cx).selected_agent.as_ref() == Some(&agent);
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = Some(agent.clone());
            agents.selected_pipeline = None;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.selected_api_target = None;
            agents.active_model_picker = None;
            agents.show_archived = false;
            if !same {
                agents.detail_tab = crate::agents::AgentDetailTab::default();
            }
        });
        self.agents_canvas.scene.borrow_mut().cursor = Some(node_key(&agent));
        if self.settings.read(cx).snapshot.is_none() {
            self.refresh_settings(cx);
        }
        // A gate never runs a process: the dock has nothing to show for it.
        let console = match &agent {
            AgentIdentifier::Canonical(role) => {
                self.configure_agent_api(*role, cx);
                true
            }
            AgentIdentifier::FreeForm(slot_id) => {
                self.configure_free_form_api(slot_id.clone(), cx);
                true
            }
            AgentIdentifier::PipelineStep {
                pipeline_id,
                step_id,
            } => self
                .agents
                .read(cx)
                .pipeline(pipeline_id)
                .is_some_and(|pipeline| {
                    pipeline.definition.steps.iter().any(|step| {
                        step.id() == step_id
                            && matches!(step, ikore_core::custom_pipelines::PipelineStep::Agent(_))
                    })
                }),
        };
        if console {
            self.focus_agent_in_dock(agent, None, cx);
        }
        cx.notify();
    }

    /// A pipeline box's title: the pipeline's synthesis opens.
    pub(crate) fn select_canvas_pipeline(&mut self, id: String, cx: &mut Context<Self>) {
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            if agents.selected_pipeline.as_ref() != Some(&id) {
                agents.selected_pipeline_run = None;
            }
            agents.selected_pipeline = Some(id);
            agents.selected_agent = None;
            agents.selected_api_target = None;
            agents.active_model_picker = None;
            agents.show_archived = false;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.pending_pipeline_delete = None;
            agents.detail_tab = crate::agents::AgentDetailTab::default();
        });
        cx.notify();
    }

    /// A click on the background, or `Esc`: the synthesis closes and the
    /// canvas takes the whole width.
    pub(crate) fn close_agents_synthesis(&mut self, cx: &mut Context<Self>) {
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.selected_pipeline_run = None;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.pending_pipeline_delete = None;
            agents.selected_api_target = None;
            agents.active_model_picker = None;
            agents.show_archived = false;
            agents.detail_tab = crate::agents::AgentDetailTab::default();
        });
        cx.notify();
    }

    /// `Esc` on the Agents page: an open drawer closes first, then the
    /// synthesis. Returns whether anything closed.
    pub(crate) fn dismiss_agents_synthesis(&mut self, cx: &mut Context<Self>) -> bool {
        let (drawer, open) = {
            let agents = self.agents.read(cx);
            (
                agents.detail_tab != crate::agents::AgentDetailTab::default(),
                agents.selected_agent.is_some()
                    || agents.show_archived
                    || agents.selected_pipeline.is_some()
                    || agents.show_archived_pipelines
                    || agents.show_global_pipelines
                    || self.pipeline_editor.is_some(),
            )
        };
        if drawer && open {
            self.agents.update(cx, |agents, _| {
                agents.detail_tab = crate::agents::AgentDetailTab::default();
            });
            cx.notify();
            return true;
        }
        if open {
            self.close_agents_synthesis(cx);
        }
        open
    }

    /// API Mode's `Expand viewer`: the History drawer, which shows the
    /// activity viewer, opens or closes.
    pub(crate) fn toggle_agents_history(&mut self, cx: &mut Context<Self>) {
        use crate::agents::AgentDetailTab;
        self.agents.update(cx, |agents, _| {
            agents.detail_tab = if agents.detail_tab == AgentDetailTab::History {
                AgentDetailTab::default()
            } else {
                AgentDetailTab::History
            };
        });
        cx.notify();
    }

    /// `Archived · N`: the archived free-form agents, with restore and delete.
    pub(crate) fn open_archived_agents(&mut self, cx: &mut Context<Self>) {
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.show_archived = true;
        });
        cx.notify();
    }

    /// `Fit`: every box in view.
    pub(crate) fn fit_agents_canvas(&mut self, cx: &mut Context<Self>) {
        self.agents_canvas.scene.borrow_mut().fit();
        cx.notify();
    }

    fn press_agents_node(&mut self, index: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.agents_canvas.scene.borrow_mut().press = Some(Press::Node {
            index,
            origin: position,
            moved: false,
            dragging: false,
        });
        cx.notify();
    }

    fn press_agents_background(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let mut scene = self.agents_canvas.scene.borrow_mut();
        let pan = scene.pan;
        scene.press = Some(Press::Background {
            origin: position,
            pan,
            moved: false,
        });
        drop(scene);
        cx.notify();
    }

    fn move_agents_pointer(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let mut scene = self.agents_canvas.scene.borrow_mut();
        let Some(press) = scene.press else {
            return;
        };
        let distance = |origin: Point<Pixels>| {
            f32::from(position.x - origin.x).hypot(f32::from(position.y - origin.y))
        };
        match press {
            Press::Node {
                index,
                origin,
                moved,
                dragging,
            } => {
                let moved = moved || distance(origin) > DRAG_THRESHOLD;
                let pointer = scene.to_world(position);
                let dragging = if dragging {
                    scene.physics.drag_to(pointer);
                    true
                } else if moved {
                    let start = scene.to_world(origin);
                    let started =
                        scene
                            .physics
                            .begin_drag(index, start, crate::motion::reduced_motion());
                    if started {
                        scene.physics.drag_to(pointer);
                    }
                    started
                } else {
                    false
                };
                scene.press = Some(Press::Node {
                    index,
                    origin,
                    moved,
                    dragging,
                });
            }
            Press::Background { origin, pan, moved } => {
                let moved = moved || distance(origin) > DRAG_THRESHOLD;
                if moved {
                    scene.fitted = true;
                    scene.pan = Vec2::new(
                        pan.x + f32::from(position.x - origin.x),
                        pan.y + f32::from(position.y - origin.y),
                    );
                }
                scene.press = Some(Press::Background { origin, pan, moved });
            }
        }
        drop(scene);
        cx.notify();
    }

    fn release_agents_pointer(&mut self, cx: &mut Context<Self>) {
        let (press, key) = {
            let mut scene = self.agents_canvas.scene.borrow_mut();
            let press = scene.press.take();
            if let Some(Press::Node { dragging: true, .. }) = press {
                scene.physics.end_drag();
            }
            let key = match press {
                Some(Press::Node { index, .. }) => {
                    scene.layout.nodes.get(index).map(|node| node.key.clone())
                }
                _ => None,
            };
            (press, key)
        };
        match press {
            Some(Press::Node { moved: false, .. }) => {
                if let Some(agent) = key.as_deref().and_then(crate::agent_dock::agent_from_key) {
                    self.select_canvas_agent(agent, cx);
                }
            }
            Some(Press::Background { moved: false, .. }) => self.close_agents_synthesis(cx),
            _ => {}
        }
        cx.notify();
    }

    fn scroll_agents_canvas(
        &mut self,
        event: &ScrollWheelEvent,
        line_height: Pixels,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(line_height);
        let mut scene = self.agents_canvas.scene.borrow_mut();
        if event.modifiers.control {
            let factor = (-f32::from(delta.y) * 0.002).exp();
            scene.zoom_at(factor, event.position);
        } else {
            scene.fitted = true;
            scene.pan = Vec2::new(
                scene.pan.x + f32::from(delta.x),
                scene.pan.y + f32::from(delta.y),
            );
        }
        drop(scene);
        cx.notify();
    }

    /// Arrows walk the nodes and `Enter` opens the synthesis; the page takes
    /// `Esc`. Returns whether the key was the canvas's.
    fn agents_canvas_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let direction = match event.keystroke.key.as_str() {
            "left" => Some(Direction::Left),
            "right" => Some(Direction::Right),
            "up" => Some(Direction::Up),
            "down" => Some(Direction::Down),
            "enter" => {
                let key = self.agents_canvas.scene.borrow().cursor.clone();
                if let Some(agent) = key.as_deref().and_then(crate::agent_dock::agent_from_key) {
                    self.select_canvas_agent(agent, cx);
                }
                return true;
            }
            _ => None,
        };
        let Some(direction) = direction else {
            return false;
        };
        let selected = self.agents.read(cx).selected_agent.as_ref().map(node_key);
        let mut scene = self.agents_canvas.scene.borrow_mut();
        let from = scene
            .cursor
            .clone()
            .or(selected)
            .and_then(|key| scene.layout.index_of(&key));
        let next = match from {
            Some(from) => scene.layout.neighbour(from, direction),
            None => (!scene.layout.nodes.is_empty()).then_some(0),
        };
        if let Some(next) = next {
            scene.cursor = Some(scene.layout.nodes[next].key.clone());
        }
        drop(scene);
        cx.notify();
        true
    }
}
