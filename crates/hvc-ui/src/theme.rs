use gpui::{Rgba, rgb};
use hvc_core::llm::AgentRole;

pub fn canvas() -> Rgba {
    rgb(0x0b0f14)
}

pub fn surface() -> Rgba {
    rgb(0x111820)
}

pub fn surface_raised() -> Rgba {
    rgb(0x18212b)
}

pub fn border() -> Rgba {
    rgb(0x263341)
}

pub fn text() -> Rgba {
    rgb(0xe8edf2)
}

pub fn text_muted() -> Rgba {
    rgb(0x8fa0b2)
}

pub fn accent() -> Rgba {
    rgb(0x22d3ee)
}

pub fn accent_soft() -> Rgba {
    rgb(0x0c2a36)
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

#[allow(dead_code)]
pub fn success_soft() -> Rgba {
    rgb(0x0f2d1a)
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
    surface_raised()
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
    }
}
