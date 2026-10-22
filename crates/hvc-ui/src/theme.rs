use gpui::{Rgba, rgb};
use hvc_core::llm::AgentRole;

// Elevation, darkest first. The shell chrome (header, rails, footer) sits directly on `canvas`
// with no borders of its own; `surface` is the one framed container around the centre view;
// `surface_raised` is a card, row or input inside it.

/// Raw values shared with the native caption (`title_bar.rs`).
pub const CANVAS_RGB: u32 = 0x06080b;
pub const TEXT_MUTED_RGB: u32 = 0x8b97a7;
pub const BORDER_RGB: u32 = 0x1d242e;

pub fn canvas() -> Rgba {
    rgb(CANVAS_RGB)
}

pub fn surface() -> Rgba {
    rgb(0x0d1117)
}

pub fn surface_raised() -> Rgba {
    rgb(0x151b23)
}

/// Hover fill for rows and secondary controls; one step above `surface_raised`.
pub fn surface_hover() -> Rgba {
    rgb(0x1c232d)
}

/// Hairline between regions inside the centre container. Kept close to `surface` on purpose:
/// separation comes from elevation first, lines second.
pub fn border() -> Rgba {
    rgb(BORDER_RGB)
}

/// Outline of inputs and secondary buttons, which must read as controls.
pub fn border_strong() -> Rgba {
    rgb(0x2c3643)
}

pub fn text() -> Rgba {
    rgb(0xe6ebf1)
}

pub fn text_muted() -> Rgba {
    rgb(TEXT_MUTED_RGB)
}

/// Eyebrows and tertiary labels.
pub fn text_subtle() -> Rgba {
    rgb(0x5f6b7b)
}

pub fn accent() -> Rgba {
    rgb(0x22d3ee)
}

/// Primary-button hover: the accent one step lighter.
pub fn accent_hover() -> Rgba {
    rgb(0x67e8f9)
}

pub fn accent_soft() -> Rgba {
    rgb(0x0a2630)
}

/// In-flight work (a running stage, a held runtime slot). Distinct from the brand accent so
/// "running" and "done" never share a colour.
pub fn running() -> Rgba {
    rgb(0x73b7ff)
}

pub fn running_soft() -> Rgba {
    rgb(0x10233a)
}

// Reserved for the Review-section and status-badge work (see the UI flow plan, Parts 3-4).
#[allow(dead_code)]
pub fn purple() -> Rgba {
    rgb(0xa78bfa)
}

#[allow(dead_code)]
pub fn purple_soft() -> Rgba {
    rgb(0x1e1630)
}

pub fn success() -> Rgba {
    rgb(0x4ade80)
}

pub fn success_soft() -> Rgba {
    rgb(0x0e2a19)
}

pub fn danger() -> Rgba {
    rgb(0xff7b72)
}

pub fn warning() -> Rgba {
    rgb(0xf2c66d)
}

pub fn warning_soft() -> Rgba {
    rgb(0x40351f)
}

pub fn danger_soft() -> Rgba {
    rgb(0x421f24)
}

pub const HOVER_OPACITY: f32 = 0.85;

pub fn hover_bg() -> Rgba {
    surface_hover()
}

// Reserved for the shared widgets introduced alongside the UI flow work (see widgets.rs).
#[allow(dead_code)]
pub const SPACING_XS: f32 = 4.0;
#[allow(dead_code)]
pub const SPACING_SM: f32 = 8.0;
#[allow(dead_code)]
pub const SPACING_MD: f32 = 12.0;
#[allow(dead_code)]
pub const SPACING_LG: f32 = 16.0;
#[allow(dead_code)]
pub const SPACING_XL: f32 = 20.0;

pub const FONT_UI: &str = "Segoe UI";
pub const FONT_MONO: &str = "Cascadia Mono";

pub fn agent_color(role: AgentRole) -> Rgba {
    match role {
        AgentRole::IntakeRouter => rgb(0x6ea8fe),
        AgentRole::TaskSpecifier => rgb(0xb78cff),
        AgentRole::ContractAgent => rgb(0xf2c66d),
        AgentRole::BackendAgent => rgb(0x54d6a2),
        AgentRole::TestAgent => rgb(0xff7b72),
        AgentRole::Integrator => rgb(0x67d9e8),
        AgentRole::LearningAgent => rgb(0xf08bd5),
        AgentRole::FeatureAgent => rgb(0x6bded8),
        AgentRole::Visualizer => rgb(0xc4b5fd),
        AgentRole::NotesScribe => rgb(0xf5a97f),
    }
}

pub fn agent_soft_color(role: AgentRole) -> Rgba {
    match role {
        AgentRole::IntakeRouter => rgb(0x192d49),
        AgentRole::TaskSpecifier => rgb(0x2d2145),
        AgentRole::ContractAgent => rgb(0x40351f),
        AgentRole::BackendAgent => rgb(0x183c33),
        AgentRole::TestAgent => rgb(0x421f24),
        AgentRole::Integrator => rgb(0x173740),
        AgentRole::LearningAgent => rgb(0x3d2038),
        AgentRole::FeatureAgent => rgb(0x163d3c),
        AgentRole::Visualizer => rgb(0x2a2540),
        AgentRole::NotesScribe => rgb(0x40291c),
    }
}
