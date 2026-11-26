//! Hybrid GPUI vector animation engine for `ikore-ui` (no GIF, WebP, or MP4).
//!
//! Two complementary layers keep all icons vector-crisp and theme-tinted through GPUI's
//! `paint_svg` alpha-mask atlas:
//! 1. [`AnimClip`]: 14–16 clean monochrome `.svg` keyframes baked from free Lordicon Lottie JSONs.
//! 2. [`GpuEffect`]: native GPU `TransformationMatrix` rotation/scale around a custom pivot for
//!    existing static SVGs (`Hammer`, `Vault`, `Settings`, `Bell`, `RotateCw`).
//!
//! Hover animations run as a single oneshot cycle when the cursor enters the hitbox and stop
//! requesting frames immediately on completion (0% idle repaint overhead). Continuous loops are
//! reserved exclusively for active/loading states, and every animation obeys
//! [`crate::motion::reduced_motion`].

use std::f32::consts::PI;
use std::hash::{Hash as _, Hasher as _};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseMoveEvent, Pixels,
    SharedString, Style, TransformationMatrix, Window, point, px, radians, size,
};

use crate::icons::Icon;
use crate::pipeline_status::StatusTone;
use crate::theme;

/// Baked monochrome SVG keyframe sequences (`assets/icons/anim/<slug>/f00.svg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AnimClip {
    IkoreBrand,
    StatusSaved,
    StatusLoading,
    StatusIdle,
    StatusAttention,
    StatusError,
    InfoTip,
    NavHome,
    NavNewWork,
    NavProjects,
    NavPulsations,
    NavLogs,
    NavChat,
    NavNotes,
    NavCanvas,
    NavWorkspace,
    HeaderHealth,
    AgentsGhost,
    AgentsTerminal,
    AgentsApi,
    AgentsWand,
    AgentsOracle,
}

impl AnimClip {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 22] = [
        Self::IkoreBrand,
        Self::StatusSaved,
        Self::StatusLoading,
        Self::StatusIdle,
        Self::StatusAttention,
        Self::StatusError,
        Self::InfoTip,
        Self::NavHome,
        Self::NavNewWork,
        Self::NavProjects,
        Self::NavPulsations,
        Self::NavLogs,
        Self::NavChat,
        Self::NavNotes,
        Self::NavCanvas,
        Self::NavWorkspace,
        Self::HeaderHealth,
        Self::AgentsGhost,
        Self::AgentsTerminal,
        Self::AgentsApi,
        Self::AgentsWand,
        Self::AgentsOracle,
    ];

    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::IkoreBrand => "ikore-brand",
            Self::StatusSaved => "status-saved",
            Self::StatusLoading => "status-loading",
            Self::StatusIdle => "status-idle",
            Self::StatusAttention => "status-attention",
            Self::StatusError => "status-error",
            Self::InfoTip => "info-tip",
            Self::NavHome => "nav-home",
            Self::NavNewWork => "nav-new-work",
            Self::NavProjects => "nav-projects",
            Self::NavPulsations => "nav-pulsations",
            Self::NavLogs => "nav-logs",
            Self::NavChat => "nav-chat",
            Self::NavNotes => "nav-notes",
            Self::NavCanvas => "nav-canvas",
            Self::NavWorkspace => "nav-workspace",
            Self::HeaderHealth => "header-health",
            Self::AgentsGhost => "agents-ghost",
            Self::AgentsTerminal => "agents-terminal",
            Self::AgentsApi => "agents-api",
            Self::AgentsWand => "agents-wand",
            Self::AgentsOracle => "agents-oracle",
        }
    }

    pub(crate) const fn frame_count(self) -> usize {
        match self {
            Self::StatusAttention => 14,
            _ => 16,
        }
    }

    /// Keyframe displayed while idle/settled. `StatusSaved` rests on its completed final frame.
    pub(crate) const fn rest_frame(self) -> usize {
        match self {
            Self::StatusSaved => 15,
            _ => 0,
        }
    }

    pub(crate) fn frame_path(self, frame: usize) -> SharedString {
        let clamped = frame.min(self.frame_count().saturating_sub(1));
        SharedString::from(format!("anim/{}/f{clamped:02}.svg", self.slug()))
    }

    pub(crate) const fn default_duration(self) -> Duration {
        match self {
            Self::StatusLoading => Duration::from_millis(1_200),
            Self::IkoreBrand => Duration::from_millis(1_400),
            Self::StatusAttention => Duration::from_millis(520),
            Self::AgentsApi => Duration::from_millis(780),
            Self::AgentsTerminal => Duration::from_millis(760),
            Self::NavChat | Self::NavNotes | Self::StatusIdle => Duration::from_millis(750),
            Self::AgentsGhost | Self::AgentsWand => Duration::from_millis(720),
            _ => Duration::from_millis(620),
        }
    }
}

/// GPU-native transformation animations applied to existing static SVGs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum GpuEffect {
    HammerStrike,
    VaultSpin,
    GearRotate,
    BellRing,
    SpinFull,
}

impl GpuEffect {
    pub(crate) const fn duration(self) -> Duration {
        match self {
            Self::HammerStrike => Duration::from_millis(550),
            Self::VaultSpin => Duration::from_millis(560),
            Self::GearRotate => Duration::from_millis(520),
            Self::BellRing => Duration::from_millis(580),
            Self::SpinFull => Duration::from_millis(700),
        }
    }

    /// Relative pivot `(x, y)` in `[0.0, 1.0]` inside the icon bounds.
    const fn pivot(self) -> (f32, f32) {
        match self {
            Self::HammerStrike => (0.30, 0.80),
            Self::BellRing => (0.50, 0.15),
            Self::VaultSpin | Self::GearRotate | Self::SpinFull => (0.50, 0.50),
        }
    }

    fn matrix(self, bounds: Bounds<Pixels>, scale_factor: f32, t: f32) -> TransformationMatrix {
        let t = t.clamp(0.0, 1.0);
        let (angle_deg, scale) = match self {
            Self::HammerStrike => {
                let deg = if t < 0.30 {
                    lerp(0.0, -28.0, ease_in_out(t / 0.30))
                } else if t < 0.60 {
                    lerp(-28.0, 18.0, ease_in_out((t - 0.30) / 0.30))
                } else if t < 0.80 {
                    lerp(18.0, -6.0, ease_in_out((t - 0.60) / 0.20))
                } else {
                    lerp(-6.0, 0.0, ease_in_out((t - 0.80) / 0.20))
                };
                (deg, 1.0)
            }
            Self::VaultSpin => {
                let deg = if t < 0.55 {
                    lerp(0.0, 90.0, ease_in_out(t / 0.55))
                } else {
                    lerp(90.0, 0.0, ease_in_out((t - 0.55) / 0.45))
                };
                let scale = 1.0 + 0.06 * (PI * t).sin();
                (deg, scale)
            }
            Self::GearRotate => {
                let eased = 1.0 - (1.0 - t).powi(5);
                // `settings.svg` has 6-fold tooth symmetry, so 120 deg lands seamlessly back at rest.
                (120.0 * eased, 1.0)
            }
            Self::BellRing => {
                let deg = if t < 0.22 {
                    lerp(0.0, 16.0, ease_in_out(t / 0.22))
                } else if t < 0.46 {
                    lerp(16.0, -14.0, ease_in_out((t - 0.22) / 0.24))
                } else if t < 0.68 {
                    lerp(-14.0, 9.0, ease_in_out((t - 0.46) / 0.22))
                } else if t < 0.85 {
                    lerp(9.0, -5.0, ease_in_out((t - 0.68) / 0.17))
                } else {
                    lerp(-5.0, 0.0, ease_in_out((t - 0.85) / 0.15))
                };
                (deg, 1.0)
            }
            Self::SpinFull => (360.0 * ease_in_out(t), 1.0),
        };

        let (px_rel, py_rel) = self.pivot();
        let pivot = point(
            bounds.origin.x + bounds.size.width * px_rel,
            bounds.origin.y + bounds.size.height * py_rel,
        );
        let pivot_scaled = pivot.scale(scale_factor);
        let neg_pivot = point(-pivot.x, -pivot.y).scale(scale_factor);

        TransformationMatrix::unit()
            .translate(pivot_scaled)
            .rotate(radians(angle_deg.to_radians()))
            .scale(size(scale, scale))
            .translate(neg_pivot)
    }
}

/// Source specification for an animated icon (baked frame clip or GPU transform on a static SVG).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IconHoverSpec {
    Clip(AnimClip),
    Gpu { icon: Icon, effect: GpuEffect },
}

/// Maps static [`Icon`] glyphs to their hover animation.
pub(crate) const fn hover_spec(icon: Icon) -> Option<IconHoverSpec> {
    match icon {
        Icon::House => Some(IconHoverSpec::Clip(AnimClip::NavHome)),
        Icon::Plus => Some(IconHoverSpec::Clip(AnimClip::NavNewWork)),
        Icon::Folder | Icon::FolderOpen => Some(IconHoverSpec::Clip(AnimClip::NavProjects)),
        Icon::Hammer => Some(IconHoverSpec::Gpu {
            icon,
            effect: GpuEffect::HammerStrike,
        }),
        Icon::Activity => Some(IconHoverSpec::Clip(AnimClip::NavPulsations)),
        Icon::Vault => Some(IconHoverSpec::Gpu {
            icon,
            effect: GpuEffect::VaultSpin,
        }),
        Icon::ScrollText => Some(IconHoverSpec::Clip(AnimClip::NavLogs)),
        Icon::MessageSquare => Some(IconHoverSpec::Clip(AnimClip::NavChat)),
        Icon::NotebookPen => Some(IconHoverSpec::Clip(AnimClip::NavNotes)),
        Icon::Network => Some(IconHoverSpec::Clip(AnimClip::NavCanvas)),
        Icon::Settings => Some(IconHoverSpec::Gpu {
            icon,
            effect: GpuEffect::GearRotate,
        }),
        Icon::Bell => Some(IconHoverSpec::Gpu {
            icon,
            effect: GpuEffect::BellRing,
        }),
        Icon::Info => Some(IconHoverSpec::Clip(AnimClip::InfoTip)),
        Icon::RotateCw => Some(IconHoverSpec::Gpu {
            icon,
            effect: GpuEffect::SpinFull,
        }),
        Icon::Bot => Some(IconHoverSpec::Clip(AnimClip::AgentsGhost)),
        _ => None,
    }
}

/// Playback mode for [`AnimatedIcon`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayMode {
    /// Plays once (`0.0 -> 1.0`) when hovered, then rests on the clip's idle frame.
    HoverOneshot,
    /// Loops continuously while mounted (restricted to genuinely active/loading work).
    Loop,
    /// Rests on the completed frame; replays once whenever `key` changes or when hovered.
    RevealOnKey { key: u64 },
}

#[derive(Default, Clone, Copy)]
struct AnimState {
    was_hovered: bool,
    started_at: Option<Instant>,
    last_key: Option<u64>,
}

/// GPUI `Element` rendering a monochrome vector icon with baked keyframes or GPU transforms.
pub(crate) struct AnimatedIcon {
    id: ElementId,
    spec: IconHoverSpec,
    mode: PlayMode,
    duration: Duration,
    size: Pixels,
    hover_pad: Pixels,
    color: gpui::Rgba,
}

impl AnimatedIcon {
    pub(crate) fn clip(
        id: impl Into<SharedString>,
        clip: AnimClip,
        mode: PlayMode,
        size: Pixels,
        color: gpui::Rgba,
    ) -> Self {
        Self {
            id: ElementId::Name(id.into()),
            spec: IconHoverSpec::Clip(clip),
            mode,
            duration: clip.default_duration(),
            size,
            hover_pad: px(0.0),
            color,
        }
    }

    pub(crate) fn from_spec(
        id: impl Into<SharedString>,
        spec: IconHoverSpec,
        mode: PlayMode,
        size: Pixels,
        color: gpui::Rgba,
    ) -> Self {
        let duration = match spec {
            IconHoverSpec::Clip(clip) => clip.default_duration(),
            IconHoverSpec::Gpu { effect, .. } => effect.duration(),
        };
        Self {
            id: ElementId::Name(id.into()),
            spec,
            mode,
            duration,
            size,
            hover_pad: px(0.0),
            color,
        }
    }

    pub(crate) fn hover_pad(mut self, pad: Pixels) -> Self {
        self.hover_pad = pad;
        self
    }

    fn is_hover_reactive(&self) -> bool {
        !matches!(self.mode, PlayMode::Loop)
    }

    fn paint_frame(
        &self,
        bounds: Bounds<Pixels>,
        progress: Option<f32>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (path, matrix) = match self.spec {
            IconHoverSpec::Clip(clip) => {
                let frame = match progress {
                    Some(t) => {
                        let count = clip.frame_count();
                        let idx = (t.clamp(0.0, 0.999_99) * count as f32).floor() as usize;
                        idx.min(count.saturating_sub(1))
                    }
                    None => clip.rest_frame(),
                };
                (clip.frame_path(frame), TransformationMatrix::unit())
            }
            IconHoverSpec::Gpu { icon, effect } => {
                let matrix = match progress {
                    Some(t) => effect.matrix(bounds, window.scale_factor(), t),
                    None => TransformationMatrix::unit(),
                };
                (SharedString::from(icon.path()), matrix)
            }
        };

        let _ = window.paint_svg(bounds, path, matrix, self.color.into(), cx);
    }
}

impl IntoElement for AnimatedIcon {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AnimatedIcon {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: size(self.size.into(), self.size.into()),
            flex_shrink: 0.0,
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        if self.is_hover_reactive() {
            let hit_bounds = if self.hover_pad > px(0.0) {
                bounds.dilate(self.hover_pad)
            } else {
                bounds
            };
            Some(window.insert_hitbox(hit_bounds, HitboxBehavior::Normal))
        } else {
            None
        }
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(global_id) = global_id else {
            self.paint_frame(bounds, None, window, cx);
            return;
        };

        if crate::motion::reduced_motion() {
            window.with_element_state::<AnimState, _>(global_id, |state, _| {
                let mut state = state.unwrap_or_default();
                state.started_at = None;
                if let PlayMode::RevealOnKey { key } = self.mode {
                    state.last_key = Some(key);
                } else {
                    state.last_key = Some(0);
                }
                ((), state)
            });
            self.paint_frame(bounds, None, window, cx);
            return;
        }

        let hovered = hitbox.as_ref().is_some_and(|hb| hb.is_hovered(window));
        let now = Instant::now();
        let duration_secs = self.duration.as_secs_f32().max(0.05);

        let (progress, prev_hovered) =
            window.with_element_state::<AnimState, _>(global_id, |state, window| {
                let mut state = state.unwrap_or_default();
                let prev_hovered = state.was_hovered;

                match self.mode {
                    PlayMode::Loop => {
                        let started = *state.started_at.get_or_insert(now);
                        state.last_key = Some(0);
                        let frac =
                            (now.duration_since(started).as_secs_f32() / duration_secs) % 1.0;
                        window.request_animation_frame();
                        ((Some(frac), prev_hovered), state)
                    }
                    PlayMode::HoverOneshot => {
                        state.last_key = Some(0);
                        if hovered && !prev_hovered {
                            state.started_at = Some(now);
                        }
                        state.was_hovered = hovered;
                        let frac = if let Some(started) = state.started_at {
                            let f = now.duration_since(started).as_secs_f32() / duration_secs;
                            if f < 1.0 {
                                window.request_animation_frame();
                                Some(f)
                            } else {
                                state.started_at = None;
                                None
                            }
                        } else {
                            None
                        };
                        ((frac, hovered), state)
                    }
                    PlayMode::RevealOnKey { key } => {
                        if let Some(prev) = state.last_key
                            && prev != key
                        {
                            state.started_at = Some(now);
                        }
                        state.last_key = Some(key);
                        if hovered && !prev_hovered {
                            state.started_at = Some(now);
                        }
                        state.was_hovered = hovered;
                        let frac = if let Some(started) = state.started_at {
                            let f = now.duration_since(started).as_secs_f32() / duration_secs;
                            if f < 1.0 {
                                window.request_animation_frame();
                                Some(f)
                            } else {
                                state.started_at = None;
                                None
                            }
                        } else {
                            None
                        };
                        ((frac, hovered), state)
                    }
                }
            });

        if let Some(hitbox) = hitbox.clone() {
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, _cx| {
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) != prev_hovered {
                    window.refresh();
                }
            });
        }

        self.paint_frame(bounds, progress, window, cx);
    }
}

/// Semantic replacement for ordinary UI status dots (`widgets::status_dot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusGlyph {
    Brand { busy: bool },
    Saved { key: u64 },
    Success,
    Loading,
    Idle,
    Attention,
    Error,
}

pub(crate) fn status_glyph(
    id: impl Into<SharedString>,
    kind: StatusGlyph,
    size: Pixels,
    color: gpui::Rgba,
) -> AnyElement {
    let (clip, mode, pad) = match kind {
        StatusGlyph::Brand { busy: true } => (AnimClip::IkoreBrand, PlayMode::Loop, px(4.0)),
        StatusGlyph::Brand { busy: false } => {
            (AnimClip::IkoreBrand, PlayMode::HoverOneshot, px(6.0))
        }
        StatusGlyph::Saved { key } => (
            AnimClip::StatusSaved,
            PlayMode::RevealOnKey { key: key | 1 },
            px(4.0),
        ),
        StatusGlyph::Success => (AnimClip::StatusSaved, PlayMode::HoverOneshot, px(4.0)),
        StatusGlyph::Loading => (AnimClip::StatusLoading, PlayMode::Loop, px(0.0)),
        StatusGlyph::Idle => (AnimClip::StatusIdle, PlayMode::HoverOneshot, px(4.0)),
        StatusGlyph::Attention => (AnimClip::StatusAttention, PlayMode::HoverOneshot, px(4.0)),
        StatusGlyph::Error => (AnimClip::StatusError, PlayMode::HoverOneshot, px(4.0)),
    };
    AnimatedIcon::clip(id, clip, mode, size, color)
        .hover_pad(pad)
        .into_any_element()
}

pub(crate) fn status_glyph_for_tone(
    id: impl Into<SharedString>,
    tone: StatusTone,
    size: Pixels,
    color: gpui::Rgba,
) -> AnyElement {
    let kind = match tone {
        StatusTone::Active => StatusGlyph::Loading,
        StatusTone::Attention => StatusGlyph::Attention,
        StatusTone::Failure => StatusGlyph::Error,
        StatusTone::Success => StatusGlyph::Success,
        StatusTone::Idle => StatusGlyph::Idle,
    };
    status_glyph(id, kind, size, color)
}

/// Derives the status-bar glyph and colour from the live `AppModel` status string.
pub(crate) fn status_bar_glyph(status: &str, needs_resync: bool) -> (StatusGlyph, gpui::Rgba) {
    let lower = status.to_ascii_lowercase();
    if needs_resync
        || lower.contains("failed")
        || lower.contains("error")
        || lower.contains("gap detected")
        || lower.contains("delayed")
        || lower.contains("conflict")
        || lower.contains("unavailable")
    {
        return (StatusGlyph::Error, theme::danger());
    }

    if status.ends_with('…')
        || status.ends_with("...")
        || lower.contains("restoring")
        || lower.contains("saving")
        || lower.contains("applying")
        || lower.contains("opening")
        || lower.contains("granting")
        || lower.contains("reloading")
        || lower.contains("refreshing")
        || lower.contains("waiting")
    {
        return (StatusGlyph::Loading, theme::running());
    }

    if lower.contains("saved")
        || lower.contains("applied")
        || lower.contains("granted")
        || lower.contains("reloaded")
        || lower.contains("resynchronized")
        || lower.contains("ressincronizado")
        || lower.contains("copied")
    {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        status.hash(&mut hasher);
        return (
            StatusGlyph::Saved {
                key: hasher.finish() | 1,
            },
            theme::success(),
        );
    }

    (StatusGlyph::Success, theme::success())
}

fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_specs_cover_primary_navigation_and_agents_icons() {
        assert_eq!(
            hover_spec(Icon::Bot),
            Some(IconHoverSpec::Clip(AnimClip::AgentsGhost))
        );
        assert!(hover_spec(Icon::Hammer).is_some());
        assert!(hover_spec(Icon::Vault).is_some());
        assert!(hover_spec(Icon::Settings).is_some());
        assert!(hover_spec(Icon::Bell).is_some());
        assert!(hover_spec(Icon::Info).is_some());
    }

    #[test]
    fn status_bar_glyph_distinguishes_error_loading_saved_and_ready() {
        assert_eq!(
            status_bar_glyph("Session saved", true).0,
            StatusGlyph::Error
        );
        assert_eq!(
            status_bar_glyph("Failed to restore session", false).0,
            StatusGlyph::Error
        );
        assert_eq!(
            status_bar_glyph("Restoring persisted session…", false).0,
            StatusGlyph::Loading
        );
        assert_eq!(
            status_bar_glyph("Applying HITL decision…", false).0,
            StatusGlyph::Loading
        );
        assert!(matches!(
            status_bar_glyph("Session saved", false).0,
            StatusGlyph::Saved { .. }
        ));
        assert!(matches!(
            status_bar_glyph("File saved: src/main.rs", false).0,
            StatusGlyph::Saved { .. }
        ));
        assert_eq!(
            status_bar_glyph("Ready — select a workspace", false).0,
            StatusGlyph::Success
        );
    }

    #[test]
    fn gear_rotation_settles_on_6_tooth_symmetry_and_frame_paths_clamp() {
        assert_eq!(
            AnimClip::StatusAttention.frame_path(999),
            SharedString::from("anim/status-attention/f13.svg")
        );
        assert_eq!(AnimClip::StatusSaved.rest_frame(), 15);
        assert_eq!(AnimClip::StatusIdle.rest_frame(), 0);
        let m = GpuEffect::GearRotate.matrix(
            Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(18.0), px(18.0)),
            },
            1.0,
            1.0,
        );
        // cos(120 deg) = -0.5, sin(120 deg) = 0.8660254
        assert!((m.rotation_scale[0][0] - (-0.5)).abs() < 1e-4);
    }
}
