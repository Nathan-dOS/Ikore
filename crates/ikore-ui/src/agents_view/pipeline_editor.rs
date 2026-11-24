//! The pipeline editor (feature 25, M2a, section 4): name, description,
//! trigger (`Manual`, or an event of the Ikore pipeline from M3) and up to
//! eight steps, each an agent or a gate. Saving creates the
//! pipeline or a new version of it; a run in progress keeps its own (T4). The
//! host validates, and its errors show here, in English, by step.
//! Also the commands of a pipeline's synthesis: `Run`, `Cancel`, `Resume`,
//! a gate's decision, `Archive`, `Restore` and `Delete`.

use gpui::{AnyElement, Context, Entity, FontWeight, IntoElement, SharedString, div, prelude::*};
use ikore_core::custom_pipelines::{
    AgentStep, CUSTOM_PIPELINE_SCHEMA_VERSION, CustomPipelineDefinitionV1, CustomPipelineView,
    DecidePipelineGateRequest, DefaultEvent, GateStep, MAX_STEPS, PipelineStep, PipelineTrigger,
    PipelineValidationError, SaveCustomPipelineOutcome, SaveCustomPipelineRequest, StepAgent,
    StepOutput, StepTransport,
};
use ikore_core::terminals::{CliBackend, ThinkingLevel};

use crate::agents::CliModelPickerTarget;
use crate::app_model::AppModel;
use crate::icons::Icon;
use crate::session::HeaderNoticeKind;
use crate::text_input::NativeTextInput;
use crate::theme;

use super::{action_button, icon_action};

/// The official CLIs a step may run (E1).
const BACKENDS: [CliBackend; 5] = [
    CliBackend::Codex,
    CliBackend::Claude,
    CliBackend::Gemini,
    CliBackend::Opencode,
    CliBackend::Antigravity,
];
const EFFORTS: [ThinkingLevel; 4] = [
    ThinkingLevel::Default,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DraftKind {
    Agent,
    Gate,
}

/// One step of the form.
pub(crate) struct StepDraft {
    /// Kept across edits: the step's slot, dock tab and history follow it.
    pub(crate) id: String,
    pub(crate) kind: DraftKind,
    pub(crate) label: Entity<NativeTextInput>,
    /// The agent's instructions, or the gate's text.
    pub(crate) text: Entity<NativeTextInput>,
    pub(crate) transport: StepTransport,
    pub(crate) backend: CliBackend,
    pub(crate) model: Option<String>,
    pub(crate) model_profile: Option<String>,
    pub(crate) thinking: ThinkingLevel,
    pub(crate) route_id: Option<String>,
    pub(crate) output: StepOutput,
}

/// The form, while it is open.
pub(crate) struct PipelineEditor {
    pub(crate) pipeline_id: Option<String>,
    pub(crate) expected_version: Option<u32>,
    /// `Scope: Project | Global` on a new pipeline; fixed once it exists.
    pub(crate) scope: EditorScope,
    /// A child's parent (P1): the form then saves the changes over it.
    pub(crate) parent_pipeline_id: Option<String>,
    pub(crate) parent_label: Option<String>,
    /// What starts the pipeline (M3). A global pipeline's trigger is every
    /// project's, unless a project chose its own in the synthesis.
    pub(crate) trigger: PipelineTrigger,
    pub(crate) name: Entity<NativeTextInput>,
    pub(crate) description: Entity<NativeTextInput>,
    pub(crate) steps: Vec<StepDraft>,
    pub(crate) errors: Vec<PipelineValidationError>,
    pub(crate) saving: bool,
}

/// Where the form's pipeline lives (feature 25, M2b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorScope {
    /// A new pipeline: the developer picks.
    Choose { global: bool },
    /// A new child of a project's pipeline: it stays in the project (P3).
    ProjectOnly,
    /// An existing pipeline keeps its scope.
    Fixed { global: bool },
}

impl EditorScope {
    pub(crate) fn global(self) -> bool {
        match self {
            Self::Choose { global } | Self::Fixed { global } => global,
            Self::ProjectOnly => false,
        }
    }
}

fn input(
    placeholder: &str,
    value: &str,
    multiline: bool,
    cx: &mut Context<AppModel>,
) -> Entity<NativeTextInput> {
    let placeholder = placeholder.to_owned();
    let value = value.to_owned();
    let input = cx.new(move |cx| {
        let mut input = NativeTextInput::new(placeholder, cx);
        if multiline {
            input = input.multiline().max_rows(8);
        }
        input.set_value(value, cx);
        input
    });
    cx.observe(&input, |_, _, cx| cx.notify()).detach();
    input
}

impl PipelineEditor {
    fn next_id(&self) -> String {
        (1..)
            .map(|index| format!("step-{index}"))
            .find(|id| self.steps.iter().all(|step| &step.id != id))
            .unwrap_or_else(|| "step".into())
    }

    /// The definition the form describes.
    fn definition(&self, cx: &gpui::App) -> CustomPipelineDefinitionV1 {
        CustomPipelineDefinitionV1 {
            schema_version: CUSTOM_PIPELINE_SCHEMA_VERSION,
            trigger: self.trigger.clone(),
            steps: self
                .steps
                .iter()
                .map(|draft| {
                    let label = draft.label.read(cx).value().trim().to_owned();
                    let text = draft.text.read(cx).value().to_owned();
                    match draft.kind {
                        DraftKind::Gate => PipelineStep::Gate(GateStep {
                            id: draft.id.clone(),
                            label,
                            prompt: text,
                        }),
                        DraftKind::Agent => PipelineStep::Agent(AgentStep {
                            id: draft.id.clone(),
                            label,
                            agent: StepAgent {
                                transport: draft.transport,
                                backend: (draft.transport == StepTransport::Cli)
                                    .then(|| draft.backend.as_str().to_owned()),
                                model: (draft.transport == StepTransport::Cli)
                                    .then(|| draft.model.clone())
                                    .flatten(),
                                model_profile: (draft.transport == StepTransport::Cli)
                                    .then(|| draft.model_profile.clone())
                                    .flatten(),
                                thinking_level: draft.thinking.as_str().to_owned(),
                                api_route_id: (draft.transport == StepTransport::Api)
                                    .then(|| draft.route_id.clone())
                                    .flatten(),
                            },
                            instructions: text,
                            output: draft.output,
                        }),
                    }
                })
                .collect(),
        }
    }
}

/// The model a step on this CLI starts from: the configured one, else the
/// catalog's first fixed model.
fn starting_model(catalog: &ikore_core::terminals::CliModelCatalogSnapshot) -> Option<String> {
    catalog
        .models
        .iter()
        .find_map(|entry| {
            entry
                .raw_metadata
                .get("configuredModel")
                .and_then(|model| model.as_str())
                .map(str::to_owned)
        })
        .or_else(|| {
            catalog
                .models
                .iter()
                .find_map(|entry| entry.model_id.clone())
        })
}

/// A small choice in a row of choices.
fn choice(
    id: String,
    label: impl Into<SharedString>,
    active: bool,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> AnyElement {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .px_2()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(if active {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if active {
            theme::accent_soft()
        } else {
            theme::surface()
        })
        .text_xs()
        .opacity(if enabled { 1.0 } else { 0.5 })
        .when(enabled, |chip| {
            chip.cursor_pointer()
                .hover(|style| style.bg(theme::hover_bg()))
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(label.into())
        .into_any_element()
}

fn label(text: &'static str) -> AnyElement {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .child(text)
        .into_any_element()
}

fn errors_box(errors: Vec<String>, selector: String) -> Option<AnyElement> {
    (!errors.is_empty()).then(|| {
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(theme::danger_soft())
            .children(
                errors
                    .into_iter()
                    .map(|error| div().text_xs().text_color(theme::danger()).child(error)),
            )
            .into_any_element()
    })
}

fn backend_name(backend: CliBackend) -> &'static str {
    match backend {
        CliBackend::Codex => "Codex",
        CliBackend::Claude => "Claude",
        CliBackend::Gemini => "Gemini",
        CliBackend::Opencode => "OpenCode",
        CliBackend::Antigravity => "Antigravity",
        CliBackend::Custom => "Custom",
    }
}

fn effort_name(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Default => "Default",
        ThinkingLevel::Low => "Low",
        ThinkingLevel::Medium => "Medium",
        ThinkingLevel::High => "High",
    }
}

fn step_card(
    model: &AppModel,
    editor: &PipelineEditor,
    index: usize,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let draft = &editor.steps[index];
    let count = editor.steps.len();
    let routes = model
        .settings
        .read(cx)
        .snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .routes
                .iter()
                .filter(|route| route.enabled)
                .map(|route| (route.id.clone(), route.name.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let errors = editor
        .errors
        .iter()
        .filter(|error| error.step_id.as_deref() == Some(draft.id.as_str()))
        .map(|error| error.message.clone())
        .collect::<Vec<_>>();
    let selector = format!("pipeline-editor-step:{index}");
    let mut card = div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!(
                            "{}. {}",
                            index + 1,
                            match draft.kind {
                                DraftKind::Agent => "Agent",
                                DraftKind::Gate => "Gate",
                            }
                        )),
                )
                .child(icon_action(
                    format!("pipeline-editor-up:{index}").into(),
                    Icon::ChevronUp,
                    "Move up",
                    index > 0,
                    cx,
                    move |model, cx| model.move_pipeline_step(index, -1, cx),
                ))
                .child(icon_action(
                    format!("pipeline-editor-down:{index}").into(),
                    Icon::ChevronDown,
                    "Move down",
                    index + 1 < count,
                    cx,
                    move |model, cx| model.move_pipeline_step(index, 1, cx),
                ))
                .child(icon_action(
                    format!("pipeline-editor-remove:{index}").into(),
                    Icon::X,
                    "Remove step",
                    count > 1,
                    cx,
                    move |model, cx| model.remove_pipeline_step(index, cx),
                )),
        )
        .children(errors_box(
            errors,
            format!("pipeline-editor-step-errors:{index}"),
        ))
        .child(super::config::field(
            "LABEL",
            draft.label.clone(),
            crate::sizing::INPUT_MEDIUM,
        ));
    match draft.kind {
        DraftKind::Gate => {
            card = card
                .child(label("TEXT"))
                .child(div().w_full().child(draft.text.clone()));
        }
        DraftKind::Agent => {
            let transport = draft.transport;
            card = card.child(label("TRANSPORT")).child(
                div()
                    .flex()
                    .gap_1()
                    .child(choice(
                        format!("pipeline-editor-cli:{index}"),
                        "CLI",
                        transport == StepTransport::Cli,
                        true,
                        cx,
                        move |model, cx| {
                            model.set_pipeline_step_transport(index, StepTransport::Cli, cx)
                        },
                    ))
                    .child(choice(
                        format!("pipeline-editor-api:{index}"),
                        "API",
                        transport == StepTransport::Api,
                        true,
                        cx,
                        move |model, cx| {
                            model.set_pipeline_step_transport(index, StepTransport::Api, cx)
                        },
                    )),
            );
            if transport == StepTransport::Cli {
                let current = draft.backend;
                let mut backends = div().flex().flex_wrap().gap_1();
                for backend in BACKENDS {
                    backends = backends.child(choice(
                        format!("pipeline-editor-backend:{index}:{}", backend.as_str()),
                        backend_name(backend),
                        backend == current,
                        true,
                        cx,
                        move |model, cx| model.set_pipeline_step_backend(index, backend, cx),
                    ));
                }
                let mut efforts = div().flex().flex_wrap().gap_1();
                for level in EFFORTS {
                    efforts = efforts.child(choice(
                        format!("pipeline-editor-effort:{index}:{}", level.as_str()),
                        effort_name(level),
                        draft.thinking == level,
                        true,
                        cx,
                        move |model, cx| model.set_pipeline_step_effort(index, level, cx),
                    ));
                }
                card = card
                    .child(label("CLI"))
                    .child(backends)
                    .child(super::config::config_button(
                        format!("pipeline-editor-model:{index}"),
                        "MODEL",
                        draft
                            .model
                            .clone()
                            .unwrap_or_else(|| "Choose a model".into()),
                        true,
                        cx,
                        move |model, cx| model.open_pipeline_step_model_picker(index, cx),
                    ))
                    .child(label("EFFORT"))
                    .child(efforts);
            } else {
                let current = draft.route_id.clone();
                let mut row = div().flex().flex_wrap().gap_1();
                if routes.is_empty() {
                    row = row.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child("No API route yet. Add one in Settings › Routes."),
                    );
                }
                for (route_id, name) in routes {
                    let active = current.as_deref() == Some(route_id.as_str());
                    row = row.child(choice(
                        format!("pipeline-editor-route:{index}:{route_id}"),
                        name,
                        active,
                        true,
                        cx,
                        move |model, cx| model.set_pipeline_step_route(index, route_id.clone(), cx),
                    ));
                }
                card = card.child(label("ROUTE")).child(row);
            }
            let output = draft.output;
            card = card
                .child(label("OUTPUT"))
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(choice(
                            format!("pipeline-editor-text:{index}"),
                            "Text",
                            output == StepOutput::Text,
                            true,
                            cx,
                            move |model, cx| {
                                model.set_pipeline_step_output(index, StepOutput::Text, cx)
                            },
                        ))
                        .child(choice(
                            format!("pipeline-editor-verdict:{index}"),
                            "Verdict",
                            output == StepOutput::Verdict,
                            true,
                            cx,
                            move |model, cx| {
                                model.set_pipeline_step_output(index, StepOutput::Verdict, cx)
                            },
                        )),
                )
                .child(label("INSTRUCTIONS"))
                .child(div().w_full().child(draft.text.clone()));
        }
    }
    card.into_any_element()
}

/// `Based on {parent} v{n}`: the parent's version this project's box resolves over.
pub(crate) fn parent_label(pipeline: &CustomPipelineView) -> Option<String> {
    pipeline
        .parent_name
        .as_ref()
        .map(|name| match pipeline.adopted_parent_version {
            Some(version) => format!("{name} v{version}"),
            None => name.clone(),
        })
}

/// `Project | Global`: a choice on a new pipeline, a line otherwise.
fn scope_row(scope: EditorScope, cx: &mut Context<AppModel>) -> AnyElement {
    let note = |text: &'static str| {
        div()
            .debug_selector(|| "pipeline-editor-scope".into())
            .text_xs()
            .text_color(theme::text_muted())
            .child(text)
            .into_any_element()
    };
    match scope {
        EditorScope::Choose { global } => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .debug_selector(|| "pipeline-editor-scope".into())
                    .flex()
                    .gap_2()
                    .child(choice(
                        "pipeline-editor-scope-project".into(),
                        "Project",
                        !global,
                        true,
                        cx,
                        |model, cx| model.set_pipeline_editor_scope(false, cx),
                    ))
                    .child(choice(
                        "pipeline-editor-scope-global".into(),
                        "Global",
                        global,
                        true,
                        cx,
                        |model, cx| model.set_pipeline_editor_scope(true, cx),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(if global {
                        "Any project can add it. A project takes a new version when it opens."
                    } else {
                        "Only this project shows it."
                    }),
            )
            .into_any_element(),
        EditorScope::ProjectOnly => {
            note("Project \u{b7} a child of a project's pipeline stays in its project")
        }
        EditorScope::Fixed { global: true } => {
            note("Global \u{b7} other projects take this version when they open")
        }
        EditorScope::Fixed { global: false } => note("Project"),
    }
}

/// The stages a pipeline can follow; an approved Integrator is `Pulsation
/// done`, never a passed stage.
pub(crate) const TRIGGER_STAGES: [&str; 5] =
    ["Intake", "SPECIFICATION", "Contract", "Code", "Test"];

pub(crate) fn stage_passed(stage: &str) -> PipelineTrigger {
    PipelineTrigger::DefaultEvent {
        event: DefaultEvent::StagePassed,
        stage: Some(stage.to_owned()),
    }
}

pub(crate) fn pulsation_done() -> PipelineTrigger {
    PipelineTrigger::DefaultEvent {
        event: DefaultEvent::PulsationDone,
        stage: None,
    }
}

/// What a trigger does, for the line under its choices.
pub(crate) fn trigger_help(trigger: &PipelineTrigger) -> String {
    match trigger {
        PipelineTrigger::Manual => "Run on the pipeline's box starts it.".into(),
        PipelineTrigger::DefaultEvent {
            event: DefaultEvent::PulsationDone,
            ..
        } => "Starts once when a pulsation is done, after the Integrator's approval.".into(),
        PipelineTrigger::DefaultEvent { stage, .. } => format!(
            "Starts each time a pulsation passes {}, once its files are written. The Ikore pipeline does not wait for it.",
            ikore_core::custom_pipelines::trigger_stage_label(stage.as_deref().unwrap_or_default())
        ),
    }
}

/// `Manual | Stage passed | Pulsation done`, and the stage when one passes.
/// The editor's trigger, and a project's own one in the synthesis (R4b).
pub(crate) fn trigger_rows(
    prefix: &'static str,
    trigger: &PipelineTrigger,
    global: bool,
    cx: &mut Context<AppModel>,
    set: impl Fn(&mut AppModel, PipelineTrigger, &mut Context<AppModel>) + Clone + 'static,
) -> AnyElement {
    let stage = match trigger {
        PipelineTrigger::DefaultEvent {
            event: DefaultEvent::StagePassed,
            stage,
        } => Some(stage.clone().unwrap_or_default()),
        _ => None,
    };
    let done = matches!(
        trigger,
        PipelineTrigger::DefaultEvent {
            event: DefaultEvent::PulsationDone,
            ..
        }
    );
    let mut kinds = div().flex().flex_wrap().gap_2();
    for (id, text, active, next) in [
        (
            "manual",
            "Manual",
            matches!(trigger, PipelineTrigger::Manual),
            PipelineTrigger::Manual,
        ),
        (
            "stage-passed",
            "Stage passed",
            stage.is_some(),
            stage_passed(stage.as_deref().unwrap_or("Code")),
        ),
        ("pulsation-done", "Pulsation done", done, pulsation_done()),
    ] {
        let set = set.clone();
        kinds = kinds.child(choice(
            format!("{prefix}-{id}"),
            text,
            active,
            true,
            cx,
            move |model, cx| set(model, next.clone(), cx),
        ));
    }
    let stages = stage.as_ref().map(|current| {
        let mut row = div().flex().flex_wrap().gap_2();
        for value in TRIGGER_STAGES {
            let set = set.clone();
            row = row.child(choice(
                format!("{prefix}-stage:{value}"),
                ikore_core::custom_pipelines::trigger_stage_label(value).to_owned(),
                current == value,
                true,
                cx,
                move |model, cx| set(model, stage_passed(value), cx),
            ));
        }
        row
    });
    let help = format!(
        "{}{}",
        trigger_help(trigger),
        if global && !matches!(trigger, PipelineTrigger::Manual) {
            " Each project that uses it follows this trigger unless it chose its own."
        } else {
            ""
        }
    );
    div()
        .debug_selector(move || prefix.into())
        .flex()
        .flex_col()
        .gap_2()
        .child(kinds)
        .children(stages)
        .child(div().text_xs().text_color(theme::text_muted()).child(help))
        .into_any_element()
}

/// The form in the synthesis column.
pub(super) fn pipeline_editor_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some(editor) = model.pipeline_editor.as_ref() else {
        return div().into_any_element();
    };
    let general = editor
        .errors
        .iter()
        .filter(|error| error.step_id.is_none())
        .map(|error| error.message.clone())
        .collect::<Vec<_>>();
    let title = format!(
        "{}{}",
        match (&editor.pipeline_id, &editor.parent_pipeline_id) {
            (Some(_), _) => "Edit pipeline",
            (None, Some(_)) => "New child pipeline",
            (None, None) => "New pipeline",
        },
        if editor.scope.global() {
            " \u{b7} Global"
        } else {
            ""
        }
    );
    let scope = editor.scope;
    let parent = editor.parent_label.clone();
    let full = editor.steps.len() >= MAX_STEPS;
    let saving = editor.saving;
    let cards = (0..editor.steps.len())
        .map(|index| step_card(model, editor, index, cx))
        .collect::<Vec<_>>();
    div()
        .id("agents-pipeline-editor")
        .debug_selector(|| "agents-pipeline-editor".into())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(icon_action(
                    "pipeline-editor-close".into(),
                    Icon::X,
                    "Discard",
                    true,
                    cx,
                    |model, cx| model.close_pipeline_editor(cx),
                )),
        )
        .children(errors_box(general, "pipeline-editor-errors".into()))
        .child(super::config::field(
            "NAME",
            editor.name.clone(),
            crate::sizing::INPUT_MEDIUM,
        ))
        .child(super::config::field(
            "DESCRIPTION",
            editor.description.clone(),
            crate::sizing::INPUT_LONG,
        ))
        .child(label("SCOPE"))
        .child(scope_row(scope, cx))
        .when_some(parent, |panel, parent| {
            panel.child(
                div()
                    .debug_selector(|| "pipeline-editor-parent".into())
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "Based on {parent} \u{b7} what you change here stays over its new versions"
                    )),
            )
        })
        .child(label("TRIGGER"))
        .child(trigger_rows(
            "pipeline-editor-trigger",
            &editor.trigger,
            scope.global(),
            cx,
            |model, trigger, cx| model.set_pipeline_editor_trigger(trigger, cx),
        ))
        .child(label("STEPS"))
        .children(cards)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(action_button(
                    "pipeline-editor-add-agent".into(),
                    "Agent".into(),
                    !full,
                    false,
                    cx,
                    |model, cx| model.add_pipeline_step(DraftKind::Agent, cx),
                ))
                .child(action_button(
                    "pipeline-editor-add-gate".into(),
                    "Gate".into(),
                    !full,
                    false,
                    cx,
                    |model, cx| model.add_pipeline_step(DraftKind::Gate, cx),
                ))
                .child(
                    div()
                        .id("pipeline-editor-add-host-tool")
                        .debug_selector(|| "pipeline-editor-add-host-tool".into())
                        .tooltip(|_, cx| {
                            cx.new(|_| {
                                crate::widgets::Tooltip::new(
                                    "Available after the commit tool ships",
                                )
                            })
                            .into()
                        })
                        .child(action_button(
                            "pipeline-editor-host-tool".into(),
                            "Host tool".into(),
                            false,
                            false,
                            cx,
                            |_, _| {},
                        )),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .pt_2()
                .child(action_button(
                    "pipeline-editor-save".into(),
                    if saving { "Saving…" } else { "Save" }.into(),
                    !saving,
                    false,
                    cx,
                    |model, cx| model.save_pipeline_editor(cx),
                ))
                .child(action_button(
                    "pipeline-editor-cancel".into(),
                    "Cancel".into(),
                    true,
                    false,
                    cx,
                    |model, cx| model.close_pipeline_editor(cx),
                )),
        )
        .into_any_element()
}

impl AppModel {
    fn pipelines_project(&self, cx: &gpui::App) -> Option<String> {
        self.workspace
            .read(cx)
            .project_id
            .as_ref()
            .map(ToString::to_string)
    }

    /// The editor's starting model (plan P9): the one the CLI is configured
    /// with, else the catalog's first fixed model. Without a loaded catalog,
    /// the catalog is read and the step's model is filled when it arrives.
    fn catalog_first_model(
        &mut self,
        backend: CliBackend,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let first = self
            .agents
            .read(cx)
            .model_catalogs
            .get(&backend)
            .and_then(starting_model);
        if first.is_none() && !self.agents.read(cx).model_catalogs.contains_key(&backend) {
            self.load_cli_model_catalog(backend, false, cx);
        }
        first
    }

    /// A catalog arrived: the form's steps on that CLI with no model yet
    /// start from it.
    pub(crate) fn fill_pipeline_step_models(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.pipeline_editor.as_ref() else {
            return;
        };
        let backends = editor
            .steps
            .iter()
            .filter(|step| step.kind == DraftKind::Agent && step.model.is_none())
            .map(|step| step.backend)
            .collect::<Vec<_>>();
        let models = backends
            .into_iter()
            .filter_map(|backend| {
                self.agents
                    .read(cx)
                    .model_catalogs
                    .get(&backend)
                    .and_then(starting_model)
                    .map(|model| (backend, model))
            })
            .collect::<Vec<_>>();
        if let Some(editor) = self.pipeline_editor.as_mut() {
            for step in &mut editor.steps {
                if step.kind == DraftKind::Agent
                    && step.model.is_none()
                    && let Some((_, model)) =
                        models.iter().find(|(backend, _)| *backend == step.backend)
                {
                    step.model = Some(model.clone());
                }
            }
        }
    }

    fn new_step_draft(&mut self, id: String, kind: DraftKind, cx: &mut Context<Self>) -> StepDraft {
        let backend = self
            .agents
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot.discovery.iter().find(|item| {
                    BACKENDS.contains(&item.backend)
                        && item.state == ikore_core::terminals::CliAvailabilityState::Available
                })
            })
            .map_or(CliBackend::Codex, |item| item.backend);
        let model = (kind == DraftKind::Agent)
            .then(|| self.catalog_first_model(backend, cx))
            .flatten();
        StepDraft {
            label: input(
                "Step label",
                match kind {
                    DraftKind::Agent => "Review",
                    DraftKind::Gate => "Approve",
                },
                false,
                cx,
            ),
            text: input(
                match kind {
                    DraftKind::Agent => "What the agent should do",
                    DraftKind::Gate => "What the developer should check",
                },
                "",
                true,
                cx,
            ),
            id,
            kind,
            transport: StepTransport::Cli,
            backend,
            model,
            model_profile: None,
            thinking: ThinkingLevel::Default,
            route_id: None,
            output: StepOutput::Text,
        }
    }

    fn open_editor_column(&mut self, cx: &mut Context<Self>) {
        if self.settings.read(cx).snapshot.is_none() {
            self.refresh_settings(cx);
        }
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.show_archived = false;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = false;
            agents.active_model_picker = None;
        });
    }

    /// `New pipeline`: an empty form with one agent step.
    pub(crate) fn open_new_pipeline(&mut self, cx: &mut Context<Self>) {
        self.open_editor_column(cx);
        let first = self.new_step_draft("step-1".into(), DraftKind::Agent, cx);
        self.pipeline_editor = Some(PipelineEditor {
            pipeline_id: None,
            expected_version: None,
            scope: EditorScope::Choose { global: false },
            parent_pipeline_id: None,
            parent_label: None,
            trigger: PipelineTrigger::Manual,
            name: input("Pipeline name", "", false, cx),
            description: input("What the pipeline is for (optional)", "", false, cx),
            steps: vec![first],
            errors: Vec::new(),
            saving: false,
        });
        cx.notify();
    }

    /// The form's steps for a pipeline's definition.
    fn drafts_of(
        &mut self,
        pipeline: &CustomPipelineView,
        cx: &mut Context<Self>,
    ) -> Vec<StepDraft> {
        pipeline
            .definition
            .steps
            .iter()
            .filter_map(|step| match step {
                PipelineStep::Agent(agent) => Some(StepDraft {
                    id: agent.id.clone(),
                    kind: DraftKind::Agent,
                    label: input("Step label", &agent.label, false, cx),
                    text: input("What the agent should do", &agent.instructions, true, cx),
                    transport: agent.agent.transport,
                    backend: agent
                        .agent
                        .backend
                        .as_deref()
                        .and_then(CliBackend::parse)
                        .unwrap_or(CliBackend::Codex),
                    model: agent.agent.model.clone(),
                    model_profile: agent.agent.model_profile.clone(),
                    thinking: ThinkingLevel::parse(&agent.agent.thinking_level)
                        .unwrap_or(ThinkingLevel::Default),
                    route_id: agent.agent.api_route_id.clone(),
                    output: agent.output,
                }),
                PipelineStep::Gate(gate) => Some(StepDraft {
                    id: gate.id.clone(),
                    kind: DraftKind::Gate,
                    label: input("Step label", &gate.label, false, cx),
                    text: input("What the developer should check", &gate.prompt, true, cx),
                    transport: StepTransport::Cli,
                    backend: CliBackend::Codex,
                    model: None,
                    model_profile: None,
                    thinking: ThinkingLevel::Default,
                    route_id: None,
                    output: StepOutput::Text,
                }),
                // Not offered until M5.
                PipelineStep::HostTool(_) => None,
            })
            .collect()
    }

    /// `Edit`: the form on what the project's box resolves to.
    pub(crate) fn edit_pipeline(&mut self, pipeline_id: String, cx: &mut Context<Self>) {
        let Some(pipeline) = self.agents.read(cx).pipeline(&pipeline_id).cloned() else {
            return;
        };
        self.open_editor_column(cx);
        let steps = self.drafts_of(&pipeline, cx);
        self.pipeline_editor = Some(PipelineEditor {
            pipeline_id: Some(pipeline.id.clone()),
            expected_version: Some(pipeline.current_version),
            scope: EditorScope::Fixed {
                global: pipeline.is_global(),
            },
            parent_pipeline_id: pipeline.parent_pipeline_id.clone(),
            parent_label: parent_label(&pipeline),
            trigger: pipeline.definition.trigger.clone(),
            name: input("Pipeline name", &pipeline.name, false, cx),
            description: input(
                "What the pipeline is for (optional)",
                &pipeline.description,
                false,
                cx,
            ),
            steps,
            errors: Vec::new(),
            saving: false,
        });
        cx.notify();
    }

    /// `Create child`: a new pipeline based on this one (P1), with its steps.
    pub(crate) fn create_child_pipeline(&mut self, parent_id: String, cx: &mut Context<Self>) {
        let Some(parent) = self.agents.read(cx).pipeline(&parent_id).cloned() else {
            return;
        };
        self.open_editor_column(cx);
        let steps = self.drafts_of(&parent, cx);
        self.pipeline_editor = Some(PipelineEditor {
            pipeline_id: None,
            expected_version: None,
            scope: if parent.is_global() {
                EditorScope::Choose { global: false }
            } else {
                EditorScope::ProjectOnly
            },
            parent_pipeline_id: Some(parent.id.clone()),
            parent_label: Some(format!("{} v{}", parent.name, parent.adopted_version)),
            trigger: parent.definition.trigger.clone(),
            name: input("Pipeline name", "", false, cx),
            description: input("What the pipeline is for (optional)", "", false, cx),
            steps,
            errors: Vec::new(),
            saving: false,
        });
        cx.notify();
    }

    /// `Scope: Project | Global` on a new pipeline.
    pub(crate) fn set_pipeline_editor_scope(&mut self, global: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.pipeline_editor.as_mut()
            && matches!(editor.scope, EditorScope::Choose { .. })
        {
            editor.scope = EditorScope::Choose { global };
        }
        cx.notify();
    }

    /// The form's trigger (M3).
    pub(crate) fn set_pipeline_editor_trigger(
        &mut self,
        trigger: PipelineTrigger,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.pipeline_editor.as_mut() {
            editor.trigger = trigger;
        }
        cx.notify();
    }

    pub(crate) fn close_pipeline_editor(&mut self, cx: &mut Context<Self>) {
        let reopen = self
            .pipeline_editor
            .take()
            .and_then(|editor| editor.pipeline_id);
        if let Some(id) = reopen {
            self.select_canvas_pipeline(id, cx);
        }
        cx.notify();
    }

    pub(crate) fn add_pipeline_step(&mut self, kind: DraftKind, cx: &mut Context<Self>) {
        let Some(id) = self.pipeline_editor.as_ref().map(PipelineEditor::next_id) else {
            return;
        };
        let draft = self.new_step_draft(id, kind, cx);
        if let Some(editor) = self.pipeline_editor.as_mut()
            && editor.steps.len() < MAX_STEPS
        {
            editor.steps.push(draft);
        }
        cx.notify();
    }

    pub(crate) fn move_pipeline_step(&mut self, index: usize, by: isize, cx: &mut Context<Self>) {
        if let Some(editor) = self.pipeline_editor.as_mut() {
            let target = index as isize + by;
            if target >= 0 && (target as usize) < editor.steps.len() {
                editor.steps.swap(index, target as usize);
            }
        }
        cx.notify();
    }

    pub(crate) fn remove_pipeline_step(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.pipeline_editor.as_mut()
            && editor.steps.len() > 1
            && index < editor.steps.len()
        {
            editor.steps.remove(index);
        }
        cx.notify();
    }

    fn with_step(&mut self, index: usize, change: impl FnOnce(&mut StepDraft)) {
        if let Some(step) = self
            .pipeline_editor
            .as_mut()
            .and_then(|editor| editor.steps.get_mut(index))
        {
            change(step);
        }
    }

    pub(crate) fn set_pipeline_step_transport(
        &mut self,
        index: usize,
        transport: StepTransport,
        cx: &mut Context<Self>,
    ) {
        self.with_step(index, |step| step.transport = transport);
        cx.notify();
    }

    pub(crate) fn set_pipeline_step_backend(
        &mut self,
        index: usize,
        backend: CliBackend,
        cx: &mut Context<Self>,
    ) {
        let unchanged = self
            .pipeline_editor
            .as_ref()
            .and_then(|editor| editor.steps.get(index))
            .is_some_and(|step| step.backend == backend);
        if !unchanged {
            // A model belongs to its CLI: the catalog's first one stands in.
            let model = self.catalog_first_model(backend, cx);
            self.with_step(index, |step| {
                step.backend = backend;
                step.model = model;
                step.model_profile = None;
                step.thinking = ThinkingLevel::Default;
            });
        }
        cx.notify();
    }

    pub(crate) fn set_pipeline_step_effort(
        &mut self,
        index: usize,
        level: ThinkingLevel,
        cx: &mut Context<Self>,
    ) {
        self.with_step(index, |step| step.thinking = level);
        cx.notify();
    }

    pub(crate) fn set_pipeline_step_route(
        &mut self,
        index: usize,
        route_id: String,
        cx: &mut Context<Self>,
    ) {
        self.with_step(index, |step| step.route_id = Some(route_id));
        cx.notify();
    }

    pub(crate) fn set_pipeline_step_output(
        &mut self,
        index: usize,
        output: StepOutput,
        cx: &mut Context<Self>,
    ) {
        self.with_step(index, |step| step.output = output);
        cx.notify();
    }

    pub(crate) fn open_pipeline_step_model_picker(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(backend) = self
            .pipeline_editor
            .as_ref()
            .and_then(|editor| editor.steps.get(index))
            .map(|step| step.backend)
        else {
            return;
        };
        self.open_cli_model_picker(CliModelPickerTarget::PipelineStep(index), backend, cx);
    }

    /// The model picker chose a model for a step of the form.
    pub(crate) fn set_pipeline_step_model(
        &mut self,
        index: usize,
        model_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.with_step(index, |step| {
            step.model = model_id;
            step.model_profile = None;
        });
        cx.notify();
    }

    /// `Save`: the host validates; its errors stay on the form.
    pub(crate) fn save_pipeline_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(backend), Some(project_id)) =
            (self.agents_backend.clone(), self.pipelines_project(cx))
        else {
            return;
        };
        let Some(editor) = self.pipeline_editor.as_mut() else {
            return;
        };
        if editor.saving {
            return;
        }
        editor.saving = true;
        let request = SaveCustomPipelineRequest {
            project_id,
            pipeline_id: editor.pipeline_id.clone(),
            expected_version: editor.expected_version,
            scope: editor.pipeline_id.is_none().then(|| {
                if editor.scope.global() {
                    "global".to_owned()
                } else {
                    "project".to_owned()
                }
            }),
            parent_pipeline_id: editor
                .pipeline_id
                .is_none()
                .then(|| editor.parent_pipeline_id.clone())
                .flatten(),
            name: editor.name.read(cx).value().to_owned(),
            description: editor.description.read(cx).value().to_owned(),
            definition: editor.definition(cx),
        };
        cx.notify();
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.save_pipeline(request).await;
            let _ = this.update(cx, |model, cx| {
                if let Some(editor) = model.pipeline_editor.as_mut() {
                    editor.saving = false;
                }
                match result {
                    Ok(SaveCustomPipelineOutcome::Saved { pipeline }) => {
                        model.pipeline_editor = None;
                        model.refresh_agents(cx);
                        model.select_canvas_pipeline(pipeline.id, cx);
                    }
                    Ok(SaveCustomPipelineOutcome::Invalid { errors }) => {
                        if let Some(editor) = model.pipeline_editor.as_mut() {
                            editor.errors = errors;
                        }
                    }
                    Err(error) => {
                        model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                    }
                }
                cx.notify();
            });
        }));
    }

    /// A pipeline command, then a fresh snapshot; errors go to the header.
    fn pipeline_command(
        &mut self,
        command: impl FnOnce(
            std::sync::Arc<dyn crate::agents::AgentsBackend>,
            String,
        ) -> crate::agents::AgentsFuture<()>
        + 'static,
        cx: &mut Context<Self>,
    ) {
        let (Some(backend), Some(project_id)) =
            (self.agents_backend.clone(), self.pipelines_project(cx))
        else {
            return;
        };
        let future = command(backend, project_id);
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = future.await;
            let _ = this.update(cx, |model, cx| {
                if let Err(error) = result {
                    model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                }
                model.refresh_agents(cx);
                cx.notify();
            });
        }));
    }

    /// `Run` on a pipeline's synthesis.
    pub(crate) fn start_pipeline_run(&mut self, pipeline_id: String, cx: &mut Context<Self>) {
        self.agents
            .update(cx, |agents, _| agents.selected_pipeline_run = None);
        self.pipeline_command(
            move |backend, project| {
                Box::pin(async move {
                    backend
                        .start_pipeline_run(project, pipeline_id)
                        .await
                        .map(|_| ())
                })
            },
            cx,
        );
    }

    pub(crate) fn cancel_pipeline_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.pipeline_command(
            move |backend, project| backend.cancel_pipeline_run(project, run_id),
            cx,
        );
    }

    pub(crate) fn resume_pipeline_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.pipeline_command(
            move |backend, project| backend.resume_pipeline_run(project, run_id),
            cx,
        );
    }

    /// A gate's `Approve` or `Reject`, with the note typed under it.
    pub(crate) fn decide_pipeline_gate(
        &mut self,
        run_id: String,
        step_index: u32,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        let note = self
            .pipeline_gate_note_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        self.pipeline_gate_note_input
            .update(cx, |input, cx| input.clear(cx));
        self.pipeline_command(
            move |backend, project_id| {
                backend.decide_pipeline_gate(DecidePipelineGateRequest {
                    project_id,
                    run_id,
                    step_index,
                    approve,
                    note: (!note.is_empty()).then_some(note),
                })
            },
            cx,
        );
    }

    pub(crate) fn set_pipeline_archived(
        &mut self,
        pipeline_id: String,
        archived: bool,
        cx: &mut Context<Self>,
    ) {
        if archived {
            self.agents.update(cx, |agents, _| {
                agents.selected_pipeline = None;
                agents.selected_pipeline_run = None;
            });
        }
        self.pipeline_command(
            move |backend, project| {
                Box::pin(async move {
                    match backend
                        .set_pipeline_archived(project, pipeline_id, archived)
                        .await?
                    {
                        SaveCustomPipelineOutcome::Saved { .. } => Ok(()),
                        SaveCustomPipelineOutcome::Invalid { errors } => {
                            Err(crate::errors::UiCommandError::from_chat_parts(
                                "pipeline_refused",
                                errors
                                    .into_iter()
                                    .map(|error| error.message)
                                    .collect::<Vec<_>>()
                                    .join(" "),
                            ))
                        }
                    }
                })
            },
            cx,
        );
    }

    /// `Delete`, then `Confirm delete`: only a pipeline that never ran.
    pub(crate) fn delete_pipeline(&mut self, pipeline_id: String, cx: &mut Context<Self>) {
        let confirmed = self.agents.read(cx).pending_pipeline_delete.as_ref() == Some(&pipeline_id);
        if !confirmed {
            self.agents.update(cx, |agents, _| {
                agents.pending_pipeline_delete = Some(pipeline_id);
            });
            cx.notify();
            return;
        }
        self.agents.update(cx, |agents, _| {
            agents.pending_pipeline_delete = None;
            if agents.selected_pipeline.as_ref() == Some(&pipeline_id) {
                agents.selected_pipeline = None;
            }
        });
        self.pipeline_command(
            move |backend, project| backend.delete_pipeline(project, pipeline_id),
            cx,
        );
    }

    /// `Archived pipelines · N` in the header.
    pub(crate) fn open_archived_pipelines(&mut self, cx: &mut Context<Self>) {
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.show_archived = false;
            agents.show_global_pipelines = false;
            agents.show_archived_pipelines = true;
        });
        cx.notify();
    }

    /// `Add global pipeline…` in the header: the global pipelines this
    /// project does not show yet.
    pub(crate) fn open_global_pipelines(&mut self, cx: &mut Context<Self>) {
        self.pipeline_editor = None;
        self.agents.update(cx, |agents, _| {
            agents.selected_agent = None;
            agents.selected_pipeline = None;
            agents.show_archived = false;
            agents.show_archived_pipelines = false;
            agents.show_global_pipelines = true;
        });
        cx.notify();
    }

    /// `Add`, `Remove from this project`, `Make global`, `Detach`; a refusal
    /// goes to the header with the host's words.
    pub(crate) fn change_pipeline_scope(
        &mut self,
        pipeline_id: String,
        action: crate::agents::PipelineScopeAction,
        cx: &mut Context<Self>,
    ) {
        use crate::agents::PipelineScopeAction;
        match action {
            PipelineScopeAction::Remove => self.agents.update(cx, |agents, _| {
                if agents.selected_pipeline.as_ref() == Some(&pipeline_id) {
                    agents.selected_pipeline = None;
                    agents.selected_pipeline_run = None;
                }
            }),
            PipelineScopeAction::Add => self.agents.update(cx, |agents, _| {
                agents.show_global_pipelines = false;
                agents.selected_pipeline = Some(pipeline_id.clone());
            }),
            _ => {}
        }
        self.pipeline_command(
            move |backend, project| {
                Box::pin(async move {
                    let errors = backend
                        .change_pipeline_scope(project, pipeline_id, action)
                        .await?;
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(crate::errors::UiCommandError::from_chat_parts(
                            "pipeline_refused",
                            errors
                                .into_iter()
                                .map(|error| error.message)
                                .collect::<Vec<_>>()
                                .join(" "),
                        ))
                    }
                })
            },
            cx,
        );
    }

    /// `Change` on `Trigger here`: opens or closes the choices.
    pub(crate) fn toggle_pipeline_trigger_choices(
        &mut self,
        pipeline_id: String,
        cx: &mut Context<Self>,
    ) {
        self.agents.update(cx, |agents, _| {
            agents.editing_pipeline_trigger =
                if agents.editing_pipeline_trigger.as_ref() == Some(&pipeline_id) {
                    None
                } else {
                    Some(pipeline_id)
                };
        });
        cx.notify();
    }

    /// This project's own trigger for a global pipeline (R4b), saved at once;
    /// `None` goes back to the pipeline's.
    pub(crate) fn set_pipeline_trigger(
        &mut self,
        pipeline_id: String,
        trigger: Option<PipelineTrigger>,
        cx: &mut Context<Self>,
    ) {
        self.pipeline_command(
            move |backend, project| {
                Box::pin(async move {
                    let errors = backend
                        .set_pipeline_trigger(project, pipeline_id, trigger)
                        .await?;
                    if errors.is_empty() {
                        Ok(())
                    } else {
                        Err(crate::errors::UiCommandError::from_chat_parts(
                            "pipeline_refused",
                            errors
                                .into_iter()
                                .map(|error| error.message)
                                .collect::<Vec<_>>()
                                .join(" "),
                        ))
                    }
                })
            },
            cx,
        );
    }

    /// While a pipeline run is queued or running, the snapshot is read again
    /// every two seconds: the lane records a step after its CLI's last event,
    /// and the queue emits none. A run at a gate waits for the developer,
    /// whose decision refreshes the page.
    pub(crate) fn follow_pipeline_runs(&mut self, cx: &mut Context<Self>) {
        let moving = self.agents.read(cx).pipelines.runs.iter().any(|run| {
            matches!(
                run.status,
                ikore_core::custom_pipelines::PipelineRunStatus::Queued
                    | ikore_core::custom_pipelines::PipelineRunStatus::Running
            )
        });
        if !moving {
            self.pipelines_poll = None;
            return;
        }
        if self.pipelines_poll.is_some() {
            return;
        }
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_secs(2));
        self.pipelines_poll = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |model, cx| {
                model.pipelines_poll = None;
                model.refresh_agents_quiet(cx);
            });
        }));
    }

    /// Settings › Composition: steps and verifiers at once, in the whole app.
    pub(crate) fn set_automation_max_concurrent(&mut self, value: u32, cx: &mut Context<Self>) {
        let Some(backend) = self.agents_backend.clone() else {
            return;
        };
        self.pipelines_task = Some(cx.spawn(async move |this, cx| {
            let result = backend.set_automation_max_concurrent(value).await;
            let _ = this.update(cx, |model, cx| {
                if let Err(error) = result {
                    model.push_header_notice(HeaderNoticeKind::Error, error.presentable(), cx);
                }
                model.refresh_agents(cx);
                cx.notify();
            });
        }));
    }
}
