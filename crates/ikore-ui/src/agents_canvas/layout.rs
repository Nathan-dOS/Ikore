//! Where everything sits on the agents canvas (feature 25, M1): the boxes,
//! the nodes and the links. Pure and deterministic: the same agents give the
//! same positions, in canvas pixels at zoom 1.

use super::constants::{
    BOX_GAP, BOX_PADDING, BOX_TITLE_HEIGHT, DEFAULT_BOX, FOOTER_GAP, FOOTER_HEIGHT, FOOTER_WIDTH,
    LOOSE_COLUMNS, LOOSE_GAP, LOOSE_TOP_GAP, NODE_HEIGHT, NODE_WIDTH, PIPELINE_BOX_MIN_WIDTH,
    STAGE_GAP,
};
use super::physics::Vec2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Rect {
    pub(crate) const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub(crate) fn right(self) -> f32 {
        self.x + self.width
    }

    pub(crate) fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub(crate) fn center(self) -> Vec2 {
        Vec2::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    #[cfg(test)]
    pub(crate) fn contains(self, other: Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// The smallest rectangle around both.
    pub(crate) fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self::new(
            x,
            y,
            self.right().max(other.right()) - x,
            self.bottom().max(other.bottom()) - y,
        )
    }
}

/// What a node is on the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeKind {
    /// A stage of the Ikore pipeline, on its row, linked to its neighbours.
    Stage,
    /// A support agent, a role in neither list, or a free-form agent: in
    /// the box's corner, without links.
    Loose,
    /// `New freeform agent`.
    Ghost,
    /// A step of one of the developer's pipelines, linked to its neighbours
    /// in its own box (feature 25, M2).
    Step,
}

/// How a link is drawn. Only `Line` is drawn for now; the animated wave
/// comes later (D9) without touching the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgeStyle {
    Line,
    #[allow(dead_code)]
    Wave,
}

/// The agents to lay out, in their order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LayoutInput {
    /// The pipeline's stages, in `STAGES` order, by agent key.
    pub(crate) stages: Vec<String>,
    /// Support agents, then roles in neither list, then free-form agents by
    /// position, by agent key.
    pub(crate) loose: Vec<String>,
    /// Whether the `New freeform agent` ghost closes the loose area.
    pub(crate) ghost: bool,
    /// Archived free-form agents: the footer shows them as a count.
    pub(crate) archived: usize,
    /// The developer's pipelines, one box each below the default one.
    pub(crate) pipelines: Vec<PipelineBoxInput>,
}

/// A pipeline's box: its id and its steps, by agent key, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PipelineBoxInput {
    pub(crate) id: String,
    pub(crate) steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodeLayout {
    pub(crate) key: String,
    pub(crate) kind: NodeKind,
    /// Where the node rests.
    pub(crate) home: Rect,
    pub(crate) box_index: usize,
    /// The keyboard's grid: row 0 is the stages; the loose rows follow.
    pub(crate) row: usize,
    pub(crate) column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EdgeLayout {
    /// Node indexes: from the right edge of `from` to the left edge of `to`.
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) style: EdgeStyle,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoxLayout {
    /// `default`, or the pipeline's id.
    pub(crate) id: String,
    /// One of the developer's pipelines.
    pub(crate) pipeline: bool,
    pub(crate) rect: Rect,
    /// The title bar's band.
    pub(crate) title: Rect,
    /// `Archived · N`, when there is an archived agent.
    pub(crate) footer: Option<Rect>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CanvasLayout {
    pub(crate) boxes: Vec<BoxLayout>,
    pub(crate) nodes: Vec<NodeLayout>,
    pub(crate) edges: Vec<EdgeLayout>,
}

/// An arrow key, for moving between nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The default box at the origin: the stages on their row, the loose nodes
/// in a grid of six columns below them, the footer under everything.
pub(crate) fn layout(input: &LayoutInput) -> CanvasLayout {
    let mut nodes = Vec::new();
    let stage_y = BOX_TITLE_HEIGHT + BOX_PADDING;
    for (index, key) in input.stages.iter().enumerate() {
        nodes.push(NodeLayout {
            key: key.clone(),
            kind: NodeKind::Stage,
            home: Rect::new(
                BOX_PADDING + index as f32 * (NODE_WIDTH + STAGE_GAP),
                stage_y,
                NODE_WIDTH,
                NODE_HEIGHT,
            ),
            box_index: 0,
            row: 0,
            column: index,
        });
    }
    let mut bottom = if input.stages.is_empty() {
        BOX_TITLE_HEIGHT
    } else {
        stage_y + NODE_HEIGHT
    };
    let loose_y = bottom + LOOSE_TOP_GAP;
    let loose = input
        .loose
        .iter()
        .map(|key| (key.clone(), NodeKind::Loose))
        .chain(
            input
                .ghost
                .then(|| (super::constants::GHOST_KEY.to_owned(), NodeKind::Ghost)),
        );
    let first_loose_row = usize::from(!input.stages.is_empty());
    for (index, (key, kind)) in loose.enumerate() {
        let row = index / LOOSE_COLUMNS;
        let column = index % LOOSE_COLUMNS;
        let home = Rect::new(
            BOX_PADDING + column as f32 * (NODE_WIDTH + LOOSE_GAP),
            loose_y + row as f32 * (NODE_HEIGHT + LOOSE_GAP),
            NODE_WIDTH,
            NODE_HEIGHT,
        );
        bottom = bottom.max(home.bottom());
        nodes.push(NodeLayout {
            key,
            kind,
            home,
            box_index: 0,
            row: first_loose_row + row,
            column,
        });
    }
    let footer = (input.archived > 0).then(|| {
        let footer = Rect::new(
            BOX_PADDING,
            bottom + FOOTER_GAP,
            FOOTER_WIDTH,
            FOOTER_HEIGHT,
        );
        bottom = footer.bottom();
        footer
    });
    let widest = nodes
        .iter()
        .map(|node| node.home.right())
        .chain(footer.map(Rect::right))
        .fold(BOX_PADDING, f32::max);
    let width = widest + BOX_PADDING;
    let rect = Rect::new(0.0, 0.0, width, bottom + BOX_PADDING);
    let edges = (1..input.stages.len())
        .map(|to| EdgeLayout {
            from: to - 1,
            to,
            style: EdgeStyle::Line,
        })
        .collect::<Vec<_>>();
    let mut edges = edges;
    let mut boxes = vec![BoxLayout {
        id: DEFAULT_BOX.to_owned(),
        pipeline: false,
        rect,
        title: Rect::new(0.0, 0.0, width, BOX_TITLE_HEIGHT),
        footer,
    }];
    // The developer's pipelines, each in a box of its own below the default
    // one, its steps linked left to right (D5, D6).
    let mut top = rect.bottom() + BOX_GAP;
    // Each box is one row for the arrows, after the default box's rows.
    let first_row = nodes.iter().map(|node| node.row + 1).max().unwrap_or(0);
    for (offset, pipeline) in input.pipelines.iter().enumerate() {
        let box_index = offset + 1;
        let row = first_row + offset;
        let step_y = top + BOX_TITLE_HEIGHT + BOX_PADDING;
        let first = nodes.len();
        for (column, key) in pipeline.steps.iter().enumerate() {
            nodes.push(NodeLayout {
                key: key.clone(),
                kind: NodeKind::Step,
                home: Rect::new(
                    BOX_PADDING + column as f32 * (NODE_WIDTH + STAGE_GAP),
                    step_y,
                    NODE_WIDTH,
                    NODE_HEIGHT,
                ),
                box_index,
                row,
                column,
            });
        }
        edges.extend((first + 1..nodes.len()).map(|to| EdgeLayout {
            from: to - 1,
            to,
            style: EdgeStyle::Line,
        }));
        let widest = nodes[first..]
            .iter()
            .map(|node| node.home.right() + BOX_PADDING)
            .fold(PIPELINE_BOX_MIN_WIDTH, f32::max);
        let height = BOX_TITLE_HEIGHT + 2.0 * BOX_PADDING + NODE_HEIGHT;
        boxes.push(BoxLayout {
            id: pipeline.id.clone(),
            pipeline: true,
            rect: Rect::new(0.0, top, widest, height),
            title: Rect::new(0.0, top, widest, BOX_TITLE_HEIGHT),
            footer: None,
        });
        top += height + BOX_GAP;
    }
    CanvasLayout {
        boxes,
        nodes,
        edges,
    }
}

impl CanvasLayout {
    pub(crate) fn index_of(&self, key: &str) -> Option<usize> {
        self.nodes.iter().position(|node| node.key == key)
    }

    /// Everything on the canvas, for `Fit`.
    pub(crate) fn bounds(&self) -> Option<Rect> {
        self.boxes.iter().map(|item| item.rect).reduce(Rect::union)
    }

    /// The agent an arrow key leads to: left and right along the row, up and
    /// down to the nearest column of the next row. The ghost is not an agent:
    /// the arrows pass it by.
    pub(crate) fn neighbour(&self, from: usize, direction: Direction) -> Option<usize> {
        let node = self.nodes.get(from)?;
        let same_row = |row: usize| {
            self.nodes.iter().enumerate().filter(move |(_, candidate)| {
                candidate.row == row && candidate.kind != NodeKind::Ghost
            })
        };
        match direction {
            Direction::Left => same_row(node.row)
                .filter(|(_, candidate)| candidate.column < node.column)
                .max_by_key(|(_, candidate)| candidate.column)
                .map(|(index, _)| index),
            Direction::Right => same_row(node.row)
                .filter(|(_, candidate)| candidate.column > node.column)
                .min_by_key(|(_, candidate)| candidate.column)
                .map(|(index, _)| index),
            Direction::Up | Direction::Down => {
                let row = if direction == Direction::Up {
                    node.row.checked_sub(1)?
                } else {
                    node.row + 1
                };
                same_row(row)
                    .min_by_key(|(_, candidate)| candidate.column.abs_diff(node.column))
                    .map(|(index, _)| index)
            }
        }
    }
}
