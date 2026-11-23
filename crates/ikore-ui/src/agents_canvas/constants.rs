//! Every measure and parameter of the agents canvas (feature 25, M1), in one
//! place. The values are the feature's starting point, tuned by eye.

// Layout, in canvas pixels at zoom 1.

/// A node's size and corner radius.
pub(crate) const NODE_WIDTH: f32 = 148.0;
pub(crate) const NODE_HEIGHT: f32 = 56.0;
pub(crate) const NODE_RADIUS: f32 = 10.0;
/// The gap between two stages, where their link runs.
pub(crate) const STAGE_GAP: f32 = 40.0;
/// The gap between two loose nodes, across and down.
pub(crate) const LOOSE_GAP: f32 = 14.0;
/// The loose area starts this far below what sits lowest above it.
pub(crate) const LOOSE_TOP_GAP: f32 = 36.0;
/// The loose area's columns.
pub(crate) const LOOSE_COLUMNS: usize = 6;
/// A box's inner margin and title bar.
pub(crate) const BOX_PADDING: f32 = 20.0;
pub(crate) const BOX_TITLE_HEIGHT: f32 = 30.0;
/// The gap between two boxes, and the narrowest a pipeline's box gets.
pub(crate) const BOX_GAP: f32 = 28.0;
pub(crate) const PIPELINE_BOX_MIN_WIDTH: f32 = 360.0;
/// The `Archived · N` footer.
pub(crate) const FOOTER_GAP: f32 = 12.0;
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;
pub(crate) const FOOTER_WIDTH: f32 = 160.0;

/// A node's key for the `New freeform agent` ghost.
pub(crate) const GHOST_KEY: &str = "ghost:new-free-form";
/// The box of the Ikore pipeline.
pub(crate) const DEFAULT_BOX: &str = "default";
/// Free-form agents a project may have, as the page allows today.
pub(crate) const FREE_FORM_LIMIT: usize = 8;

// Navigation.

pub(crate) const ZOOM_MIN: f32 = 0.5;
pub(crate) const ZOOM_MAX: f32 = 2.0;
/// The margin `Fit` leaves around the boxes.
pub(crate) const FIT_MARGIN: f32 = 24.0;
/// Pressing and moving more than this starts a drag; less is a click.
pub(crate) const DRAG_THRESHOLD: f32 = 4.0;
/// The synthesis panel never gets narrower than this.
pub(crate) const SYNTHESIS_MIN_WIDTH: f32 = 300.0;

// Physics: time in seconds, distance in pixels, unit mass.

/// The fixed step and the most steps one frame may take.
pub(crate) const STEP: f32 = 1.0 / 120.0;
pub(crate) const MAX_SUBSTEPS: usize = 8;
/// The spring that brings a node home: underdamped on purpose, so it sways.
pub(crate) const HOME_K: f32 = 140.0;
pub(crate) const HOME_ZETA: f32 = 0.32;
/// The spring that pulls a dragged node to the pointer.
pub(crate) const DRAG_K: f32 = 1_100.0;
pub(crate) const DRAG_ZETA: f32 = 0.75;
/// The coupling along a link.
pub(crate) const LINK_K: f32 = 70.0;
/// A dragged node faster than this sends a ring through the loose nodes,
/// at most one every `RING_INTERVAL`.
pub(crate) const RING_SPEED: f32 = 250.0;
pub(crate) const RING_INTERVAL: f32 = 0.12;
/// How fast the ring travels, its impulse and how it fades with distance.
pub(crate) const RING_WAVE_SPEED: f32 = 650.0;
pub(crate) const RING_IMPULSE: f32 = 90.0;
pub(crate) const RING_DECAY: f32 = 260.0;
/// How far a node may stray from home before a stiff spring holds it.
pub(crate) const LIMIT: f32 = 140.0;
pub(crate) const LIMIT_K: f32 = 2_000.0;
/// The stretch along the motion at speed `STRETCH_SPEED` and above.
pub(crate) const STRETCH: f32 = 0.16;
pub(crate) const STRETCH_SPEED: f32 = 900.0;
/// The outline's sway: modes 2 and 3, their stiffness and damping, and how
/// much a release or an impulse excites them.
pub(crate) const MODE_K: [f32; 2] = [280.0, 420.0];
pub(crate) const MODE_ZETA: f32 = 0.14;
pub(crate) const MODE_GAIN: f32 = 0.06;
/// The most one mode may sway, whatever excites it.
pub(crate) const MODE_MAX: f32 = 0.2;
/// Pixels of outline per unit of mode amplitude.
pub(crate) const WOBBLE_RADIUS: f32 = 6.0;
/// The outline's points while it sways.
pub(crate) const BUBBLE_POINTS: usize = 48;
/// At rest: every speed together, every offset and every sway below these
/// for `REST_HOLD`; then everything snaps home and the canvas stops asking
/// for frames.
pub(crate) const REST_SPEED: f32 = 2.0;
pub(crate) const REST_OFFSET: f32 = 0.3;
pub(crate) const REST_AMPLITUDE: f32 = 0.002;
pub(crate) const REST_HOLD: f32 = 0.15;

// Halo of a running node (D7); the pulse itself is `motion::soft_pulse()`.

/// The halo's opacity and blur, scaled by the pulse.
pub(crate) const HALO_OPACITY: f32 = 0.25;
pub(crate) const HALO_BLUR: f32 = 8.0;
