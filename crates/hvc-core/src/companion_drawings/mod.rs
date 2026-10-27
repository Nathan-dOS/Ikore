//! Native architectural drawing contracts, independent of the UI framework.

pub mod geometry;
pub mod service;
pub mod svg;
pub mod validation;
pub mod workspace_sync;

pub use service::{
    CompanionDrawing, CompanionDrawingSummary, CompanionDrawingsService, DrawingSyncStatus,
    DrawingsEvent, SyncReport,
};

pub use svg::export_svg;
pub use validation::{normalize_scene, sanitize_title, scene_json, validate_viewport};

use serde::{Deserialize, Serialize};

pub const CANVAS_SCHEMA_VERSION: u32 = 1;
pub const MAX_ELEMENTS: usize = 1_000;
pub const MAX_LABEL_CHARS: usize = 4_000;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_SCENE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SVG_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_COORDINATE: f32 = 1_000_000.0;
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.0;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasBackgroundMode {
    #[default]
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasSemanticColor {
    #[default]
    Neutral,
    Blue,
    Green,
    Amber,
    Red,
}

impl CanvasSemanticColor {
    pub const ALL: [Self; 5] = [
        Self::Neutral,
        Self::Blue,
        Self::Green,
        Self::Amber,
        Self::Red,
    ];

    pub const fn rgb(self, background: CanvasBackgroundMode) -> u32 {
        match (background, self) {
            (CanvasBackgroundMode::Dark, Self::Neutral) => 0xe6ebf1,
            (CanvasBackgroundMode::Dark, Self::Blue) => 0x67b8f7,
            (CanvasBackgroundMode::Dark, Self::Green) => 0x65d69a,
            (CanvasBackgroundMode::Dark, Self::Amber) => 0xf4c369,
            (CanvasBackgroundMode::Dark, Self::Red) => 0xf78181,
            (CanvasBackgroundMode::Light, Self::Neutral) => 0x1e293b,
            (CanvasBackgroundMode::Light, Self::Blue) => 0x1971c2,
            (CanvasBackgroundMode::Light, Self::Green) => 0x237a42,
            (CanvasBackgroundMode::Light, Self::Amber) => 0xa86108,
            (CanvasBackgroundMode::Light, Self::Red) => 0xc92a2a,
        }
    }
}

impl CanvasBackgroundMode {
    pub const fn rgb(self) -> u32 {
        match self {
            Self::Dark => 0x06080b,
            Self::Light => 0xf8f9fa,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasStrokeWidth {
    Thin,
    #[default]
    Medium,
    Bold,
}

impl CanvasStrokeWidth {
    pub const fn units(self) -> f32 {
        match self {
            Self::Thin => 1.5,
            Self::Medium => 2.5,
            Self::Bold => 4.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasStrokeStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrowBindingV1 {
    pub element_id: String,
    /// A direction from the target's centre, projected onto its actual border.
    pub anchor: [f32; 2],
    pub gap: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CanvasElementKindV1 {
    Rectangle {
        round: bool,
    },
    Diamond,
    Ellipse,
    Arrow {
        points: [[f32; 2]; 2],
        bidirectional: bool,
        start_binding: Option<ArrowBindingV1>,
        end_binding: Option<ArrowBindingV1>,
    },
    Line {
        points: [[f32; 2]; 2],
    },
    Text {
        font_size: f32,
    },
}

impl CanvasElementKindV1 {
    pub fn points(&self) -> Option<[[f32; 2]; 2]> {
        match self {
            Self::Arrow { points, .. } | Self::Line { points } => Some(*points),
            _ => None,
        }
    }

    pub fn is_connector(&self) -> bool {
        self.points().is_some()
    }

    pub fn is_binding_target(&self) -> bool {
        matches!(self, Self::Rectangle { .. } | Self::Diamond | Self::Ellipse)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasElementV1 {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub stroke_color: CanvasSemanticColor,
    pub fill_enabled: bool,
    pub stroke_width: CanvasStrokeWidth,
    pub stroke_style: CanvasStrokeStyle,
    pub label: String,
    pub container_id: Option<String>,
    pub z_index: i32,
    pub shape: CanvasElementKindV1,
}

impl CanvasElementV1 {
    pub fn new(shape: CanvasElementKindV1, x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            x,
            y,
            width,
            height,
            stroke_color: CanvasSemanticColor::Neutral,
            fill_enabled: false,
            stroke_width: CanvasStrokeWidth::Medium,
            stroke_style: CanvasStrokeStyle::Solid,
            label: String::new(),
            container_id: None,
            z_index: 0,
            shape,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasSceneV1 {
    pub schema_version: u32,
    pub background_mode: CanvasBackgroundMode,
    pub show_grid: bool,
    pub elements: Vec<CanvasElementV1>,
}

impl Default for CanvasSceneV1 {
    fn default() -> Self {
        Self {
            schema_version: CANVAS_SCHEMA_VERSION,
            background_mode: CanvasBackgroundMode::Dark,
            show_grid: true,
            elements: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasViewportV1 {
    pub pan_x: f32,
    pub pan_y: f32,
    pub zoom: f32,
}

impl Default for CanvasViewportV1 {
    fn default() -> Self {
        Self {
            pan_x: 0.0,
            pan_y: 0.0,
            zoom: 1.0,
        }
    }
}

#[cfg(test)]
mod domain_tests;
#[cfg(test)]
mod service_tests;
