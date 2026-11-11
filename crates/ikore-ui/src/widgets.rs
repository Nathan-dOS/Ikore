use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::pipeline_status::{PipelineStatus, step_label};
use crate::pulsations::{PIPELINE_STAGES, stage_label};
use crate::sizing::StyledExt;
use crate::theme;
use gpui::{AnyElement, Context, IntoElement, Pixels, SharedString, div, prelude::*, px};

// `btn`/`btn_confirm`/`inline_banner`/`loading_pill` are adopted progressively as each view is
// touched during the UI flow overhaul (see the navigation/flow plan); until a view is converted,
// these stay unused in a plain build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Danger,
    Ghost,
    Selected,
    /// Accent ink on an accent-tinted fill: a suggested action that is not the primary one.
    Tinted,
}

/// Colours, radius, padding and interaction states shared by every text button. The outer
/// geometry is identical for every variant and state — a 1px border is always there, transparent
/// when a variant has no outline, and the padding is reduced by that pixel so the box keeps the
/// 36px-tall `px_3`/`py_2` footprint the layout tests pin — so a state change never moves a
/// neighbouring control. Handlers and keyboard wiring stay with the caller.
pub(crate) fn button_frame(
    button: gpui::Stateful<gpui::Div>,
    variant: ButtonVariant,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let clear = gpui::transparent_black();
    let button = button
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .px(px(11.0))
        .py(px(7.0))
        .rounded_md()
        .border_1()
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .focus_visible(|style| style.border_color(theme::accent()));
    if !enabled {
        return button
            .bg(theme::surface_raised())
            .border_color(clear)
            .text_color(theme::text_subtle());
    }
    let button = button.cursor_pointer().active(|style| style.opacity(0.8));
    match variant {
        ButtonVariant::Primary => button
            .bg(theme::accent())
            .border_color(theme::accent())
            .text_color(theme::canvas())
            .hover(|style| {
                style
                    .bg(theme::accent_hover())
                    .border_color(theme::accent_hover())
            }),
        ButtonVariant::Secondary => button
            .bg(theme::surface_raised())
            .border_color(theme::border_strong())
            .text_color(theme::text())
            .hover(|style| {
                style
                    .bg(theme::surface_hover())
                    .border_color(theme::text_subtle())
            }),
        ButtonVariant::Ghost => button
            .border_color(clear)
            .text_color(theme::text_muted())
            .hover(|style| style.bg(theme::surface_raised()).text_color(theme::text())),
        ButtonVariant::Danger => button
            .bg(theme::danger_soft())
            .border_color(theme::danger_soft())
            .text_color(theme::danger())
            .hover(|style| style.border_color(theme::danger())),
        ButtonVariant::Selected | ButtonVariant::Tinted => button
            .bg(theme::accent_soft())
            .border_color(theme::accent_soft())
            .text_color(theme::accent())
            .hover(|style| style.border_color(theme::accent())),
    }
}

/// The recessed track of a segmented control (a view's section strip). Segments go inside it.
pub(crate) fn segment_track<E: Styled>(track: E) -> E {
    track
        .flex()
        .items_center()
        .gap_1()
        .p(px(3.0))
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::canvas())
}

/// One segment: the selected one is raised out of the track; the rest are quiet until hovered.
pub(crate) fn segment(
    segment: gpui::Stateful<gpui::Div>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let segment = segment
        .flex_shrink_0()
        .px_3()
        .py(px(5.0))
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        // Background only: a refinement that sets the text colour replaces the whole text style in
        // GPUI, so the label would lose its `text_sm` size while focused.
        .focus_visible(|style| style.bg(theme::accent_soft()));
    if selected {
        segment
            .bg(theme::surface_hover())
            .text_color(theme::text())
            .font_weight(gpui::FontWeight::MEDIUM)
    } else {
        segment
            .text_color(theme::text_muted())
            .hover(|style| style.text_color(theme::text()).bg(theme::surface_raised()))
    }
}

/// Icon and single-line name of a file-tree row, shared by the Workspace and Vault explorers.
/// `folder` is `Some(open)` for a folder (a chevron shows its state) and `None` for a file,
/// which gets a spacer the chevron's width so names stay aligned. Long names end in an ellipsis
/// instead of wrapping onto a second line.
pub(crate) fn tree_row_label(
    folder: Option<bool>,
    name: impl Into<SharedString>,
    ink: gpui::Rgba,
) -> AnyElement {
    let chevron = match folder {
        Some(true) => Some(Icon::ChevronDown),
        Some(false) => Some(Icon::ChevronRight),
        None => None,
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .child(match chevron {
            Some(glyph) => icon(glyph, px(12.0), theme::text_subtle()).into_any_element(),
            None => div().flex_shrink_0().w(px(12.0)).into_any_element(),
        })
        .child(icon(
            if folder.is_some() {
                Icon::Folder
            } else {
                Icon::FileText
            },
            px(14.0),
            if folder.is_some() {
                theme::text_subtle()
            } else {
                ink
            },
        ))
        .child(
            div()
                .ml_1()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(name.into()),
        )
        .into_any_element()
}

/// A filesystem path as the developer would write it: `canonicalize()` on Windows adds the
/// `\\?\` extended-length prefix, which is noise in a header.
pub(crate) fn display_path(path: &str) -> String {
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    }
}

/// Footer shown under an explorer whose tree the core capped at `MAX_TREE_FILES`.
pub(crate) fn tree_capped_notice() -> AnyElement {
    div()
        .debug_selector(|| "tree-capped-notice".into())
        .mt_2()
        .mx_2()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme::surface_raised())
        .text_xs()
        .text_color(theme::text_muted())
        .child(format!(
            "Showing the first {} files. Dependency and build folders are skipped; use search to reach the rest.",
            ikore_core::workspace_service::MAX_TREE_FILES
        ))
        .into_any_element()
}

/// Label colour of a `button_frame` button, so a leading glyph matches its text.
pub(crate) fn button_ink(variant: ButtonVariant, enabled: bool) -> gpui::Rgba {
    if !enabled {
        return theme::text_subtle();
    }
    match variant {
        ButtonVariant::Primary => theme::canvas(),
        ButtonVariant::Secondary => theme::text(),
        ButtonVariant::Ghost => theme::text_muted(),
        ButtonVariant::Danger => theme::danger(),
        ButtonVariant::Selected | ButtonVariant::Tinted => theme::accent(),
    }
}

/// The leading glyph for an action, decided by what its id says it does, so Start, Stop, Save,
/// Send and friends look the same in Agents, Chat, Settings and Forge.
pub(crate) fn action_icon(id: &str) -> Option<Icon> {
    let has = |needle: &str| id.contains(needle);
    Some(if has("stop") || id.ends_with("cancel-turn") {
        Icon::Square
    } else if has("start") || has("resume") {
        Icon::Play
    } else if has("send") {
        Icon::Send
    } else if has("save") {
        Icon::Save
    } else if has("delete") {
        Icon::Trash
    } else if has("copy") {
        Icon::Copy
    } else if has("refresh") || has("retry") || has("reindex") {
        Icon::RotateCw
    } else if has("create") || has("-new-") || id.starts_with("new-") || has("capture-feature") {
        Icon::Plus
    } else if has("test-connection") {
        Icon::Activity
    } else if has("advanced") {
        Icon::Settings
    } else {
        return None;
    })
}

/// Track-and-knob switch. Purely visual: the row that owns it handles the click, so the whole
/// row is the hit area.
pub(crate) fn switch(on: bool, enabled: bool) -> AnyElement {
    div()
        .flex_shrink_0()
        .w(px(34.0))
        .h(px(20.0))
        .p(px(3.0))
        .rounded_full()
        .flex()
        .items_center()
        .when(on, |track| track.justify_end().bg(theme::accent()))
        .when(!on, |track| {
            track
                .justify_start()
                .bg(theme::surface_hover())
                .border_1()
                .border_color(theme::border_strong())
        })
        .opacity(if enabled { 1.0 } else { 0.45 })
        .child(div().size(px(14.0)).rounded_full().bg(if on {
            theme::canvas()
        } else {
            theme::text_muted()
        }))
        .into_any_element()
}

#[allow(dead_code)]
pub fn btn(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    variant: ButtonVariant,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let label = label.into();

    button_frame(
        div()
            .id(id)
            .debug_selector(move || debug_id.to_string())
            .self_start()
            .tab_index(0),
        variant,
        enabled,
    )
    // Enter and Space reach `on_click` through GPUI's keyboard click on the focused button.
    .when(enabled, |b| {
        b.on_click(cx.listener(move |model, _, _, cx| {
            handler(model, cx);
        }))
    })
    .child(label)
    .into_any_element()
}

// Keep the existing view call sites explicit about both confirmation callbacks.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub fn btn_confirm(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    confirming: bool,
    enabled: bool,
    variant: ButtonVariant,
    cx: &mut Context<AppModel>,
    on_first_click: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
    on_confirm: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let current_label: SharedString = if confirming {
        confirm_label.into()
    } else {
        label.into()
    };
    let effective_variant = if confirming {
        ButtonVariant::Danger
    } else {
        variant
    };

    btn(
        id,
        current_label,
        effective_variant,
        enabled,
        cx,
        move |model, cx| {
            if confirming {
                on_confirm(model, cx);
            } else {
                on_first_click(model, cx);
            }
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BannerKind {
    Info,
    Warning,
    Error,
}

#[allow(dead_code)]
pub fn inline_banner(message: impl Into<SharedString>, kind: BannerKind) -> AnyElement {
    let message = message.into();
    let debug_key = message.clone();
    let (bg, fg) = match kind {
        BannerKind::Info => (theme::accent_soft(), theme::accent()),
        BannerKind::Warning => (theme::warning_soft(), theme::warning()),
        BannerKind::Error => (theme::danger_soft(), theme::danger()),
    };
    div()
        .debug_selector(move || debug_key.to_string())
        .p_3()
        .rounded_md()
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .child(message)
        .into_any_element()
}

#[allow(dead_code)]
pub fn loading_pill(label: impl Into<SharedString>) -> AnyElement {
    let label = label.into();
    let debug_key = label.clone();
    div()
        .debug_selector(move || debug_key.to_string())
        .px_3()
        .py_1()
        .rounded_full()
        .bg(theme::accent_soft())
        .text_color(theme::accent())
        .text_xs()
        .child(label)
        .into_any_element()
}

/// The one `(eyebrow, title, count)` section heading: a quiet uppercase eyebrow, a semibold
/// title and the count in a pill. Every list panel (Pulsations, Vault, Settings, Chat) uses it.
pub fn section_header(
    eyebrow: impl Into<SharedString>,
    title: impl Into<SharedString>,
    count: usize,
) -> AnyElement {
    div()
        .flex()
        .items_end()
        .justify_between()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text_subtle())
                        .child(eyebrow.into()),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title.into()),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .min_w(px(24.0))
                .h(px(22.0))
                .px_2()
                .rounded_full()
                .bg(theme::surface_raised())
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme::text_muted())
                .child(count.to_string()),
        )
        .into_any_element()
}

/// Centered empty state: a quiet glyph in a soft disc above one line of guidance. Shared by
/// every panel that can be empty so they read the same.
pub fn empty_state(message: impl Into<SharedString>) -> AnyElement {
    empty_block(Icon::Sparkles, None, message.into())
}

/// `empty_state` with a title, for a whole panel that has nothing to show yet.
pub(crate) fn empty_panel(
    glyph: Icon,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
) -> AnyElement {
    empty_block(glyph, Some(title.into()), description.into())
}

fn empty_block(glyph: Icon, title: Option<SharedString>, message: SharedString) -> AnyElement {
    div()
        .flex_1()
        .size_full()
        .p_5()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(
            div()
                .size(px(44.0))
                .rounded_full()
                .bg(theme::surface_raised())
                .flex()
                .items_center()
                .justify_center()
                .child(icon(glyph, px(20.0), theme::text_subtle())),
        )
        .when_some(title, |block, title| {
            block.child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
        })
        .child(
            div()
                .max_w(px(420.0))
                .text_sm()
                .text_center()
                .text_color(theme::text_muted())
                .child(message),
        )
        .into_any_element()
}

/// Shared key/value fact box (STATUS, TOKENS, ...) used by detail panels.
#[allow(dead_code)]
pub fn fact(label: impl Into<SharedString>, value: impl Into<SharedString>) -> AnyElement {
    div()
        .p_3()
        .rounded_md()
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child(label.into()),
        )
        .child(div().text_sm().child(value.into()))
        .into_any_element()
}

/// A tinted Lucide glyph. Always pass the colour explicitly: `Svg` paints with its own
/// `text_color`, not the inherited one.
pub(crate) fn icon(icon: Icon, size: Pixels, color: gpui::Rgba) -> gpui::Svg {
    gpui::svg()
        .path(icon.path())
        .size(size)
        .flex_shrink_0()
        .text_color(color)
}

/// Default glyph size inside a 36px `icon_button`.
pub(crate) const ICON_SIZE: Pixels = px(18.0);
/// Hit area of an icon-only control; matches the nav rail item size.
pub(crate) const ICON_BUTTON_SIZE: Pixels = px(36.0);

/// Hover tooltip for icon-only controls: a label and, optionally, the keyboard shortcut that
/// does the same thing. The value is the spec; GPUI builds the view lazily when the pointer
/// rests on the control, so it costs nothing until then.
#[derive(Clone)]
pub(crate) struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    /// Extra wait on top of GPUI's 500 ms before the text shows, for explanations of an action
    /// that should not pop up while the pointer merely crosses the control.
    rest: Option<std::time::Duration>,
    rested: bool,
    waiting: bool,
}

impl Tooltip {
    pub(crate) fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            shortcut: None,
            rest: None,
            rested: false,
            waiting: false,
        }
    }

    pub(crate) fn with_shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Shows the tooltip only once the pointer has rested `total` on the control (GPUI's own
    /// 500 ms included).
    pub(crate) fn after_rest(mut self, total: std::time::Duration) -> Self {
        self.rest = Some(total.saturating_sub(std::time::Duration::from_millis(500)));
        self
    }
}

/// Explanations of an action wait this long, so they read as help rather than as noise.
pub(crate) const EXPLAIN_REST: std::time::Duration = std::time::Duration::from_secs(2);

impl From<&'static str> for Tooltip {
    fn from(text: &'static str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Tooltip {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<SharedString> for Tooltip {
    fn from(text: SharedString) -> Self {
        Self::new(text)
    }
}

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(rest) = self.rest.filter(|_| !self.rested) {
            if !self.waiting {
                self.waiting = true;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(rest).await;
                    this.update(cx, |tooltip, cx| {
                        tooltip.rested = true;
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            return div().into_any_element();
        }
        div()
            .debug_selector(|| "tooltip".into())
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme::border())
            .bg(theme::surface_raised())
            .text_xs()
            .text_color(theme::text())
            .font_family(theme::FONT_UI)
            // Explanations wrap at a readable width instead of stretching across the window.
            .max_w(px(320.0))
            .child(div().min_w_0().child(self.text.clone()))
            .when_some(self.shortcut.clone(), |tip, shortcut| {
                tip.child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text_muted())
                        .child(shortcut),
                )
            })
            .into_any_element()
    }
}

/// A plain tooltip builder for `.tooltip(...)`, for labels that may be cut off (a tree row's
/// full path) or icons that need a name.
pub(crate) fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip::new(text.clone())).into()
}

/// The "i" that replaces a paragraph of explanation: a small info glyph beside a heading or a
/// control, with the explanation in its tooltip. `id` only needs to be unique among siblings.
pub(crate) fn info_tip(id: impl Into<SharedString>, text: impl Into<SharedString>) -> AnyElement {
    let id = id.into();
    let selector = format!("info:{id}");
    let text = text.into();
    div()
        .id(SharedString::from(format!("info:{id}")))
        .debug_selector(move || selector.clone())
        .flex_shrink_0()
        .size(px(18.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(gpui::transparent_black())
        .opacity(0.8)
        .tab_index(0)
        .hover(|style| style.opacity(1.0).bg(theme::surface_hover()))
        .focus_visible(|style| {
            style
                .opacity(1.0)
                .border_color(theme::accent())
                .bg(theme::surface_hover())
        })
        .child(icon(Icon::Info, px(14.0), theme::text_subtle()))
        .tooltip(move |_, cx| cx.new(|_| Tooltip::new(text.clone())).into())
        .into_any_element()
}

/// Icon-only control with the same focus/keyboard/disabled discipline as `btn`. `selected`
/// renders the accent-soft "current" state used by the nav rail; `badge_count` overlays a count
/// at the top-right corner. The handler receives the `Window` because flow steps need it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn icon_button(
    id: impl Into<SharedString>,
    glyph: Icon,
    tooltip: impl Into<Tooltip>,
    selected: bool,
    badge_count: Option<usize>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut gpui::Window, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let id = id.into();
    let debug_id = id.clone();
    let tooltip = tooltip.into();
    let ink = if selected {
        theme::accent()
    } else {
        theme::text_muted()
    };

    div()
        .id(id)
        .debug_selector(move || debug_id.to_string())
        .relative()
        .flex_shrink_0()
        .size(ICON_BUTTON_SIZE)
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(gpui::transparent_black())
        .when(selected, |b| b.bg(theme::accent_soft()))
        .text_color(ink)
        .opacity(if enabled { 1.0 } else { 0.45 })
        .tab_index(0)
        .focus_visible(|style| style.border_color(theme::accent()))
        .tooltip(move |_, cx| cx.new(|_| tooltip.clone()).into())
        .when(enabled, |b| {
            b.cursor_pointer()
                .hover(|s| s.bg(theme::surface_raised()).text_color(theme::text()))
                .on_click(cx.listener(move |model, _, window, cx| {
                    handler(model, window, cx);
                }))
        })
        .child(icon(glyph, ICON_SIZE, ink))
        .when_some(badge_count.filter(|count| *count > 0), |button, count| {
            button.child(div().absolute().top_0().right_0().child(badge(
                count,
                theme::accent(),
                theme::canvas(),
            )))
        })
        .into_any_element()
}

/// Small numeric count pill (bell, rail items, section counters).
pub(crate) fn badge(count: usize, bg: gpui::Rgba, fg: gpui::Rgba) -> AnyElement {
    div()
        .min_w(px(16.0))
        .h(px(16.0))
        .px_1()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .bg(bg)
        .text_color(fg)
        .child(count.to_string())
        .into_any_element()
}

/// 8px status dot; the shell's cheapest live indicator.
pub(crate) fn status_dot(color: gpui::Rgba) -> AnyElement {
    div()
        .flex_shrink_0()
        .size_2()
        .rounded_full()
        .bg(color)
        .into_any_element()
}

/// Outlined text chip in a semantic colour (agent role, run state).
#[allow(dead_code)]
pub(crate) fn chip(label: impl Into<SharedString>, color: gpui::Rgba) -> AnyElement {
    div()
        .px_2()
        .py_1()
        .rounded_full()
        .border_1()
        .border_color(color)
        .text_xs()
        .text_color(color)
        .child(label.into())
        .into_any_element()
}

/// Horizontal hairline separating groups in a vertical rail.
#[allow(dead_code)]
pub(crate) fn rail_divider() -> AnyElement {
    div()
        .w(px(24.0))
        .h_px()
        .my_1()
        .bg(theme::border())
        .into_any_element()
}

/// The six-stage stepper for a pulsation. `id_prefix` scopes the debug selectors
/// (`{prefix}` and `{prefix}-stage-{stage}`); `vertical` stacks the stages for the context rail.
pub(crate) fn pipeline_stepper(
    status: &PipelineStatus,
    id_prefix: &'static str,
    vertical: bool,
) -> AnyElement {
    let stage_index = status.stage_index;
    let current_fill = status.current_fill();
    let completed: Vec<bool> = (0..PIPELINE_STAGES.len())
        .map(|index| status.stage_complete(index))
        .collect();
    div()
        .id(id_prefix)
        .debug_selector(move || id_prefix.to_string())
        .flex_shrink_0()
        .flex()
        .when(vertical, |stepper| stepper.flex_col().items_start())
        .when(!vertical, |stepper| stepper.items_center())
        .gap_1()
        .text_xs()
        .children(
            PIPELINE_STAGES
                .into_iter()
                .enumerate()
                .flat_map(move |(index, stage)| {
                    let current = index == stage_index;
                    let complete = completed[index];
                    let stage_id = SharedString::from(format!(
                        "{id_prefix}-stage-{}",
                        stage_label(stage).to_lowercase()
                    ));
                    let stage_selector = stage_id.clone();
                    // Done is green everywhere: a finished stage, and the last stage of a
                    // pulsation that reached Done.
                    let (fill, ink) = if complete {
                        (theme::success_soft(), theme::success())
                    } else if current {
                        (current_fill, theme::canvas())
                    } else {
                        (theme::surface_raised(), theme::text_muted())
                    };
                    let step = div()
                        .id(stage_id)
                        .debug_selector(move || stage_selector.to_string())
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size_5()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(fill)
                                .text_color(ink)
                                .when(current, |circle| {
                                    circle.font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .when(complete, |circle| {
                                    circle.child(icon(Icon::Check, px(12.0), ink))
                                })
                                .when(!complete, |circle| circle.child((index + 1).to_string())),
                        )
                        .child(
                            div()
                                .text_color(if current {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .when(current, |label| {
                                    label.font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .child(step_label(stage)),
                        );
                    let connector = (index + 1 < PIPELINE_STAGES.len()).then(|| {
                        let line = div().flex_shrink_0().bg(if complete {
                            theme::success()
                        } else {
                            theme::border()
                        });
                        if vertical {
                            line.ml(px(9.5)).w_px().h_2()
                        } else {
                            line.w_4().h_px()
                        }
                    });
                    std::iter::once(step.into_any_element())
                        .chain(connector.map(IntoElement::into_any_element))
                }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Render, TestAppContext, Window};

    #[test]
    fn display_path_drops_the_extended_length_prefix() {
        assert_eq!(display_path(r"\\?\C:\Dev\app"), r"C:\Dev\app");
        assert_eq!(
            display_path(r"\\?\UNC\server\share\app"),
            r"\\server\share\app"
        );
        assert_eq!(display_path(r"C:\Dev\app"), r"C:\Dev\app");
        assert_eq!(display_path("/home/dev/app"), "/home/dev/app");
    }

    struct WidgetsHarness(Entity<AppModel>);

    impl Render for WidgetsHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |_, cx| {
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(btn(
                        "widgets-test-enabled",
                        "Clique",
                        ButtonVariant::Primary,
                        true,
                        cx,
                        |_, _| {},
                    ))
                    .child(btn(
                        "widgets-test-disabled",
                        "Clique",
                        ButtonVariant::Primary,
                        false,
                        cx,
                        |_, _| {},
                    ))
                    .child(btn_confirm(
                        "widgets-test-confirm-idle",
                        "Delete",
                        "Confirm deletion",
                        false,
                        true,
                        ButtonVariant::Danger,
                        cx,
                        |_, _| {},
                        |_, _| {},
                    ))
                    .child(btn_confirm(
                        "widgets-test-confirm-active",
                        "Delete",
                        "Confirm deletion",
                        true,
                        true,
                        ButtonVariant::Danger,
                        cx,
                        |_, _| {},
                        |_, _| {},
                    ))
                    .child(inline_banner("Informational message", BannerKind::Info))
                    .child(loading_pill("Loading…"))
                    .child(icon_button(
                        "widgets-test-icon",
                        Icon::House,
                        "Home",
                        false,
                        None,
                        true,
                        cx,
                        |model, _, cx| model.select_view(crate::app_model::AppView::Logs, cx),
                    ))
                    .child(icon_button(
                        "widgets-test-icon-selected",
                        Icon::Settings,
                        "Settings",
                        true,
                        Some(3),
                        true,
                        cx,
                        |_, _, _| {},
                    ))
                    .child(icon_button(
                        "widgets-test-icon-disabled",
                        Icon::Play,
                        "Run",
                        false,
                        None,
                        false,
                        cx,
                        |model, _, cx| model.select_view(crate::app_model::AppView::Vault, cx),
                    ))
                    .child(badge(12, theme::accent(), theme::canvas()))
                    .child(status_dot(theme::success()))
                    .child(chip("BackendAgent", theme::accent()))
                    .child(rail_divider())
            })
        }
    }

    struct TooltipHarness(Entity<Tooltip>);

    impl Render for TooltipHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.0.clone())
        }
    }

    /// The crash `end byte index 73 is not a char boundary; it is inside '…'` came from text
    /// runs truncated for one width being reused for a wider one (HVC-GPUI-004 follow-up).
    #[gpui::test]
    fn truncating_a_label_never_leaves_its_runs_cut_for_another_width(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let text_system = cx.text_system().clone();
            let mut wrapper = text_system.line_wrapper(gpui::font("Segoe UI"), px(16.0));
            let run = |len| gpui::TextRun {
                len,
                font: gpui::font("Segoe UI"),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let text = SharedString::from("abcdefghijkl");
            let runs = vec![run(4), run(4), run(4)];
            // One layout measures a label at several widths, narrower and wider.
            for width in [px(50.0), px(90.0), px(70.0), px(500.0)] {
                let (line, line_runs) =
                    wrapper.truncate_for_layout(text.clone(), Some(width), "…", &runs);
                assert_eq!(
                    line_runs.iter().map(|run| run.len).sum::<usize>(),
                    line.len(),
                    "{width:?}: {line}"
                );
            }
            // The element's own runs still describe the whole text.
            assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), text.len());
        });
    }

    struct KeyboardHarness(Entity<AppModel>, std::rc::Rc<std::cell::Cell<usize>>);

    impl Render for KeyboardHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let presses = self.1.clone();
            self.0.update(cx, |_, cx| {
                div()
                    .p_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(btn(
                        "keyboard-button",
                        "Press",
                        ButtonVariant::Secondary,
                        true,
                        cx,
                        move |_, _| presses.set(presses.get() + 1),
                    ))
                    .child(info_tip("keyboard-first", "First explanation"))
                    .child(info_tip("keyboard-second", "Second explanation"))
            })
        }
    }

    #[gpui::test]
    fn keyboard_focus_presses_once_and_opens_tooltips_under_the_control(cx: &mut TestAppContext) {
        let presses = std::rc::Rc::new(std::cell::Cell::new(0));
        let (_, visual) = cx.add_window_view(|_, cx| {
            KeyboardHarness(cx.new(AppModel::new_for_test), presses.clone())
        });
        visual.update(|window, _| window.focus_next());
        visual.run_until_parked();
        assert!(visual.update(|window, _| window.focus_visible()));
        // Enter: key down, then key up (where GPUI clicks the focused control) — one press.
        visual.simulate_keystrokes("enter");
        visual.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        visual.run_until_parked();
        assert_eq!(presses.get(), 1);
        // The next stop is an "i": its explanation opens right under it, no pointer needed.
        visual.update(|window, _| window.focus_next());
        visual.run_until_parked();
        let tip = visual.debug_bounds("info:keyboard-first").unwrap();
        let tooltip = visual
            .debug_bounds("tooltip")
            .expect("a focused info tip shows its tooltip");
        assert!(tooltip.top() >= tip.bottom(), "{tooltip:?} vs {tip:?}");
        assert!(
            (tooltip.left() - tip.left()).abs() <= px(2.0),
            "{tooltip:?} vs {tip:?}"
        );
        // Pointing again hides the keyboard state.
        visual.simulate_mouse_down(
            tip.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        visual.run_until_parked();
        assert!(!visual.update(|window, _| window.focus_visible()));
    }

    #[gpui::test]
    fn an_explanation_tooltip_waits_for_the_pointer_to_rest(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, cx| {
            TooltipHarness(cx.new(|_| Tooltip::new("Explains an action").after_rest(EXPLAIN_REST)))
        });
        assert!(
            visual.debug_bounds("tooltip").is_none(),
            "hidden while the rest runs"
        );
        visual
            .executor()
            .advance_clock(EXPLAIN_REST - std::time::Duration::from_millis(400));
        visual.run_until_parked();
        assert!(visual.debug_bounds("tooltip").is_some());
    }

    struct StepperHarness;

    impl Render for StepperHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let status = PipelineStatus {
                stage: ikore_core::persistence_service::PulsationStage::Code,
                stage_index: 3,
                pulsation_status: ikore_core::persistence_service::PulsationStatus::Running,
                agent_name: "BackendAgent",
                state: "Running",
                tone: crate::pipeline_status::StatusTone::Active,
            };
            div()
                .flex()
                .flex_col()
                .items_start()
                .child(pipeline_stepper(&status, "stepper-h", false))
                .child(pipeline_stepper(&status, "stepper-v", true))
        }
    }

    #[gpui::test]
    fn icon_buttons_have_a_fixed_hit_area_and_only_enabled_ones_react(cx: &mut TestAppContext) {
        let (harness, visual) =
            cx.add_window_view(|_, cx| WidgetsHarness(cx.new(AppModel::new_for_test)));

        for id in [
            "widgets-test-icon",
            "widgets-test-icon-selected",
            "widgets-test-icon-disabled",
        ] {
            let bounds = visual.debug_bounds(id).unwrap();
            assert_eq!(bounds.size.width, ICON_BUTTON_SIZE, "{id}");
            assert_eq!(bounds.size.height, ICON_BUTTON_SIZE, "{id}");
        }

        let enabled = visual.debug_bounds("widgets-test-icon").unwrap();
        visual.simulate_click(enabled.center(), gpui::Modifiers::default());
        visual.run_until_parked();
        harness.read_with(visual, |harness, cx| {
            assert_eq!(
                harness.0.read(cx).active_view(),
                crate::app_model::AppView::Logs
            );
        });

        let disabled = visual.debug_bounds("widgets-test-icon-disabled").unwrap();
        visual.simulate_click(disabled.center(), gpui::Modifiers::default());
        visual.run_until_parked();
        harness.read_with(visual, |harness, cx| {
            assert_eq!(
                harness.0.read(cx).active_view(),
                crate::app_model::AppView::Logs
            );
        });
    }

    #[gpui::test]
    fn tooltip_and_stepper_render_with_real_geometry(cx: &mut TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, cx| {
            TooltipHarness(cx.new(|_| Tooltip::new("Home").with_shortcut("ctrl-1")))
        });
        let tooltip = visual.debug_bounds("tooltip").unwrap();
        assert!(tooltip.size.width > px(0.0) && tooltip.size.height > px(0.0));
        // A paragraph wraps at the tooltip's reading width instead of running off the window.
        let (_, visual) = cx.add_window_view(|_, cx| {
            TooltipHarness(cx.new(|_| {
                Tooltip::new(
                    "Only files this pulsation created and never edited since are removed. Kept files stay in the base review; adopting the base remains an explicit step.",
                )
            }))
        });
        let paragraph = visual.debug_bounds("tooltip").unwrap();
        assert!(paragraph.size.width <= px(320.0), "{paragraph:?}");
        assert!(
            paragraph.size.height > tooltip.size.height * 1.5,
            "{paragraph:?}"
        );

        let (_, visual) = cx.add_window_view(|_, _| StepperHarness);
        let horizontal = visual.debug_bounds("stepper-h").unwrap();
        let vertical = visual.debug_bounds("stepper-v").unwrap();
        assert!(horizontal.size.width > horizontal.size.height);
        assert!(vertical.size.height > vertical.size.width);
        for id in [
            "stepper-h-stage-intake",
            "stepper-h-stage-specification",
            "stepper-h-stage-contract",
            "stepper-h-stage-code",
            "stepper-h-stage-test",
            "stepper-h-stage-integrator",
            "stepper-v-stage-intake",
            "stepper-v-stage-integrator",
        ] {
            assert!(visual.debug_bounds(id).is_some(), "{id}");
        }
        let code_h = visual.debug_bounds("stepper-h-stage-code").unwrap();
        let test_h = visual.debug_bounds("stepper-h-stage-test").unwrap();
        assert!(test_h.left() > code_h.right());
        let code_v = visual.debug_bounds("stepper-v-stage-code").unwrap();
        let test_v = visual.debug_bounds("stepper-v-stage-test").unwrap();
        assert!(test_v.top() > code_v.bottom());
    }

    #[gpui::test]
    fn widgets_render_with_real_geometry_across_enabled_and_confirming_states(
        cx: &mut TestAppContext,
    ) {
        let (_, visual) =
            cx.add_window_view(|_, cx| WidgetsHarness(cx.new(AppModel::new_for_test)));

        let enabled = visual.debug_bounds("widgets-test-enabled").unwrap();
        let disabled = visual.debug_bounds("widgets-test-disabled").unwrap();
        let confirm_idle = visual.debug_bounds("widgets-test-confirm-idle").unwrap();
        let confirm_active = visual.debug_bounds("widgets-test-confirm-active").unwrap();
        let banner = visual.debug_bounds("Informational message").unwrap();
        let pill = visual.debug_bounds("Loading…").unwrap();

        // Every widget must actually mount with real layout, not an empty/zero-size element.
        for bounds in [
            enabled,
            disabled,
            confirm_idle,
            confirm_active,
            banner,
            pill,
        ] {
            assert!(bounds.size.width > px(0.0));
            assert!(bounds.size.height > px(0.0));
        }

        // "Confirm deletion" is longer than "Delete": the confirming state must actually
        // swap the rendered label, not silently keep showing the idle one.
        assert!(confirm_active.size.width > confirm_idle.size.width);
    }
}
